//! Japanese OCR with PaddleOCR PP-OCRv6 (ONNX): DBNet text-line detection and CTC recognition.
//!
//! Pre/post-processing follows PaddleOCR's published pipeline (and RapidOCR's defaults): BGR input
//! normalised to [-1, 1], detection side limit 736 (`min`), binarisation 0.3, box score 0.5,
//! unclip ratio 1.6; recognition height 48, greedy CTC decoding. Recognition also reports where
//! each character sits along the line (from the CTC time steps), which the style analysis uses
//! for per-character colours.

use serde::{Deserialize, Serialize};

use crate::image::{Rgba8, components};
use crate::models::{Model, OCR_DET, OCR_REC};
use crate::{Error, Result};

/// A rotated rectangle: centre, size along/across the text direction, angle in radians.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RotRect {
    pub cx: f32,
    pub cy: f32,
    /// Extent along `angle`.
    pub w: f32,
    /// Extent across `angle`.
    pub h: f32,
    pub angle: f32,
}

impl RotRect {
    /// Corners: top-left, top-right, bottom-right, bottom-left (in the rect's own frame).
    pub fn corners(&self) -> [[f32; 2]; 4] {
        let (c, s) = (self.angle.cos(), self.angle.sin());
        let (hw, hh) = (self.w / 2.0, self.h / 2.0);
        let p = |u: f32, v: f32| [self.cx + u * c - v * s, self.cy + u * s + v * c];
        [p(-hw, -hh), p(hw, -hh), p(hw, hh), p(-hw, hh)]
    }
    pub fn bbox(&self) -> crate::image::IRect {
        let cs = self.corners();
        let x0 = cs.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
        let y0 = cs.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let x1 = cs.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        let y1 = cs.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
        crate::image::IRect { x0: x0.floor() as i64, y0: y0.floor() as i64, x1: x1.ceil() as i64, y1: y1.ceil() as i64 }
    }
    /// Map a point given in the rect frame (u along, v across, origin at top-left corner).
    pub fn to_image(&self, u: f32, v: f32) -> [f32; 2] {
        let (c, s) = (self.angle.cos(), self.angle.sin());
        let (u, v) = (u - self.w / 2.0, v - self.h / 2.0);
        [self.cx + u * c - v * s, self.cy + u * s + v * c]
    }
}

/// One recognised text line.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrLine {
    pub text: String,
    pub score: f32,
    pub rect: RotRect,
    /// True when the line was read top-to-bottom (tall box).
    pub vertical: bool,
    /// Per character: the fraction (0..1) along the line where its CTC peak fell.
    pub char_pos: Vec<f32>,
}

pub struct Ocr {
    det: Model,
    rec: Model,
    dict: Vec<String>,
}

const DET_LIMIT: f32 = 736.0;
const DET_MAX_SIDE: f32 = 2000.0;
const DET_THRESH: f32 = 0.3;
const BOX_THRESH: f32 = 0.5;
const UNCLIP: f32 = 1.6;
const REC_H: usize = 48;
const REC_MIN_W: usize = 320;

impl Ocr {
    pub fn load() -> Result<Ocr> {
        let det = Model::load(&OCR_DET)?;
        let rec = Model::load(&OCR_REC)?;
        let chars = rec.metadata("character").ok_or_else(|| Error::Model("recognition model has no character list".into()))?;
        // Index 0 is the CTC blank; the list follows; a space closes it.
        let mut dict = vec![String::new()];
        dict.extend(chars.split('\n').map(|s| s.trim_end_matches('\r').to_string()));
        dict.push(" ".into());
        Ok(Ocr { det, rec, dict })
    }

    /// Detect and read every text line.
    pub fn read(&self, img: &Rgba8) -> Result<Vec<OcrLine>> {
        let rects = self.detect(img)?;
        let mut lines = Vec::new();
        for r in rects {
            let vertical = r.h > r.w * 1.5;
            let (text, score, char_pos) = self.recognize(img, &r, vertical)?;
            let text = text.trim().to_string();
            if text.is_empty() || score < 0.5 {
                continue;
            }
            lines.push(OcrLine { text, score, rect: r, vertical, char_pos });
        }
        Ok(lines)
    }

    /// Text-line boxes (rotated rectangles in image pixels), top to bottom.
    pub fn detect(&self, img: &Rgba8) -> Result<Vec<RotRect>> {
        if img.w < 8 || img.h < 8 {
            return Ok(Vec::new());
        }
        let (w, h) = (img.w as f32, img.h as f32);
        let mut ratio = if w.min(h) < DET_LIMIT { DET_LIMIT / w.min(h) } else { 1.0 };
        if w.max(h) * ratio > DET_MAX_SIDE {
            ratio = DET_MAX_SIDE / w.max(h);
        }
        let rw = (((w * ratio) / 32.0).round() as usize * 32).max(32);
        let rh = (((h * ratio) / 32.0).round() as usize * 32).max(32);
        let rgb = img.resize_rgb(rw, rh);
        let mut data = vec![0.0f32; 3 * rw * rh];
        let plane = rw * rh;
        for (i, p) in rgb.iter().enumerate() {
            // BGR planes, (x / 255 - 0.5) / 0.5.
            data[i] = p[2] / 127.5 - 1.0;
            data[plane + i] = p[1] / 127.5 - 1.0;
            data[2 * plane + i] = p[0] / 127.5 - 1.0;
        }
        let (shape, prob) = self.det.run([1, 3, rh, rw], data)?;
        let (ph, pw) = match shape.as_slice() {
            [_, _, ph, pw] => (*ph, *pw),
            _ => return Err(Error::Model(format!("unexpected detection output shape {shape:?}"))),
        };
        if prob.len() < ph * pw {
            return Err(Error::Model("detection output too short".into()));
        }
        let rects = boxes_from_prob(&prob[..ph * pw], pw, ph);
        let sx = w / pw as f32;
        let sy = h / ph as f32;
        let mut out: Vec<RotRect> = rects
            .into_iter()
            .map(|r| scale_rect(&r, sx, sy))
            .filter(|r| r.w > 3.0 && r.h > 3.0)
            .collect();
        // Reading order: top to bottom, then left to right within ~a line.
        out.sort_by(|a, b| {
            let ay = a.cy - a.h / 2.0;
            let by = b.cy - b.h / 2.0;
            if (ay - by).abs() < 10.0 { a.cx.total_cmp(&b.cx) } else { ay.total_cmp(&by) }
        });
        Ok(out)
    }

    /// Recognise one line. Returns text, mean confidence and character positions (0..1 along).
    pub fn recognize(&self, img: &Rgba8, r: &RotRect, vertical: bool) -> Result<(String, f32, Vec<f32>)> {
        // Crop the rotated rect to an upright strip; vertical lines are turned 90° anticlockwise
        // (as PaddleOCR does) so the text runs left to right.
        let (cw, ch) = (r.w.max(1.0), r.h.max(1.0));
        let (sw, sh) = if vertical { (ch, cw) } else { (cw, ch) };
        let out_h = REC_H;
        let out_w_content = ((out_h as f32 * sw / sh).ceil() as usize).clamp(1, 4096);
        let out_w = out_w_content.max(REC_MIN_W);
        let plane = out_h * out_w;
        let mut data = vec![0.0f32; 3 * plane];
        for y in 0..out_h {
            for x in 0..out_w_content {
                // Strip coordinates → rect frame (u along w, v across h).
                let su = (x as f32 + 0.5) / out_w_content as f32 * sw;
                let sv = (y as f32 + 0.5) / out_h as f32 * sh;
                // np.rot90 (anticlockwise): strip x runs down the column, strip y from its right edge.
                let (u, v) = if vertical { (cw - sv, su) } else { (su, sv) };
                let p = r.to_image(u, v);
                let px = img.sample(p[0], p[1]);
                let i = y * out_w + x;
                data[i] = px[2] / 127.5 - 1.0;
                data[plane + i] = px[1] / 127.5 - 1.0;
                data[2 * plane + i] = px[0] / 127.5 - 1.0;
            }
        }
        let (shape, probs) = self.rec.run([1, 3, out_h, out_w], data)?;
        let (t, c) = match shape.as_slice() {
            [_, t, c] => (*t, *c),
            _ => return Err(Error::Model(format!("unexpected recognition output shape {shape:?}"))),
        };
        if c == 0 || probs.len() < t * c {
            return Err(Error::Model("recognition output too short".into()));
        }
        let mut text = String::new();
        let mut scores = Vec::new();
        let mut pos = Vec::new();
        let mut last = usize::MAX;
        let content_frac = out_w_content as f32 / out_w as f32;
        for step in 0..t {
            let row = &probs[step * c..(step + 1) * c];
            let (mut best, mut bp) = (0usize, f32::NEG_INFINITY);
            for (i, &p) in row.iter().enumerate() {
                if p > bp {
                    bp = p;
                    best = i;
                }
            }
            if best != 0 && best != last {
                if let Some(ch) = self.dict.get(best) {
                    text.push_str(ch);
                    scores.push(bp);
                    let f = ((step as f32 + 0.5) / t as f32 / content_frac).clamp(0.0, 1.0);
                    for _ in ch.chars() {
                        pos.push(f);
                    }
                }
            }
            last = best;
        }
        let score = if scores.is_empty() { 0.0 } else { scores.iter().sum::<f32>() / scores.len() as f32 };
        // Drop positions of leading/trailing spaces trimmed by the caller.
        let lead = text.chars().take_while(|c| c.is_whitespace()).count();
        let trimmed_len = text.trim().chars().count();
        let pos = pos.into_iter().skip(lead).take(trimmed_len).collect();
        Ok((text, score, pos))
    }
}

fn scale_rect(r: &RotRect, sx: f32, sy: f32) -> RotRect {
    let cs = r.corners();
    let pts: Vec<[f32; 2]> = cs.iter().map(|p| [p[0] * sx, p[1] * sy]).collect();
    min_area_rect(&pts).unwrap_or(*r)
}

/// DBNet post-processing: components of the dilated binary map → min-area rectangles → score →
/// unclip → rectangles in map pixels.
fn boxes_from_prob(prob: &[f32], w: usize, h: usize) -> Vec<RotRect> {
    let bin: Vec<bool> = prob.iter().map(|&p| p > DET_THRESH).collect();
    // 2×2 dilation (PaddleOCR `use_dilation`).
    let mut dil = bin.clone();
    for y in 0..h {
        for x in 0..w {
            if bin[y * w + x] {
                continue;
            }
            let l = x > 0 && bin[y * w + x - 1];
            let u = y > 0 && bin[(y - 1) * w + x];
            let lu = x > 0 && y > 0 && bin[(y - 1) * w + x - 1];
            if l || u || lu {
                dil[y * w + x] = true;
            }
        }
    }
    let (lab, n) = components(w, h, &dil);
    // Per component: per-row extreme points (enough for the convex hull).
    let mut rows: Vec<Vec<(i64, i64, i64)>> = vec![Vec::new(); n as usize + 1];
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let l = lab[y * w + x];
            if l == 0 {
                x += 1;
                continue;
            }
            let x0 = x;
            while x < w && lab[y * w + x] == l {
                x += 1;
            }
            if let Some(v) = rows.get_mut(l as usize) {
                v.push((y as i64, x0 as i64, x as i64));
            }
        }
    }
    let mut out = Vec::new();
    for spans in rows.iter().skip(1) {
        if spans.is_empty() || spans.len() > 100_000 {
            continue;
        }
        let mut pts: Vec<[f32; 2]> = Vec::with_capacity(spans.len() * 4);
        for &(y, x0, x1) in spans {
            pts.push([x0 as f32, y as f32]);
            pts.push([x1 as f32, y as f32]);
            pts.push([x0 as f32, y as f32 + 1.0]);
            pts.push([x1 as f32, y as f32 + 1.0]);
        }
        let Some(r) = min_area_rect(&pts) else { continue };
        if r.w.min(r.h) < 3.0 {
            continue;
        }
        // Score: mean probability inside the rectangle (PaddleOCR "fast" mode uses the polygon).
        let score = rect_mean(prob, w, h, &r);
        if score < BOX_THRESH {
            continue;
        }
        let d = r.w * r.h * UNCLIP / (2.0 * (r.w + r.h));
        let mut u = RotRect { w: r.w + 2.0 * d, h: r.h + 2.0 * d, ..r };
        if u.w.min(u.h) < 5.0 {
            continue;
        }
        // Normalise so that `w` runs along the text for horizontal lines.
        if u.angle.abs() > std::f32::consts::FRAC_PI_4 {
            u = RotRect { w: u.h, h: u.w, angle: u.angle - std::f32::consts::FRAC_PI_2 * u.angle.signum(), ..u };
        }
        out.push(u);
    }
    out
}

fn rect_mean(prob: &[f32], w: usize, h: usize, r: &RotRect) -> f32 {
    let bb = r.bbox().clamp_to(w, h);
    let (c, s) = (r.angle.cos(), r.angle.sin());
    let (mut sum, mut n) = (0.0f32, 0usize);
    for y in bb.y0..bb.y1 {
        for x in bb.x0..bb.x1 {
            let (dx, dy) = (x as f32 + 0.5 - r.cx, y as f32 + 0.5 - r.cy);
            let u = dx * c + dy * s;
            let v = -dx * s + dy * c;
            if u.abs() <= r.w / 2.0 && v.abs() <= r.h / 2.0 {
                sum += prob.get(y as usize * w + x as usize).copied().unwrap_or(0.0);
                n += 1;
            }
        }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
}

/// Minimum-area enclosing rectangle (rotating calipers over the convex hull).
pub fn min_area_rect(pts: &[[f32; 2]]) -> Option<RotRect> {
    let hull = convex_hull(pts);
    if hull.len() < 3 {
        let (x0, y0, x1, y1) = pts.iter().fold((f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY), |a, p| {
            (a.0.min(p[0]), a.1.min(p[1]), a.2.max(p[0]), a.3.max(p[1]))
        });
        if !x0.is_finite() {
            return None;
        }
        return Some(RotRect { cx: (x0 + x1) / 2.0, cy: (y0 + y1) / 2.0, w: (x1 - x0).max(1.0), h: (y1 - y0).max(1.0), angle: 0.0 });
    }
    let mut best: Option<(f32, RotRect)> = None;
    for i in 0..hull.len() {
        let a = hull[i];
        let b = hull[(i + 1) % hull.len()];
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let len = (ex * ex + ey * ey).sqrt();
        if len < 1e-6 {
            continue;
        }
        let (ux, uy) = (ex / len, ey / len);
        let (mut umin, mut umax, mut vmin, mut vmax) = (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
        for p in &hull {
            let u = p[0] * ux + p[1] * uy;
            let v = -p[0] * uy + p[1] * ux;
            umin = umin.min(u);
            umax = umax.max(u);
            vmin = vmin.min(v);
            vmax = vmax.max(v);
        }
        let area = (umax - umin) * (vmax - vmin);
        if best.as_ref().is_none_or(|(ba, _)| area < *ba) {
            let (cu, cv) = ((umin + umax) / 2.0, (vmin + vmax) / 2.0);
            let cx = cu * ux - cv * uy;
            let cy = cu * uy + cv * ux;
            let mut angle = uy.atan2(ux);
            let (mut rw, mut rh) = (umax - umin, vmax - vmin);
            // Keep the angle in (-90°, 90°].
            while angle > std::f32::consts::FRAC_PI_2 {
                angle -= std::f32::consts::PI;
            }
            while angle <= -std::f32::consts::FRAC_PI_2 {
                angle += std::f32::consts::PI;
            }
            // Prefer the representation closest to axis-aligned.
            if angle.abs() > std::f32::consts::FRAC_PI_4 {
                std::mem::swap(&mut rw, &mut rh);
                angle -= std::f32::consts::FRAC_PI_2 * angle.signum();
            }
            best = Some((area, RotRect { cx, cy, w: rw, h: rh, angle }));
        }
    }
    best.map(|b| b.1)
}

fn convex_hull(pts: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut p: Vec<[f32; 2]> = pts.iter().copied().filter(|q| q[0].is_finite() && q[1].is_finite()).collect();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut lower: Vec<[f32; 2]> = Vec::new();
    for &q in &p {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], q) <= 0.0 {
            lower.pop();
        }
        lower.push(q);
    }
    let mut upper: Vec<[f32; 2]> = Vec::new();
    for &q in p.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], q) <= 0.0 {
            upper.pop();
        }
        upper.push(q);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_area_rect_of_axis_box_and_rotated_box() {
        let pts = [[10.0, 20.0], [110.0, 20.0], [110.0, 50.0], [10.0, 50.0]];
        let r = min_area_rect(&pts).unwrap();
        assert!((r.w - 100.0).abs() < 1e-3 && (r.h - 30.0).abs() < 1e-3 && r.angle.abs() < 1e-3, "{r:?}");
        assert!((r.cx - 60.0).abs() < 1e-3 && (r.cy - 35.0).abs() < 1e-3);
        let a: f32 = 0.2;
        let rot = RotRect { cx: 50.0, cy: 40.0, w: 80.0, h: 20.0, angle: a };
        let r2 = min_area_rect(&rot.corners()).unwrap();
        assert!((r2.w - 80.0).abs() < 1e-2 && (r2.h - 20.0).abs() < 1e-2 && (r2.angle - a).abs() < 1e-3, "{r2:?}");
    }

    #[test]
    fn prob_map_boxes() {
        let (w, h) = (64, 32);
        let mut prob = vec![0.0f32; w * h];
        for y in 10..18 {
            for x in 8..50 {
                prob[y * w + x] = 0.9;
            }
        }
        let b = boxes_from_prob(&prob, w, h);
        assert_eq!(b.len(), 1);
        assert!(b[0].w > 42.0 && b[0].h > 8.0, "{:?}", b[0]);
        assert!(boxes_from_prob(&vec![0.0; w * h], w, h).is_empty());
    }
}
