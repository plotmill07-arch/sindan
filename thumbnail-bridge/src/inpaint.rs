//! Background repair with PhotoCraft's own algorithms:
//! * holes on a flat or smoothly varying plate (a banner, a speech bubble, a sky) get a
//!   membrane (harmonic) fill from their border, `photocraft_algo::poisson::membrane_fill`, with
//!   border outliers (an outline or neighbouring text touching the hole) pulled to the plate
//!   colour first — no texture to invent, and nothing foreign gets copied in;
//! * holes in texture get PhotoCraft's content-aware fill (`photocraft_algo::inpaint`, the
//!   PatchMatch / space-time completion behind Edit › Content-Aware Fill), in a window around
//!   the hole so the copies come from nearby.
//!
//! Pixels marked `avoid` are never copied from.

use photocraft_algo::inpaint::{CompleteParams, Px, complete_masked};

use crate::image::{IRect, Rgba8, components, dist2, median_color};

/// How the hole was filled (for reports and tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Membrane,
    PatchMatch,
}

/// Fill `hole` (image-sized). Returns the repaired image (alpha kept) and the method used for
/// each hole region.
pub fn fill(img: &Rgba8, hole: &[bool], avoid: Option<&[bool]>) -> Rgba8 {
    fill_report(img, hole, avoid).0
}

pub fn fill_report(img: &Rgba8, hole: &[bool], avoid: Option<&[bool]>) -> (Rgba8, Vec<Method>) {
    let (w, h) = (img.w, img.h);
    let mut out = img.clone();
    let mut methods = Vec::new();
    if hole.len() != w * h {
        return (out, methods);
    }
    let grown = crate::image::dilate(w, h, hole, 3.0);
    let (lab, n) = components(w, h, &grown);
    let mut boxes = vec![IRect::default(); n as usize + 1];
    for y in 0..h {
        for x in 0..w {
            let l = lab[y * w + x] as usize;
            if l > 0 && hole[y * w + x] {
                let b = IRect { x0: x as i64, y0: y as i64, x1: x as i64 + 1, y1: y as i64 + 1 };
                boxes[l] = boxes[l].union(&b);
            }
        }
    }
    for (l, b) in boxes.iter().enumerate().skip(1) {
        if b.is_empty() {
            continue;
        }
        let this = |i: usize| hole[i] && lab[i] as usize == l;
        // Border ring of this hole: is it one smooth plate?
        let ring_win = b.grow(4).clamp_to(w, h);
        let mut ring = Vec::new();
        for y in ring_win.y0..ring_win.y1 {
            for x in ring_win.x0..ring_win.x1 {
                let i = y as usize * w + x as usize;
                if !hole[i] && grown[i] && lab[i] as usize == l {
                    ring.push(out.rgb_f(x, y));
                }
            }
        }
        let flat = median_color(&ring).filter(|m| {
            let near = ring.iter().filter(|p| dist2(**p, *m).sqrt() < 28.0).count();
            near as f32 >= 0.85 * ring.len().max(1) as f32
        });
        let method = if let Some(m) = flat {
            membrane(&mut out, w, h, &ring_win, &this, m);
            Method::Membrane
        } else {
            patchmatch(&mut out, w, h, b, hole, avoid, &this);
            Method::PatchMatch
        };
        methods.push(method);
    }
    (out, methods)
}

fn membrane(out: &mut Rgba8, w: usize, h: usize, win: &IRect, this: &dyn Fn(usize) -> bool, plate: [f32; 3]) {
    let (ww, wh) = (win.w() as usize, win.h() as usize);
    let mut buf = Vec::with_capacity(ww * wh * 3);
    let mut holes = Vec::with_capacity(ww * wh);
    for y in win.y0..win.y1 {
        for x in win.x0..win.x1 {
            let i = y as usize * w + x as usize;
            let mut p = out.rgb_f(x, y);
            // Border outliers (an outline, other ink) would bleed in: use the plate colour.
            if !this(i) && dist2(p, plate).sqrt() >= 28.0 {
                p = plate;
            }
            buf.extend_from_slice(&[p[0] / 255.0, p[1] / 255.0, p[2] / 255.0]);
            holes.push(this(i));
        }
    }
    let filled = photocraft_algo::poisson::membrane_fill(ww, wh, 3, &buf, &holes);
    write_back(out, w, h, win, &filled, this);
}

fn patchmatch(out: &mut Rgba8, w: usize, h: usize, b: &IRect, hole: &[bool], avoid: Option<&[bool]>, this: &dyn Fn(usize) -> bool) {
    // Context around the hole, proportional to its size.
    let ext = ((b.w().max(b.h()) as f32 * 0.6) as i64).clamp(40, 200);
    let win = b.grow(ext).clamp_to(w, h);
    let (ww, wh) = (win.w() as usize, win.h() as usize);
    let mut buf = Vec::with_capacity(ww * wh * 3);
    let mut px = Vec::with_capacity(ww * wh);
    for y in win.y0..win.y1 {
        for x in win.x0..win.x1 {
            let i = y as usize * w + x as usize;
            let p = out.px[i];
            buf.extend_from_slice(&[p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]);
            px.push(if this(i) {
                Px::Hole
            } else if hole[i] || avoid.is_some_and(|a| a.get(i).copied().unwrap_or(false)) {
                // Other holes (not yet repaired) and avoided pixels: context only.
                Px::Known
            } else {
                Px::Source
            });
        }
    }
    let params = CompleteParams::default();
    let filled = match complete_masked(ww, wh, 3, &buf, &px, &params, &photocraft_raster::Interrupt::NONE) {
        Ok(Some(v)) => v,
        _ => {
            let holes: Vec<bool> = px.iter().map(|p| *p == Px::Hole).collect();
            photocraft_algo::poisson::membrane_fill(ww, wh, 3, &buf, &holes)
        }
    };
    write_back(out, w, h, &win, &filled, this);
}

fn write_back(out: &mut Rgba8, w: usize, _h: usize, win: &IRect, filled: &[f32], this: &dyn Fn(usize) -> bool) {
    let ww = win.w() as usize;
    for y in win.y0..win.y1 {
        for x in win.x0..win.x1 {
            let i = y as usize * w + x as usize;
            if !this(i) {
                continue;
            }
            let k = ((y - win.y0) as usize * ww + (x - win.x0) as usize) * 3;
            let q = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
            if let Some(s) = filled.get(k..k + 3) {
                let a = out.px[i][3];
                out.px[i] = [q(s[0]), q(s[1]), q(s[2]), a];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_plate_gets_membrane_and_plate_colour() {
        let (w, h) = (60, 40);
        let mut img = Rgba8::new(w, h, [250, 220, 0, 255]);
        let mut hole = vec![false; w * h];
        for y in 15..25 {
            for x in 10..50 {
                img.px[y * w + x] = [0, 0, 0, 255];
                hole[y * w + x] = true;
            }
        }
        let (out, m) = fill_report(&img, &hole, None);
        assert_eq!(m, vec![Method::Membrane]);
        let p = out.px[20 * w + 30];
        assert!(p[0] > 240 && p[1] > 210 && p[2] < 10, "{p:?}");
    }

    #[test]
    fn mismatched_hole_is_ignored() {
        let img = Rgba8::new(4, 4, [1, 2, 3, 255]);
        assert_eq!(fill(&img, &[true; 3], None), img);
    }
}
