//! Decorations behind text: banners, speech bubbles, badges. After the text is removed, a plate
//! is a region of nearly one colour around the text, bounded (not the whole backdrop), possibly
//! with an outline of another colour. Each plate becomes its own pixel layer.

use crate::image::{IRect, Mask, Rgba8, dilate, dist2, edt_sq, median_color};
use crate::style::LineStyle;

#[derive(Clone, Debug)]
pub struct Plate {
    pub mask: Mask,
    pub bbox: IRect,
    pub color: [f32; 3],
    pub outline: Option<([f32; 3], f32)>,
    /// Indices of the text lines sitting on it.
    pub lines: Vec<usize>,
    pub kind: PlateKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlateKind {
    Banner,
    Bubble,
    Badge,
}

impl PlateKind {
    pub fn name(&self) -> &'static str {
        match self {
            PlateKind::Banner => "帯",
            PlateKind::Bubble => "吹き出し",
            PlateKind::Badge => "バッジ",
        }
    }
}

/// Find the plate behind each text line in the text-free image `clean`.
pub fn find_plates(clean: &Rgba8, lines: &[(usize, &LineStyle)]) -> Vec<Plate> {
    let (w, h) = (clean.w, clean.h);
    let total = (w * h) as f32;
    let mut plates: Vec<Plate> = Vec::new();
    for &(li, st) in lines {
        let fb = st.fill_bbox;
        if fb.is_empty() {
            continue;
        }
        // Colour right around the ink (now repaired).
        let pad = (st.stroke.as_ref().map_or(0.0, |s| s.width) as i64) + 4;
        let inner = fb.grow(pad).clamp_to(w, h);
        let outer = fb.grow(pad + 10).clamp_to(w, h);
        let ring: Vec<[f32; 3]> = ring_pixels(clean, &inner, &outer);
        if ring.len() < 30 {
            continue;
        }
        let Some(c) = median_color(&ring) else { continue };
        let near = ring.iter().filter(|p| dist2(**p, c).sqrt() < 26.0).count();
        if (near as f32) < 0.85 * ring.len() as f32 {
            continue; // not a flat plate
        }
        if plates.iter_mut().any(|p| {
            let hit = p.mask.at((fb.x0 + fb.x1) / 2, (fb.y0 + fb.y1) / 2) > 0.5 && dist2(p.color, c).sqrt() < 30.0;
            if hit {
                p.lines.push(li);
            }
            hit
        }) {
            continue;
        }
        // Flood fill from the ring.
        let mut m = vec![false; w * h];
        let mut stack: Vec<usize> = Vec::new();
        for y in outer.y0..outer.y1 {
            for x in outer.x0..outer.x1 {
                let i = y as usize * w + x as usize;
                if dist2(clean.rgb_f(x, y), c).sqrt() < 26.0 && !m[i] {
                    m[i] = true;
                    stack.push(i);
                }
            }
        }
        let limit = (total * 0.4) as usize;
        let mut count = stack.len();
        while let Some(i) = stack.pop() {
            if count > limit {
                break;
            }
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                let j = ny as usize * w + nx as usize;
                if !m[j] && dist2(clean.rgb_f(nx, ny), c).sqrt() < 30.0 {
                    m[j] = true;
                    count += 1;
                    stack.push(j);
                }
            }
        }
        if count > limit {
            continue; // the backdrop, not a plate
        }
        let bbox = crate::image::bbox_of(w, h, |i| m[i]);
        let touches = [bbox.x0 <= 0, bbox.y0 <= 0, bbox.x1 >= w as i64, bbox.y1 >= h as i64].iter().filter(|b| **b).count();
        if touches >= 3 && (count as f32) > total * 0.25 || touches == 4 {
            continue;
        }
        // Close small gaps (anti-aliased text residue) and fill enclosed holes.
        let closed = crate::image::erode(w, h, &dilate(w, h, &m, 2.0), 2.0);
        let filled = fill_enclosed(w, h, &closed);
        // Outline: a consistent band of another colour right outside the plate.
        let d = edt_sq(w, h, &filled);
        let band = |lo: f32, hi: f32| -> Vec<[f32; 3]> {
            (0..w * h).filter(|&i| d[i] > lo * lo && d[i] <= hi * hi).map(|i| clean.rgb_f((i % w) as i64, (i / w) as i64)).collect()
        };
        let mut outline = None;
        let b1 = band(0.0, 2.0);
        if let Some(oc) = median_color(&b1)
            && dist2(oc, c).sqrt() > 60.0
        {
            let mut width = 0.0;
            for k in 1..12 {
                let b = band(k as f32 - 1.0, k as f32);
                let ok = b.iter().filter(|p| dist2(**p, oc).sqrt() < 60.0).count();
                if (ok as f32) < 0.6 * b.len().max(1) as f32 {
                    break;
                }
                width = k as f32;
            }
            if width >= 1.0 {
                outline = Some((oc, width));
            }
        }
        let grow = outline.map_or(0.5, |o| o.1 + 0.5);
        let full = dilate(w, h, &filled, grow);
        let soft = crate::image::box_blur(w, h, &full.iter().map(|b| if *b { 1.0 } else { 0.0 }).collect::<Vec<_>>(), 1, 1);
        let bbox = crate::image::bbox_of(w, h, |i| full[i]);
        let ellipse_like = ellipse_fill_ratio(&filled, w, &crate::image::bbox_of(w, h, |i| filled[i]));
        let kind = if outline.is_some() && ellipse_like < 0.9 {
            PlateKind::Bubble
        } else if ellipse_like < 0.85 {
            PlateKind::Badge
        } else {
            PlateKind::Banner
        };
        plates.push(Plate { mask: Mask { w, h, v: soft }, bbox, color: c, outline, lines: vec![li], kind });
    }
    plates
}

/// Fraction of the bounding box the shape fills (≈1 rectangle, ≈0.785 ellipse, less for stars).
fn ellipse_fill_ratio(m: &[bool], w: usize, b: &IRect) -> f32 {
    if b.is_empty() {
        return 1.0;
    }
    let mut n = 0usize;
    for y in b.y0..b.y1 {
        for x in b.x0..b.x1 {
            if m[y as usize * w + x as usize] {
                n += 1;
            }
        }
    }
    n as f32 / b.area() as f32
}

fn ring_pixels(img: &Rgba8, inner: &IRect, outer: &IRect) -> Vec<[f32; 3]> {
    let mut v = Vec::new();
    for y in outer.y0..outer.y1 {
        for x in outer.x0..outer.x1 {
            if x >= inner.x0 && x < inner.x1 && y >= inner.y0 && y < inner.y1 {
                continue;
            }
            v.push(img.rgb_f(x, y));
        }
    }
    v
}

/// Set every pixel not reachable from the image border through unset pixels.
fn fill_enclosed(w: usize, h: usize, m: &[bool]) -> Vec<bool> {
    let mut outside = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    for x in 0..w {
        for y in [0, h.saturating_sub(1)] {
            let i = y * w + x;
            if !m[i] && !outside[i] {
                outside[i] = true;
                stack.push(i);
            }
        }
    }
    for y in 0..h {
        for x in [0, w.saturating_sub(1)] {
            let i = y * w + x;
            if !m[i] && !outside[i] {
                outside[i] = true;
                stack.push(i);
            }
        }
    }
    while let Some(i) = stack.pop() {
        let (x, y) = ((i % w) as i64, (i / w) as i64);
        for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            if !m[j] && !outside[j] {
                outside[j] = true;
                stack.push(j);
            }
        }
    }
    outside.iter().map(|o| !o).collect()
}
