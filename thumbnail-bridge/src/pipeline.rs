//! End-to-end decomposition of one thumbnail: canvas fit → OCR → per-line style → text removal
//! → person matte → decorations → background repair → font fitting.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::decor::{Plate, find_plates};
use crate::fontmatch::{FontFit, fit_line, japanese_faces};
use crate::image::{IRect, Mask, Rgba8, dilate};
use crate::ocr::{Ocr, OcrLine};
use crate::segment::PersonSegmenter;
use crate::style::{LineStyle, analyze};
use crate::{Error, Result};

/// YouTube's recommended thumbnail size.
pub const YOUTUBE_W: usize = 1280;
pub const YOUTUBE_H: usize = 720;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Canvas {
    /// 1280×720: scale to cover, crop the spare edge where it cuts the least text.
    #[default]
    Youtube,
    /// Keep the image's own size.
    Original,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Options {
    pub canvas: Canvas,
    /// Cut out people (needs the segmentation model).
    pub person: bool,
    /// Separate plates/bubbles behind text.
    pub decorations: bool,
    /// Font candidates kept per text layer.
    pub font_candidates: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { canvas: Canvas::Youtube, person: true, decorations: true, font_candidates: 5 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    Prepare,
    Ocr,
    TextStyle,
    TextRemoval,
    Person,
    Decorations,
    Background,
    Fonts,
    Done,
}

impl Stage {
    pub fn label_ja(&self) -> &'static str {
        match self {
            Stage::Prepare => "画像を準備中",
            Stage::Ocr => "文字を検出中 (OCR)",
            Stage::TextStyle => "文字の色・縁取り・影を解析中",
            Stage::TextRemoval => "背景から文字を消去中",
            Stage::Person => "人物を切り抜き中",
            Stage::Decorations => "装飾（帯・吹き出し）を分離中",
            Stage::Background => "背景を修復中",
            Stage::Fonts => "フォントを推定中",
            Stage::Done => "完了",
        }
    }
}

/// One text line's analysis.
#[derive(Clone, Debug)]
pub struct LineResult {
    pub ocr: OcrLine,
    pub style: Option<LineStyle>,
    /// Best first.
    pub fits: Vec<FontFit>,
}

pub struct Decomposition {
    /// The analysed image (after the canvas fit).
    pub source: Rgba8,
    /// Source with the text removed.
    pub clean: Rgba8,
    /// Background with text, people and plates removed.
    pub background: Rgba8,
    pub person: Option<Mask>,
    pub plates: Vec<Plate>,
    pub lines: Vec<LineResult>,
    pub notes: Vec<String>,
    pub timings: Vec<(Stage, f32)>,
}

/// Loaded models, reusable across images (loading takes a second or two).
pub struct Engines {
    pub ocr: Ocr,
    pub person: Option<PersonSegmenter>,
    pub person_error: Option<String>,
}

impl Engines {
    pub fn load() -> Result<Engines> {
        let ocr = Ocr::load()?;
        let (person, person_error) = match PersonSegmenter::load() {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e.to_string())),
        };
        Ok(Engines { ocr, person, person_error })
    }
}

/// Fit an image to the canvas. For the YouTube canvas the crop window is chosen so that the
/// detected text loses as little as possible.
pub fn fit_canvas(img: &Rgba8, canvas: Canvas, ocr: Option<&Ocr>) -> Result<Rgba8> {
    if canvas == Canvas::Original || (img.w == YOUTUBE_W && img.h == YOUTUBE_H) {
        return Ok(img.clone());
    }
    let (tw, th) = (YOUTUBE_W as f32, YOUTUBE_H as f32);
    let s = (tw / img.w as f32).max(th / img.h as f32);
    // Window in source pixels.
    let (ww, wh) = (tw / s, th / s);
    let (free_x, free_y) = ((img.w as f32 - ww).max(0.0), (img.h as f32 - wh).max(0.0));
    let mut best = (f32::NEG_INFINITY, free_x / 2.0, free_y / 2.0);
    let boxes: Vec<IRect> = match ocr {
        Some(o) if free_x > 1.0 || free_y > 1.0 => o.detect(img)?.iter().map(|r| r.bbox()).collect(),
        _ => Vec::new(),
    };
    let steps = 32;
    for k in 0..=steps {
        let t = k as f32 / steps as f32;
        let (ox, oy) = (free_x * t, free_y * t);
        let win = IRect { x0: ox as i64, y0: oy as i64, x1: (ox + ww) as i64, y1: (oy + wh) as i64 };
        let kept: f32 = boxes.iter().map(|b| b.intersect(&win).area() as f32 - b.area() as f32).sum();
        // Ties: prefer the centre.
        let score = kept - (t - 0.5).abs();
        if score > best.0 {
            best = (score, ox, oy);
        }
    }
    let (ox, oy) = (best.1, best.2);
    let mut out = Rgba8::new(YOUTUBE_W, YOUTUBE_H, [0, 0, 0, 255]);
    let scale_down = s < 1.0;
    out.px.par_chunks_mut(YOUTUBE_W).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let fx = ox + (x as f32 + 0.5) / s;
            let fy = oy + (y as f32 + 0.5) / s;
            let v = if scale_down {
                // Supersample the source footprint.
                let n = 3;
                let mut acc = [0.0f32; 4];
                for j in 0..n {
                    for i in 0..n {
                        let q = img.sample(fx + ((i as f32 + 0.5) / n as f32 - 0.5) / s, fy + ((j as f32 + 0.5) / n as f32 - 0.5) / s);
                        for c in 0..4 {
                            acc[c] += q[c];
                        }
                    }
                }
                acc.map(|a| a / (n * n) as f32)
            } else {
                img.sample(fx, fy)
            };
            *p = v.map(|c| c.round().clamp(0.0, 255.0) as u8);
        }
    });
    Ok(out)
}

/// Decompose one image. `progress` gets each stage as it starts; `cancel` aborts between stages.
pub fn decompose(engines: &Engines, input: &Rgba8, opts: &Options, progress: &(dyn Fn(Stage) + Sync), cancel: &AtomicBool) -> Result<Decomposition> {
    let check = || if cancel.load(Ordering::Relaxed) { Err(Error::Cancelled) } else { Ok(()) };
    let mut timings = Vec::new();
    let mut notes = Vec::new();
    let mut t = Instant::now();
    let mut lap = |stage: Stage, timings: &mut Vec<(Stage, f32)>| {
        timings.push((stage, t.elapsed().as_secs_f32()));
        t = Instant::now();
    };

    progress(Stage::Prepare);
    if input.w < 16 || input.h < 16 || input.w > 8192 || input.h > 8192 {
        return Err(Error::Input(format!("image size {}×{} is outside 16–8192 px", input.w, input.h)));
    }
    // Flatten any transparency onto white (AI images are opaque; this keeps the analysis sane).
    let mut src = input.clone();
    for p in &mut src.px {
        if p[3] < 255 {
            let a = p[3] as f32 / 255.0;
            for c in 0..3 {
                p[c] = (p[c] as f32 * a + 255.0 * (1.0 - a)).round() as u8;
            }
            p[3] = 255;
        }
    }
    let source = fit_canvas(&src, opts.canvas, Some(&engines.ocr))?;
    lap(Stage::Prepare, &mut timings);
    check()?;

    progress(Stage::Ocr);
    let ocr_lines = engines.ocr.read(&source)?;
    lap(Stage::Ocr, &mut timings);
    check()?;

    progress(Stage::TextStyle);
    let styles: Vec<Option<LineStyle>> = ocr_lines.par_iter().map(|l| analyze(&source, l)).collect();
    for (l, s) in ocr_lines.iter().zip(&styles) {
        if s.is_none() {
            notes.push(format!("「{}」: 文字色を背景から分離できませんでした（元画像のまま残ります）", l.text));
        }
    }
    lap(Stage::TextStyle, &mut timings);
    check()?;

    progress(Stage::TextRemoval);
    let (w, h) = (source.w, source.h);
    let mut text_hole = vec![false; w * h];
    for st in styles.iter().flatten() {
        paste_mask(&mut text_hole, w, h, &st.hole, st.crop);
    }
    let clean = crate::inpaint::fill(&source, &text_hole, None);
    lap(Stage::TextRemoval, &mut timings);
    check()?;

    progress(Stage::Person);
    let mut person = None;
    if opts.person {
        match &engines.person {
            Some(seg) => {
                let m = seg.matte(&clean)?;
                let area = m.v.iter().filter(|a| **a > 0.5).count() as f32 / (w * h) as f32;
                if area > 0.015 {
                    person = Some(m);
                } else {
                    notes.push("人物は検出されませんでした".into());
                }
            }
            None => notes.push(format!(
                "人物切り抜きモデルがありません（{}）。人物は背景に含まれます",
                engines.person_error.as_deref().unwrap_or("未読み込み")
            )),
        }
    }
    lap(Stage::Person, &mut timings);
    check()?;

    progress(Stage::Decorations);
    let mut plates = Vec::new();
    if opts.decorations {
        let ls: Vec<(usize, &LineStyle)> = styles.iter().enumerate().filter_map(|(i, s)| s.as_ref().map(|s| (i, s))).collect();
        plates = find_plates(&clean, &ls);
    }
    // People are drawn below plates: keep the plate pixels out of the person layer.
    if let Some(p) = &mut person {
        for pl in &plates {
            for (a, b) in p.v.iter_mut().zip(&pl.mask.v) {
                *a = (*a * (1.0 - b)).max(0.0);
            }
        }
    }
    lap(Stage::Decorations, &mut timings);
    check()?;

    progress(Stage::Background);
    let mut bg_hole = vec![false; w * h];
    if let Some(p) = &person {
        let hard: Vec<bool> = p.v.iter().map(|a| *a > 0.05).collect();
        let grown = dilate(w, h, &hard, 4.0);
        for (a, b) in bg_hole.iter_mut().zip(grown) {
            *a |= b;
        }
    }
    for pl in &plates {
        let hard: Vec<bool> = pl.mask.v.iter().map(|a| *a > 0.02).collect();
        let grown = dilate(w, h, &hard, 2.0);
        for (a, b) in bg_hole.iter_mut().zip(grown) {
            *a |= b;
        }
    }
    let background = if bg_hole.iter().any(|b| *b) { crate::inpaint::fill(&clean, &bg_hole, None) } else { clean.clone() };
    lap(Stage::Background, &mut timings);
    check()?;

    progress(Stage::Fonts);
    let faces = japanese_faces();
    if faces.is_empty() {
        notes.push("日本語フォントが見つかりません。標準フォントで文字レイヤーを作成します".into());
    }
    let mut lines = Vec::new();
    for (ocr, style) in ocr_lines.into_iter().zip(styles) {
        check()?;
        let fits = match &style {
            Some(st) if !faces.is_empty() => fit_line(&ocr, st, &faces, opts.font_candidates.max(1)),
            _ => Vec::new(),
        };
        lines.push(LineResult { ocr, style, fits });
    }
    lap(Stage::Fonts, &mut timings);
    if let Some(dir) = std::env::var_os("PLOTMILL_DEBUG_DIR") {
        debug_dump(std::path::Path::new(&dir), &[("clean", &clean), ("background", &background)], &text_hole, w, h);
    }
    progress(Stage::Done);
    Ok(Decomposition { source, clean, background, person, plates, lines, notes, timings })
}

fn paste_mask(dst: &mut [bool], w: usize, h: usize, src: &[bool], at: IRect) {
    let cw = at.w() as usize;
    for (i, &v) in src.iter().enumerate() {
        if !v {
            continue;
        }
        let (x, y) = (at.x0 + (i % cw) as i64, at.y0 + (i / cw) as i64);
        if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
            dst[y as usize * w + x as usize] = true;
        }
    }
}

/// Development aid: write intermediate images as PPM/PGM (no encoder dependency).
fn debug_dump(dir: &std::path::Path, imgs: &[(&str, &Rgba8)], hole: &[bool], w: usize, h: usize) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(dir);
    for (name, img) in imgs {
        if let Ok(mut f) = std::fs::File::create(dir.join(format!("{name}.ppm"))) {
            let _ = write!(f, "P6\n{} {}\n255\n", img.w, img.h);
            let bytes: Vec<u8> = img.px.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
            let _ = f.write_all(&bytes);
        }
    }
    if let Ok(mut f) = std::fs::File::create(dir.join("text_hole.pgm")) {
        let _ = write!(f, "P5\n{w} {h}\n255\n");
        let _ = f.write_all(&hole.iter().map(|b| if *b { 255u8 } else { 0 }).collect::<Vec<_>>());
    }
}
