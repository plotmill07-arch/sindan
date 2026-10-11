//! Font estimation: render the recognised text with every Japanese-capable face PhotoCraft can
//! use (its own type engine, so the match is what the editable layer will look like), fit size
//! and tracking to the glyph fill found in the image, and rank faces by shape overlap.
//!
//! The same renderer also settles OCR confusions that only differ in shape/size: small vs large
//! kana (っ/つ, ゃ/や, …) and half- vs full-width punctuation and digits, and adds a trailing
//! "！"/"？" the recogniser dropped when the ink clearly extends past the rendered text.

use std::sync::OnceLock;

use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::TextLayer;
use photocraft_doc::text::{CharStyle, Orientation, TextRun};
use photocraft_geom::Rect;
use serde::{Deserialize, Serialize};

use crate::image::box_blur;
use crate::ocr::OcrLine;
use crate::style::LineStyle;

/// A face that can be offered for a text layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Face {
    pub family: String,
    pub style: String,
    pub weight: u16,
}

/// A fitted face, best first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontFit {
    pub face: Face,
    pub text: String,
    pub size_px: f32,
    /// Letter spacing in 1/1000 em.
    pub tracking: f32,
    /// Shape similarity with the image (0..1).
    pub score: f32,
    /// Baseline origin of the first line, image pixels.
    pub anchor: [f32; 2],
    /// Rotation in radians (0 for level text).
    pub angle: f32,
    pub vertical: bool,
}

/// Japanese-capable faces known to PhotoCraft's type engine (cached per process; call
/// [`refresh_faces`] after registering fonts).
pub fn japanese_faces() -> Vec<Face> {
    FACES.get_or_init(scan_faces).clone()
}

static FACES: OnceLock<Vec<Face>> = OnceLock::new();

/// Register extra font files (e.g. a user font folder) with the shared type engine.
pub fn register_font_files(paths: &[std::path::PathBuf]) -> Vec<String> {
    let mut added = Vec::new();
    let mut eng = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
    for p in paths {
        if let Ok(bytes) = std::fs::read(p) {
            added.extend(eng.fonts.register_font_data(bytes));
        }
    }
    added
}

fn scan_faces() -> Vec<Face> {
    let mut eng = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
    let families = eng.fonts.families();
    let mut out = Vec::new();
    for fam in families {
        // Bitmap-style fallback fonts never match designed type.
        if fam.contains("Unifont") || fam.ends_with(" Sample") {
            continue;
        }
        // Single-face layout of mixed Latin + kana + kanji = the family covers Japanese itself.
        let probe = TextLayer { text: "Aあア漢".into(), runs: vec![TextRun { len: "Aあア漢".len(), style: CharStyle { font_family: fam.clone(), size_pt: 20.0, ..Default::default() } }], ..Default::default() };
        let l = eng.layout(&probe, 72.0);
        // One face entry per glyph run: all must be the same font file and index.
        let mut ids = l.faces.iter().map(|f| (f.font.data.id(), f.font.index));
        let first = ids.next();
        if l.glyphs.len() < 4 || first.is_none() || ids.any(|i| Some(i) != first) || l.glyphs.iter().any(|g| g.id == 0) {
            continue;
        }
        let mut faces = eng.fonts.faces(&fam);
        faces.retain(|f| !f.italic);
        let mut weights: Vec<(u16, String)> = faces.iter().map(|f| (f.weight.round().clamp(100.0, 900.0) as u16, f.style.clone())).collect();
        weights.sort_by_key(|w| w.0);
        weights.dedup_by_key(|w| w.0);
        // At most four weights per family: the lightest, regular-ish, bold-ish and heaviest.
        let pick: Vec<(u16, String)> = if weights.len() <= 4 {
            weights
        } else {
            let near = |t: u16| weights.iter().min_by_key(|w| (w.0 as i32 - t as i32).abs()).cloned();
            let mut v: Vec<(u16, String)> = [weights.first().cloned(), near(400), near(700), weights.last().cloned()].into_iter().flatten().collect();
            v.dedup_by_key(|w| w.0);
            v
        };
        for (weight, style) in pick {
            out.push(Face { family: fam.clone(), style, weight });
        }
    }
    out
}

/// A binary ink mask with its bounding box (in its own pixel frame).
#[derive(Clone, Debug)]
struct InkMask {
    w: usize,
    h: usize,
    v: Vec<bool>,
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl InkMask {
    fn from(w: usize, h: usize, v: Vec<bool>) -> Option<Self> {
        let b = crate::image::bbox_of(w, h, |i| v.get(i).copied().unwrap_or(false));
        if b.is_empty() {
            return None;
        }
        Some(InkMask { w, h, v, x0: b.x0, y0: b.y0, x1: b.x1, y1: b.y1 })
    }
    fn bw(&self) -> f32 {
        (self.x1 - self.x0) as f32
    }
    fn bh(&self) -> f32 {
        (self.y1 - self.y0) as f32
    }
    fn at(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h && self.v[y as usize * self.w + x as usize]
    }
}

/// The line's glyph fill resampled into the line frame (deskewed), plus the frame origin.
struct Target {
    mask: InkMask,
    /// Line-frame coordinates of mask pixel (0,0): u, v relative to the rect's top-left corner.
    origin: (f32, f32),
}

fn target(line: &OcrLine, st: &LineStyle) -> Option<Target> {
    let r = &line.rect;
    let cw = st.cw();
    let crop = st.crop;
    let sample = |x: f32, y: f32| {
        let (ix, iy) = (x.floor() as i64 - crop.x0, y.floor() as i64 - crop.y0);
        ix >= 0 && iy >= 0 && ix < crop.w() && iy < crop.h() && st.fill_mask.get(iy as usize * cw + ix as usize).copied().unwrap_or(false)
    };
    if line.vertical || r.angle.abs() < LEVEL {
        // Level text: use the crop as is; the frame is image space.
        let v = st.fill_mask.clone();
        let mask = InkMask::from(cw, st.ch(), v)?;
        return Some(Target { mask, origin: (crop.x0 as f32, crop.y0 as f32) });
    }
    let m = 0.45 * r.h;
    let (w, h) = ((r.w + 2.0 * m).ceil() as usize, (r.h + 2.0 * m).ceil() as usize);
    let mut v = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let p = r.to_image(x as f32 + 0.5 - m, y as f32 + 0.5 - m);
            v[y * w + x] = sample(p[0], p[1]);
        }
    }
    Some(Target { mask: InkMask::from(w, h, v)?, origin: (-m, -m) })
}

/// Rendered ink of `text` in `face` at `size_px` / `tracking` (anchor at the text origin).
fn render(text: &str, face: &Face, size_px: f32, tracking: f32, vertical: bool) -> Option<(InkMask, Rect)> {
    let style = CharStyle {
        font_family: face.family.clone(),
        font_style: face.style.clone(),
        weight: face.weight,
        size_pt: size_px,
        tracking,
        color: Color::WHITE,
        ..Default::default()
    };
    let layer = TextLayer {
        text: text.to_string(),
        runs: vec![TextRun { len: text.len(), style }],
        orientation: if vertical { Orientation::Vertical } else { Orientation::Horizontal },
        ..Default::default()
    };
    let fmt = PixelFormat::new(ColorMode::Rgb, SampleType::U8, true);
    let rendered = {
        let mut eng = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
        eng.render(&layer, 72.0, fmt).1
    };
    let rect = rendered.rect;
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    if w == 0 || h == 0 || w * h > 40_000_000 {
        return None;
    }
    let mut buf = vec![[0u8; 4]; w * h];
    rendered.surface.read_rgba8_into(rect, &mut buf);
    let v: Vec<bool> = buf.iter().map(|p| p[3] >= 128).collect();
    Some((InkMask::from(w, h, v)?, rect))
}

/// Soft Jaccard similarity of two ink masks after aligning their bounding boxes' top-left.
fn similarity(a: &InkMask, b: &InkMask) -> f32 {
    let pad = 3i64;
    let w = (a.bw().max(b.bw()) as i64 + 2 * pad) as usize;
    let h = (a.bh().max(b.bh()) as i64 + 2 * pad) as usize;
    if w * h > 30_000_000 {
        return 0.0;
    }
    let raster = |m: &InkMask| -> Vec<f32> {
        let mut v = vec![0.0f32; w * h];
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                if m.at(x - pad + m.x0, y - pad + m.y0) {
                    v[y as usize * w + x as usize] = 1.0;
                }
            }
        }
        box_blur(w, h, &v, 1, 2)
    };
    let (ra, rb) = (raster(a), raster(b));
    let (mut num, mut den) = (0.0f32, 0.0f32);
    for (x, y) in ra.iter().zip(&rb) {
        num += x.min(*y);
        den += x.max(*y);
    }
    if den <= 0.0 { 0.0 } else { num / den }
}

struct Fitted {
    size: f32,
    tracking: f32,
    score: f32,
    ink: InkMask,
    rect: Rect,
}

/// Fit size then tracking of one face to the target; returns the fit and its score.
fn fit_face(text: &str, face: &Face, t: &InkMask, vertical: bool) -> Option<Fitted> {
    let n = text.chars().count();
    // Size follows the extent across the line; tracking the extent along it.
    let (t_across, t_along) = if vertical { (t.bw(), t.bh()) } else { (t.bh(), t.bw()) };
    let across = |m: &InkMask| if vertical { m.bw() } else { m.bh() };
    let along = |m: &InkMask| if vertical { m.bh() } else { m.bw() };
    let mut size = (t_across / 0.85).clamp(4.0, 1200.0);
    for _ in 0..2 {
        let (ink, _) = render(text, face, size, 0.0, vertical)?;
        let a = across(&ink);
        if a < 1.0 {
            return None;
        }
        size = (size * t_across / a).clamp(4.0, 1200.0);
    }
    let (ink0, _) = render(text, face, size, 0.0, vertical)?;
    let tracking = if n > 1 { ((t_along - along(&ink0)) / (n as f32 - 1.0) / size * 1000.0).clamp(-150.0, 400.0) } else { 0.0 };
    let (ink, rect) = if tracking.abs() >= 5.0 { render(text, face, size, tracking, vertical)? } else { (ink0, Rect::EMPTY) };
    let rect = if rect == Rect::EMPTY { render(text, face, size, 0.0, vertical)?.1 } else { rect };
    let tracking = if tracking.abs() >= 5.0 { tracking } else { 0.0 };
    let score = similarity(&ink, t);
    Some(Fitted { size, tracking, score, ink, rect })
}

/// Tilts below this (≈3°) are detector noise; thumbnails tilt titles on purpose by more.
const LEVEL: f32 = 0.05;

const SMALL_KANA: &[(char, char)] = &[
    ('つ', 'っ'), ('や', 'ゃ'), ('ゆ', 'ゅ'), ('よ', 'ょ'), ('あ', 'ぁ'), ('い', 'ぃ'), ('う', 'ぅ'), ('え', 'ぇ'), ('お', 'ぉ'), ('わ', 'ゎ'),
    ('ツ', 'ッ'), ('ヤ', 'ャ'), ('ユ', 'ュ'), ('ヨ', 'ョ'), ('ア', 'ァ'), ('イ', 'ィ'), ('ウ', 'ゥ'), ('エ', 'ェ'), ('オ', 'ォ'), ('カ', 'ヵ'),
    ('ケ', 'ヶ'), ('ワ', 'ヮ'),
    ('!', '！'), ('?', '？'), ('(', '（'), (')', '）'), (':', '：'), ('%', '％'), ('&', '＆'), ('+', '＋'), ('#', '＃'),
    ('0', '０'), ('1', '１'), ('2', '２'), ('3', '３'), ('4', '４'), ('5', '５'), ('6', '６'), ('7', '７'), ('8', '８'), ('9', '９'),
];

fn swap_char(c: char) -> Option<char> {
    SMALL_KANA.iter().find_map(|&(a, b)| if c == a { Some(b) } else if c == b { Some(a) } else { None })
}

/// Fit every candidate face to one line. Best first; `max` results.
pub fn fit_line(line: &OcrLine, st: &LineStyle, faces: &[Face], max: usize) -> Vec<FontFit> {
    let Some(tg) = target(line, st) else { return Vec::new() };
    let t = &tg.mask;
    let text = line.text.clone();
    let mut fits: Vec<(Face, Fitted)> = faces.iter().filter_map(|f| fit_face(&text, f, t, line.vertical).map(|x| (f.clone(), x))).collect();
    fits.sort_by(|a, b| b.1.score.total_cmp(&a.1.score));
    let Some(best_face) = fits.first().map(|f| f.0.clone()) else { return Vec::new() };

    // Settle shape-only OCR confusions with the best face.
    let mut text_fixed = text.clone();
    let mut best_score = fits.first().map(|f| f.1.score).unwrap_or(0.0);
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        let Some(alt) = swap_char(*c) else { continue };
        let mut cand: Vec<char> = text_fixed.chars().collect();
        if let Some(slot) = cand.get_mut(i) {
            *slot = alt;
        }
        let cand: String = cand.into_iter().collect();
        if let Some(f) = fit_face(&cand, &best_face, t, line.vertical)
            && f.score > best_score + 0.004
        {
            best_score = f.score;
            text_fixed = cand;
        }
    }
    // A dropped trailing "！"/"？": ink well past the rendered text at its natural spacing.
    if !line.vertical
        && let Some(f) = fit_face(&text_fixed, &best_face, t, false)
        && f.tracking > 60.0
    {
        for tail in ["！", "!", "？", "?", "!?", "！？", "…"] {
            let cand = format!("{text_fixed}{tail}");
            if let Some(g) = fit_face(&cand, &best_face, t, false)
                && g.score > best_score + 0.02
            {
                best_score = g.score;
                text_fixed = cand;
            }
        }
    }
    if text_fixed != text {
        fits = faces.iter().filter_map(|f| fit_face(&text_fixed, f, t, line.vertical).map(|x| (f.clone(), x))).collect();
        fits.sort_by(|a, b| b.1.score.total_cmp(&a.1.score));
    }

    let angle = if line.vertical || line.rect.angle.abs() < LEVEL { 0.0 } else { line.rect.angle };
    fits.into_iter()
        .take(max)
        .map(|(face, f)| {
            // Anchor: target ink top-left minus rendered ink top-left (rendered with the anchor
            // at the origin, so `rect` is relative to it).
            let tx = tg.origin.0 + t.x0 as f32;
            let ty = tg.origin.1 + t.y0 as f32;
            let rx = f.rect.x0 as f32 + f.ink.x0 as f32;
            let ry = f.rect.y0 as f32 + f.ink.y0 as f32;
            let (au, av) = (tx - rx, ty - ry);
            let anchor = if angle == 0.0 {
                [au, av]
            } else {
                // Line frame (relative to the rect's top-left) → image.
                line.rect.to_image(au, av)
            };
            FontFit { face, text: text_fixed.clone(), size_px: f.size, tracking: f.tracking, score: f.score, anchor, angle, vertical: line.vertical }
        })
        .collect()
}
