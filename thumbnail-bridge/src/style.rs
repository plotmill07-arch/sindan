//! Text appearance analysis for one OCR line: which pixels are glyph fill, which are an outline
//! (stroke), whether there is a drop shadow or an outer glow, and their colours and sizes.
//!
//! The method is classical image analysis on the line's neighbourhood:
//! 1. a colour histogram of the box against one of its immediate surroundings;
//! 2. colours much more frequent inside the box than around it are "ink";
//! 3. ink components that cross the line's core band are kept (drops background specks);
//! 4. ink clusters that sit on the ink/background boundary all round are an outline; the deeper
//!    ones are the fill; the outline width is the distance from the fill to the outline's edge;
//! 5. a drop shadow is the offset at which the glyph shape, shifted, darkens the backdrop the
//!    most compared with the opposite shift; its softness is the blur that best explains it;
//! 6. a glow is a coloured halo of consistent colour hugging the glyphs.
//!
//! Everything is computed in a crop around the line; masks are crop-sized.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::image::{IRect, Rgba8, box_blur, components, dilate, dist2, edt_sq, kmeans, luma, median_color, nearest};
use crate::ocr::OcrLine;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StrokeStyle {
    pub width: f32,
    pub color: [f32; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ShadowStyle {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GlowStyle {
    pub size: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

/// A run of characters sharing one fill colour (character indices, end exclusive).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ColorRun {
    pub start: usize,
    pub end: usize,
    pub color: [f32; 3],
}

#[derive(Clone, Debug)]
pub struct LineStyle {
    /// Image-space rectangle the masks below cover.
    pub crop: IRect,
    /// Glyph fill pixels.
    pub fill_mask: Vec<bool>,
    /// Fill ∪ outline pixels.
    pub ink_mask: Vec<bool>,
    /// Pixels to remove from the background (ink, shadow, glow, anti-aliasing margin).
    pub hole: Vec<bool>,
    pub fill: [f32; 3],
    pub runs: Vec<ColorRun>,
    pub stroke: Option<StrokeStyle>,
    pub shadow: Option<ShadowStyle>,
    pub glow: Option<GlowStyle>,
    /// Bounding box of the fill, image space.
    pub fill_bbox: IRect,
}

impl LineStyle {
    pub fn cw(&self) -> usize {
        self.crop.w() as usize
    }
    pub fn ch(&self) -> usize {
        self.crop.h() as usize
    }
}


/// Analyse one line. `None` when no ink could be separated from the background.
pub fn analyze(img: &Rgba8, line: &OcrLine) -> Option<LineStyle> {
    let r = &line.rect;
    let across = if line.vertical { r.w } else { r.h };
    let margin = (0.45 * across).clamp(8.0, 60.0) as i64;
    let crop = r.bbox().grow(margin).clamp_to(img.w, img.h);
    if crop.w() < 4 || crop.h() < 4 {
        return None;
    }
    let (cw, ch) = (crop.w() as usize, crop.h() as usize);
    let n = cw * ch;
    let px: Vec<[f32; 3]> = (0..n).map(|i| img.rgb_f(crop.x0 + (i % cw) as i64, crop.y0 + (i / cw) as i64)).collect();
    // Position of each crop pixel in the line's frame (u along, v across, origin at centre).
    let (ca, sa) = (r.angle.cos(), r.angle.sin());
    let frame = |i: usize| {
        let x = crop.x0 as f32 + (i % cw) as f32 + 0.5 - r.cx;
        let y = crop.y0 as f32 + (i / cw) as f32 + 0.5 - r.cy;
        (x * ca + y * sa, -x * sa + y * ca)
    };
    let (half_along, half_across) = if line.vertical { (r.h / 2.0, r.w / 2.0) } else { (r.w / 2.0, r.h / 2.0) };
    let uv = |i: usize| {
        let (u, v) = frame(i);
        if line.vertical { (v, u) } else { (u, v) }
    };
    let inside = |i: usize, grow: f32| {
        let (u, v) = uv(i);
        u.abs() <= half_along + grow && v.abs() <= half_across + grow
    };
    let in_rect: Vec<bool> = (0..n).map(|i| inside(i, 1.0)).collect();
    let core: Vec<bool> = (0..n)
        .map(|i| {
            let (u, v) = uv(i);
            v.abs() <= 0.3 * half_across && u.abs() <= half_along
        })
        .collect();

    // 1. Colours just around the box (local backdrop) vs inside it. A colour much more frequent
    // inside than around is ink (text fill, outline): a likelihood ratio over a 16³ histogram,
    // smoothed over neighbouring bins.
    let ring_hi = (0.2 * across).max(8.0);
    let mut bg: Vec<[f32; 3]> = (0..n)
        .filter(|&i| {
            let (u, v) = uv(i);
            let du = (u.abs() - half_along).max(0.0);
            let dv = (v.abs() - half_across).max(0.0);
            let d = du.max(dv);
            d > 3.0 && d <= ring_hi
        })
        .map(|i| px[i])
        .collect();
    if bg.len() < 200 {
        // Crop clipped by the image edge: use the box's outer band too.
        bg.extend((0..n).filter(|&i| {
            let (u, v) = uv(i);
            in_rect[i] && v.abs() > 0.8 * half_across && u.abs() <= half_along
        }).map(|i| px[i]));
    }
    let inner: Vec<usize> = (0..n).filter(|&i| in_rect[i]).collect();
    if inner.len() < 20 || bg.is_empty() {
        return None;
    }
    // Text on a plate that ends right around the box (a header band, a label): the box's own
    // border band is one colour that is rare in the ring. That colour is backdrop too.
    let border: Vec<[f32; 3]> = inner
        .iter()
        .copied()
        .filter(|&i| {
            let (_, v) = uv(i);
            v.abs() >= 0.85 * half_across
        })
        .map(|i| px[i])
        .collect();
    if let Some(bc) = median_color(&border) {
        let near = |p: &[f32; 3]| dist2(*p, bc).sqrt() < 30.0;
        let in_border = border.iter().filter(|p| near(p)).count() as f32 / border.len().max(1) as f32;
        let in_ring = bg.iter().filter(|p| near(p)).count() as f32 / bg.len().max(1) as f32;
        if in_border >= 0.6 && in_ring < 0.2 {
            let plate: Vec<[f32; 3]> = inner.iter().map(|&i| px[i]).filter(|p| near(p)).collect();
            bg.extend(plate);
        }
    }
    const NB: usize = 32;
    let bin = |p: [f32; 3]| -> [usize; 3] {
        let q = |v: f32| ((v.clamp(0.0, 255.0) as usize) >> 3).min(NB - 1);
        [q(p[0]), q(p[1]), q(p[2])]
    };
    let idx = |b: [usize; 3]| (b[0] * NB + b[1]) * NB + b[2];
    let mut h_in = vec![0f32; NB * NB * NB];
    let mut h_out = vec![0f32; NB * NB * NB];
    for &i in &inner {
        let b = bin(px[i]);
        h_in[idx(b)] += 1.0;
    }
    for p in &bg {
        let b = bin(*p);
        h_out[idx(b)] += 1.0;
    }
    let (n_in, n_out) = (inner.len() as f32, bg.len() as f32);
    let smooth = |h: &[f32], b: [usize; 3]| -> f32 {
        let mut acc = 0.0;
        for d0 in -1i32..=1 {
            for d1 in -1i32..=1 {
                for d2 in -1i32..=1 {
                    let (x, y, z) = (b[0] as i32 + d0, b[1] as i32 + d1, b[2] as i32 + d2);
                    let nb = NB as i32;
                    if (0..nb).contains(&x) && (0..nb).contains(&y) && (0..nb).contains(&z) {
                        let wgt = if d0 == 0 && d1 == 0 && d2 == 0 { 1.0 } else { 0.15 };
                        acc += wgt * h[((x * nb + y) * nb + z) as usize];
                    }
                }
            }
        }
        acc
    };
    let mut ratio_cache = vec![f32::NAN; NB * NB * NB];
    let mut ratio = |p: [f32; 3]| -> f32 {
        let b = bin(p);
        let k = idx(b);
        if ratio_cache[k].is_nan() {
            let pi = smooth(&h_in, b) / n_in;
            let po = smooth(&h_out, b) / n_out;
            ratio_cache[k] = pi / (po + 0.002);
        }
        ratio_cache[k]
    };

    // 2. Ink candidates near the box.
    let near_box: Vec<bool> = (0..n).map(|i| inside(i, margin as f32 * 0.6)).collect();
    let t0: Vec<bool> = (0..n).map(|i| near_box[i] && ratio(px[i]) > 1.8).collect();
    // Close 1–2 px seams: anti-aliasing between fill and outline mixes their colours into
    // greys that may be common in the backdrop.
    let t0: Vec<bool> = crate::image::erode(cw, ch, &dilate(cw, ch, &t0, 1.5), 1.5).iter().zip(&near_box).map(|(a, b)| *a && *b).collect();
    // 3. Keep ink components touching the core band.
    let (lab, nc) = components(cw, ch, &t0);
    let mut core_hits = vec![0usize; nc as usize + 1];
    for i in 0..n {
        if core[i] && lab[i] > 0 {
            core_hits[lab[i] as usize] += 1;
        }
    }
    let kept: Vec<bool> = (0..n).map(|i| lab[i] > 0 && core_hits[lab[i] as usize] >= 3).collect();
    // Detached marks (dakuten, the dot of "!", small kana) sit off the core band: keep a
    // component that lies mostly inside the box and close to the kept glyphs.
    let close_to_kept = dilate(cw, ch, &kept, 0.25 * across);
    let (mut inside_n, mut total_n, mut touches) = (vec![0usize; nc as usize + 1], vec![0usize; nc as usize + 1], vec![false; nc as usize + 1]);
    for i in 0..n {
        let l = lab[i] as usize;
        if l > 0 {
            total_n[l] += 1;
            if in_rect[i] {
                inside_n[l] += 1;
            }
            if close_to_kept[i] {
                touches[l] = true;
            }
        }
    }
    let mut ink: Vec<bool> = (0..n)
        .map(|i| {
            let l = lab[i] as usize;
            l > 0 && (core_hits[l] >= 3 || (touches[l] && inside_n[l] * 2 > total_n[l]))
        })
        .collect();
    let ink_count = ink.iter().filter(|b| **b).count();
    if ink_count < 15 {
        return None;
    }

    // 4. Fill vs outline: cluster the ink colours.
    let ink_px: Vec<[f32; 3]> = subsample(&(0..n).filter(|&i| ink[i]).map(|i| px[i]).collect::<Vec<_>>(), 12000);
    let (cents, _) = kmeans(&ink_px, 4, 15);
    let label: Vec<usize> = px.iter().map(|p| nearest(&cents, *p)).collect();
    let k = cents.len();
    // Single-colour blobs that reach the edge of the search area (away from the image border)
    // are neighbouring shapes such as a speech bubble or a photo, not glyphs.
    let reach_edge = |i: usize| -> bool {
        let (x, y) = ((i % cw) as i64, (i / cw) as i64);
        [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dy)| {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= cw as i64 || ny >= ch as i64 {
                // Crop edge: only counts when it is not the image edge.
                let (ix, iy) = (crop.x0 + nx, crop.y0 + ny);
                return ix >= 0 && iy >= 0 && (ix as usize) < img.w && (iy as usize) < img.h;
            }
            !near_box[ny as usize * cw + nx as usize]
        })
    };
    for c in 0..k {
        let mc: Vec<bool> = (0..n).map(|i| ink[i] && label[i] == c).collect();
        let (clab, cnc) = components(cw, ch, &mc);
        let mut off = vec![false; cnc as usize + 1];
        for i in 0..n {
            let l = clab[i] as usize;
            if l > 0 && !off[l] && reach_edge(i) {
                off[l] = true;
            }
        }
        // A blob mostly inside the text box is the text's own outline merely touching a
        // neighbour's outline: keep it near the box. Otherwise drop it.
        let mut inside_n = vec![0usize; cnc as usize + 1];
        let mut total_n = vec![0usize; cnc as usize + 1];
        for i in 0..n {
            let l = clab[i] as usize;
            if l > 0 {
                total_n[l] += 1;
                if in_rect[i] {
                    inside_n[l] += 1;
                }
            }
        }
        let keep_margin = 0.12 * across;
        for i in 0..n {
            let l = clab[i] as usize;
            if l > 0 && off[l] {
                let mostly_inside = inside_n[l] * 2 > total_n[l];
                if !mostly_inside || !inside(i, keep_margin) {
                    ink[i] = false;
                }
            }
        }
    }
    let ink_count = ink.iter().filter(|b| **b).count();
    if ink_count < 15 {
        return None;
    }
    let not_ink: Vec<bool> = ink.iter().map(|b| !b).collect();
    let depth: Vec<f32> = edt_sq(cw, ch, &not_ink).iter().map(|d| d.sqrt()).collect();
    let (mut cnt, mut bnd) = (vec![0usize; k], vec![0usize; k]);
    for i in 0..n {
        if ink[i] {
            let c = label[i];
            cnt[c] += 1;
            if depth[i] <= 1.5 {
                bnd[c] += 1;
            }
        }
    }
    // Major ink colours (≥ 6 % of the ink), with near-identical colours grouped (an outline
    // often splits into black and near-black).
    let major: Vec<usize> = (0..k).filter(|&c| cnt[c] * 100 >= ink_count * 6).collect();
    let mut group: Vec<usize> = (0..k).collect();
    for &a in &major {
        for &b in &major {
            if b > a && dist2(cents[a], cents[b]).sqrt() < 75.0 {
                let (ga, gb) = (group[a], group[b]);
                for g in group.iter_mut() {
                    if *g == gb {
                        *g = ga;
                    }
                }
            }
        }
    }
    let mut groups: Vec<usize> = major.iter().map(|&c| group[c]).collect();
    groups.sort_unstable();
    groups.dedup();
    let members = |g: usize| -> Vec<usize> { major.iter().copied().filter(|&c| group[c] == g).collect() };
    let gcnt = |g: usize| members(g).iter().map(|&c| cnt[c]).sum::<usize>();
    let gbnd = |g: usize| members(g).iter().map(|&c| bnd[c]).sum::<usize>();
    let gbfrac = |g: usize| if gcnt(g) == 0 { 1.0 } else { gbnd(g) as f32 / gcnt(g) as f32 };
    let gcolor = |g: usize| {
        let m = members(g);
        let big = m.iter().copied().max_by_key(|&c| cnt[c]).unwrap_or(g);
        cents[big]
    };
    if std::env::var_os("PLOTMILL_DEBUG").is_some() {
        for &g in &groups {
            eprintln!("  [{}] group {g}: {:?} n={} edge={} bfrac={:.3}", line.text, gcolor(g), gcnt(g), gbnd(g), gbfrac(g));
        }
    }
    // Outline = a major colour that encloses another: the ring just around colour A (through
    // anti-aliasing) is mostly colour B. Then A is the fill and B the outline. Without such a
    // pair the text has no outline and the fill is the dominant interior colour.
    let gid: Vec<Option<usize>> = (0..n).map(|i| if ink[i] && major.contains(&label[i]) { Some(group[label[i]]) } else { None }).collect();
    let mut best_pair: Option<(f32, usize, usize)> = None;
    for &ga in &groups {
        if gcnt(ga) * 10 < ink_count {
            continue;
        }
        let ma: Vec<bool> = gid.iter().map(|g| *g == Some(ga)).collect();
        let near = dilate(cw, ch, &ma, 3.0);
        let (mut tot, mut by) = (0usize, vec![0usize; k]);
        for i in 0..n {
            if near[i] && !ma[i] {
                match gid[i] {
                    Some(g) => {
                        by[g] += 1;
                        tot += 1;
                    }
                    None if !ink[i] => tot += 1,
                    None => {} // anti-aliasing between colours: no vote
                }
            }
        }
        for &gb in &groups {
            if gb == ga || tot == 0 {
                continue;
            }
            let share = by[gb] as f32 / tot as f32;
            if std::env::var_os("PLOTMILL_DEBUG").is_some() {
                eprintln!("  [{}] ring of {ga} {:?}: {gb} {:?} share {share:.2}", line.text, gcolor(ga), gcolor(gb));
            }
            if share >= 0.6 && dist2(gcolor(ga), gcolor(gb)).sqrt() > 60.0 && best_pair.is_none_or(|b| share > b.0) {
                best_pair = Some((share, ga, gb));
            }
        }
    }
    let (fill_g, stroke_g) = match best_pair {
        Some((_, a, b)) => (a, Some(b)),
        None => {
            let g = groups.iter().copied().max_by(|a, b| (gcnt(*a) as f32 * (1.0 - gbfrac(*a))).total_cmp(&(gcnt(*b) as f32 * (1.0 - gbfrac(*b)))))?;
            (g, None)
        }
    };
    let fill_c = members(fill_g).into_iter().max_by_key(|&c| cnt[c])?;
    let stroke_ids: Vec<usize> = stroke_g.map(&members).unwrap_or_default();
    let stroke_c = stroke_ids.iter().copied().max_by_key(|&c| cnt[c]);
    // With an outline, other colours enclosed like the fill are fill too (multi-colour titles).
    // Without one, only the fill colour is (shadows and anti-aliasing are not).
    let fill_ids: Vec<usize> = match stroke_g {
        Some(sg) => major.iter().copied().filter(|&c| group[c] != sg && gbfrac(group[c]) <= gbfrac(sg)).collect(),
        None => members(fill_g),
    };
    let others: Vec<usize> = major.iter().copied().filter(|c| !fill_ids.contains(c)).collect();

    // Major colours that are not anti-aliasing between the backdrop and the fill (they are far
    // from the backdrop→fill colour line): shadows, glows, neighbouring shapes.
    let ink_rim = dilate(cw, ch, &ink, 3.0);
    let depth_near = |i: usize| ink_rim[i];
    let rim_bg = median_color(&(0..n).filter(|&i| !ink[i] && depth_near(i)).map(|i| px[i]).collect::<Vec<_>>()).unwrap_or([255.0; 3]);
    let off_ids: Vec<usize> = others
        .iter()
        .copied()
        .filter(|&o| {
            let (b, f, c) = (rim_bg, cents[fill_c], cents[o]);
            let d = [f[0] - b[0], f[1] - b[1], f[2] - b[2]];
            let dd = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).max(1.0);
            let t = (((c[0] - b[0]) * d[0] + (c[1] - b[1]) * d[1] + (c[2] - b[2]) * d[2]) / dd).clamp(0.0, 1.0);
            let q = [b[0] + t * d[0], b[1] + t * d[1], b[2] + t * d[2]];
            dist2(c, q).sqrt() > 40.0
        })
        .collect();
    // Local backdrop colour at every pixel (normalised convolution of the non-ink pixels).
    let bg_field: Vec<[f32; 3]> = {
        let wgt: Vec<f32> = ink.iter().map(|b| if *b { 0.0 } else { 1.0 }).collect();
        let r = (0.12 * across).clamp(4.0, 16.0) as usize;
        let ws = box_blur(cw, ch, &wgt, r, 2);
        let mut chans = Vec::new();
        for c in 0..3 {
            let v: Vec<f32> = (0..n).map(|i| px[i][c] * wgt[i]).collect();
            chans.push(box_blur(cw, ch, &v, r, 2));
        }
        (0..n).map(|i| {
            let w = ws[i].max(1e-4);
            [chans[0][i] / w, chans[1][i] / w, chans[2][i] / w]
        }).collect()
    };
    // Fill mask: ink pixels nearer a fill colour than the outline colour.
    let fill_mask: Vec<bool> = (0..n)
        .map(|i| {
            if !ink[i] {
                return false;
            }
            if stroke_ids.is_empty() {
                // Anti-aliased rim: coverage along the backdrop→fill colour line, cut at 50 %
                // (as the renderer's mask is), so thin and bold faces compare fairly.
                let b = bg_field[i];
                let f = cents[fill_c];
                let d = [f[0] - b[0], f[1] - b[1], f[2] - b[2]];
                let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                if dd > 900.0 {
                    let p = px[i];
                    let a = ((p[0] - b[0]) * d[0] + (p[1] - b[1]) * d[1] + (p[2] - b[2]) * d[2]) / dd;
                    // Off the backdrop→fill line (a shadow's grey, a glow): not fill.
                    let off_line = off_ids.iter().any(|&o| dist2(p, cents[o]) < dist2(p, f));
                    return a >= 0.5 && !off_line;
                }
            }
            let d_other = others.iter().map(|&s| dist2(px[i], cents[s])).fold(f32::INFINITY, f32::min);
            fill_ids.iter().any(|&f| dist2(px[i], cents[f]) < d_other)
        })
        .collect();
    let fill_mask = fill_mask;
    // Fill pieces wholly outside the text box belong to something else.
    let fill_mask: Vec<bool> = {
        let (fl, fnc) = components(cw, ch, &fill_mask);
        let mut any_in = vec![false; fnc as usize + 1];
        for i in 0..n {
            if fl[i] > 0 && in_rect[i] {
                any_in[fl[i] as usize] = true;
            }
        }
        // Specks (stars, noise) far from the glyph bodies go too; dakuten and dots sit close.
        let mut area = vec![0usize; fnc as usize + 1];
        for i in 0..n {
            area[fl[i] as usize] += 1;
        }
        let total: usize = area.iter().skip(1).sum();
        let big: Vec<bool> = (0..n).map(|i| fl[i] > 0 && area[fl[i] as usize] * 200 >= total).collect();
        let near_big = dilate(cw, ch, &big, 0.08 * across);
        let mut speck_ok = vec![false; fnc as usize + 1];
        for i in 0..n {
            if fl[i] > 0 && near_big[i] {
                speck_ok[fl[i] as usize] = true;
            }
        }
        (0..n)
            .map(|i| {
                let l = fl[i] as usize;
                l > 0 && any_in[l] && (area[l] * 200 >= total || speck_ok[l])
            })
            .collect()
    };
    let fill_count = fill_mask.iter().filter(|b| **b).count();
    if fill_count < 10 {
        return None;
    }
    let stroke = stroke_c.and_then(|s| {
        // Outline width: distance from the fill at the ink's outer edge, taken as the median
        // over the edge so a drop shadow merged into the outline on one side doesn't count.
        let from_fill = edt_sq(cw, ch, &fill_mask);
        let edge: Vec<f32> = (0..n)
            .filter(|&i| ink[i] && !fill_mask[i] && depth[i] <= 1.0 && stroke_ids.contains(&label[i]))
            .map(|i| from_fill[i].sqrt())
            .filter(|d| d.is_finite())
            .collect();
        if edge.len() < 10 {
            return None;
        }
        // A lower percentile: a shadow merged into the outline widens only one side.
        let mut edge = edge;
        edge.sort_by(|a, b| a.total_cmp(b));
        let w = edge.get(edge.len() * 35 / 100).copied().unwrap_or(1.0);
        if w < 1.0 {
            return None;
        }
        // Ink further out than that is shadow (or a neighbouring shape), not outline.
        for i in 0..n {
            if ink[i] && !fill_mask[i] && from_fill[i].sqrt() > w + 2.0 {
                ink[i] = false;
            }
        }
        let pure: Vec<[f32; 3]> = (0..n).filter(|&i| ink[i] && !fill_mask[i] && from_fill[i].sqrt() >= 1.5 && from_fill[i].sqrt() <= w - 0.5).map(|i| px[i]).collect();
        let color = median_color(&pure).unwrap_or(cents[s]);
        Some(StrokeStyle { width: w.round().max(1.0), color })
    });
    // Without an outline the ink is the fill plus its anti-aliased rim; anything else that was
    // picked up (a drop shadow, a glow) is handled as an effect below.
    if stroke.is_none() {
        let rim = dilate(cw, ch, &fill_mask, 1.5);
        for i in 0..n {
            ink[i] = ink[i] && rim[i];
        }
    }
    let ink = ink;

    // Interior fill pixels (away from the outline / background) carry the true fill colour.
    let fill_or_bg: Vec<bool> = (0..n).map(|i| !fill_mask[i]).collect();
    let fill_depth: Vec<f32> = edt_sq(cw, ch, &fill_or_bg).iter().map(|d| d.sqrt()).collect();
    let interior: Vec<usize> = (0..n).filter(|&i| fill_mask[i] && fill_depth[i] >= 2.0).collect();
    let interior = if interior.len() >= 10 { interior } else { (0..n).filter(|&i| fill_mask[i]).collect() };
    let fill = median_color(&interior.iter().map(|&i| px[i]).collect::<Vec<_>>()).unwrap_or(cents[fill_c]);

    // Per-character fill colours → colour runs.
    let runs = color_runs(line, &interior, &px, &uv, half_along, fill);

    // 5/6. Shadow and glow around the ink.
    let lum: Vec<f32> = px.iter().map(|p| luma(*p)).collect();
    let shadow = find_shadow(cw, ch, &ink, &lum, &px);
    let (palette, _) = kmeans(&subsample(&bg, 4000), 6, 10);
    let glow = if shadow.is_none() { find_glow(cw, ch, &ink, &px, &palette) } else { None };

    // Hole for inpainting.
    // A margin around the ink also takes faint shadows and glows that were too weak to model
    // (they would otherwise stay behind as a ghost of the text).
    let mut hole = dilate(cw, ch, &ink, 2.5 + (0.06 * across).max(2.0));
    if let Some(s) = &shadow {
        let shifted = shift(cw, ch, &ink, s.dx.round() as i64, s.dy.round() as i64);
        let sh = dilate(cw, ch, &shifted, s.blur * 1.5 + 2.5);
        for (h, s) in hole.iter_mut().zip(sh) {
            *h |= s;
        }
    }
    if let Some(g) = &glow {
        let gh = dilate(cw, ch, &ink, g.size + 2.5);
        for (h, s) in hole.iter_mut().zip(gh) {
            *h |= s;
        }
    }
    // On a flat backdrop, anything still differing from it right around the text (an outline in
    // nearly the backdrop colour, a soft shadow) is text residue: remove it too.
    {
        let dist_ink: Vec<f32> = edt_sq(cw, ch, &ink).iter().map(|d| d.sqrt()).collect();
        let far: Vec<[f32; 3]> = (0..n).filter(|&i| dist_ink[i] > 20.0 && dist_ink[i] <= 28.0).map(|i| px[i]).collect();
        if let Some(bgc) = median_color(&far) {
            let flat = far.iter().filter(|p| dist2(**p, bgc).sqrt() < 18.0).count() as f32 >= 0.85 * far.len().max(1) as f32;
            if flat && far.len() > 100 {
                let residue: Vec<bool> = (0..n).map(|i| hole[i] || (dist_ink[i] <= 20.0 && dist2(px[i], bgc).sqrt() > 9.0)).collect();
                let (rl, _) = components(cw, ch, &residue);
                let mut attached = std::collections::HashSet::new();
                for i in 0..n {
                    if hole[i] && rl[i] > 0 {
                        attached.insert(rl[i]);
                    }
                }
                let grown: Vec<bool> = (0..n).map(|i| rl[i] > 0 && attached.contains(&rl[i])).collect();
                let grown = dilate(cw, ch, &grown, 1.5);
                for (h, g) in hole.iter_mut().zip(grown) {
                    *h |= g;
                }
            }
        }
    }
    let fb = crate::image::bbox_of(cw, ch, |i| fill_mask[i]);
    let fill_bbox = IRect { x0: fb.x0 + crop.x0, y0: fb.y0 + crop.y0, x1: fb.x1 + crop.x0, y1: fb.y1 + crop.y0 };
    Some(LineStyle { crop, fill_mask, ink_mask: ink, hole, fill, runs, stroke, shadow, glow, fill_bbox })
}

fn subsample<T: Copy>(v: &[T], max: usize) -> Vec<T> {
    if v.len() <= max {
        return v.to_vec();
    }
    let step = v.len() as f64 / max as f64;
    (0..max).filter_map(|i| v.get((i as f64 * step) as usize).copied()).collect()
}

fn shift(w: usize, h: usize, m: &[bool], dx: i64, dy: i64) -> Vec<bool> {
    let mut out = vec![false; w * h];
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let (sx, sy) = (x - dx, y - dy);
            if sx >= 0 && sy >= 0 && sx < w as i64 && sy < h as i64 && m[(sy as usize) * w + sx as usize] {
                out[y as usize * w + x as usize] = true;
            }
        }
    }
    out
}

fn color_runs(
    line: &OcrLine,
    interior: &[usize],
    px: &[[f32; 3]],
    uv: &dyn Fn(usize) -> (f32, f32),
    half_along: f32,
    fill: [f32; 3],
) -> Vec<ColorRun> {
    let nchars = line.text.chars().count();
    if nchars == 0 {
        return Vec::new();
    }
    let pos = &line.char_pos;
    if pos.len() != nchars || nchars == 1 {
        return vec![ColorRun { start: 0, end: nchars, color: fill }];
    }
    // Character i spans the midpoints to its neighbours' CTC peaks.
    let bounds: Vec<(f32, f32)> = (0..nchars)
        .map(|i| {
            let lo = if i == 0 { 0.0 } else { (pos[i - 1] + pos[i]) / 2.0 };
            let hi = if i + 1 == nchars { 1.0 } else { (pos[i] + pos[i + 1]) / 2.0 };
            (lo, hi)
        })
        .collect();
    let mut per: Vec<Vec<[f32; 3]>> = vec![Vec::new(); nchars];
    for &i in interior {
        let (u, _) = uv(i);
        let f = ((u + half_along) / (2.0 * half_along)).clamp(0.0, 1.0);
        if let Some(c) = bounds.iter().position(|(lo, hi)| f >= *lo && f <= *hi) {
            per[c].push(px[i]);
        }
    }
    let colors: Vec<[f32; 3]> = per.iter().map(|v| if v.len() >= 12 { median_color(v).unwrap_or(fill) } else { fill }).collect();
    let mut runs: Vec<ColorRun> = Vec::new();
    for (i, c) in colors.iter().enumerate() {
        match runs.last_mut() {
            Some(r) if dist2(r.color, *c).sqrt() < 40.0 => r.end = i + 1,
            _ => runs.push(ColorRun { start: i, end: i + 1, color: *c }),
        }
    }
    // Recompute each run's colour over all its characters.
    for r in &mut runs {
        let all: Vec<[f32; 3]> = per.get(r.start..r.end).unwrap_or(&[]).iter().flatten().copied().collect();
        if let Some(c) = median_color(&all) {
            r.color = c;
        }
    }
    runs
}

fn find_shadow(w: usize, h: usize, ink: &[bool], lum: &[f32], px: &[[f32; 3]]) -> Option<ShadowStyle> {
    const R: i64 = 14;
    // The ring around the ink where a shadow would show.
    let near = dilate(w, h, ink, 1.5);
    let far = dilate(w, h, ink, R as f32 + 2.0);
    let region: Vec<usize> = (0..w * h).filter(|&i| far[i] && !near[i]).collect();
    if region.len() < 80 {
        return None;
    }
    // Darkness against the local backdrop (a wide, ink-free average), so a busy photo behind the
    // text doesn't swamp the signal; correlation with the shifted glyph shape picks the offset.
    let wgt: Vec<f32> = near.iter().map(|b| if *b { 0.0 } else { 1.0 }).collect();
    let lw: Vec<f32> = (0..w * h).map(|i| lum[i] * wgt[i]).collect();
    let sw = box_blur(w, h, &wgt, 12, 2);
    let sl = box_blur(w, h, &lw, 12, 2);
    let y: Vec<f32> = region.iter().map(|&i| sl[i] / sw[i].max(1e-4) - lum[i]).collect();
    let n = y.len() as f32;
    let my = y.iter().map(|v| *v as f64).sum::<f64>() as f32 / n;
    let vy = y.iter().map(|v| ((v - my) * (v - my)) as f64).sum::<f64>() as f32;
    if vy <= 1e-3 {
        return None;
    }
    let at = |x: i64, yy: i64| x >= 0 && yy >= 0 && x < w as i64 && yy < h as i64 && ink[yy as usize * w + x as usize];
    let corr_for = |p: &dyn Fn(usize) -> f32| -> f32 {
        let (mut sp, mut spp, mut spy) = (0.0f64, 0.0f64, 0.0f64);
        for (k, &i) in region.iter().enumerate() {
            let v = p(i) as f64;
            sp += v;
            spp += v * v;
            spy += v * y[k] as f64;
        }
        let nn = n as f64;
        let mp = sp / nn;
        let vp = spp - nn * mp * mp;
        if vp <= 1e-3 {
            return 0.0;
        }
        ((spy - nn * mp * my as f64) / (vp.sqrt() * (vy as f64).sqrt())) as f32
    };
    let offsets: Vec<(i64, i64)> = (-R..=R).flat_map(|dy| (-R..=R).map(move |dx| (dx, dy))).filter(|(dx, dy)| dx * dx + dy * dy >= 4).collect();
    let best = offsets
        .par_iter()
        .map(|&(dx, dy)| {
            let c = corr_for(&|i: usize| if at((i % w) as i64 - dx, (i / w) as i64 - dy) { 1.0 } else { 0.0 });
            (c, dx, dy)
        })
        .reduce(|| (f32::NEG_INFINITY, 0, 0), |a, b| if b.0 > a.0 { b } else { a });
    let (c0, dx, dy) = best;
    if std::env::var_os("PLOTMILL_DEBUG").is_some() {
        eprintln!("  shadow candidate: d=({dx},{dy}) corr={c0:.3}");
    }
    if c0 < 0.3 {
        return None;
    }
    // Softness: blur of the shifted shape that correlates best.
    let shifted = shift(w, h, ink, dx, dy);
    let base: Vec<f32> = shifted.iter().map(|b| if *b { 1.0 } else { 0.0 }).collect();
    let mut best_r = (c0, 0usize, base.clone());
    for r in [1usize, 2, 3, 4, 6, 8] {
        let p = box_blur(w, h, &base, r, 3);
        let c = corr_for(&|i: usize| p[i]);
        if c > best_r.0 + 0.005 {
            best_r = (c, r, p);
        }
    }
    let (corr, blur, p) = best_r;
    // Opacity from a least-squares fit L = L0 · (1 − a·P).
    let (mut sp, mut sl, mut spp, mut spl) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for &i in &region {
        sp += p[i];
        sl += lum[i];
        spp += p[i] * p[i];
        spl += p[i] * lum[i];
    }
    let den = n * spp - sp * sp;
    if den.abs() < 1e-3 {
        return None;
    }
    let slope = (n * spl - sp * sl) / den;
    let l0 = (sl - slope * sp) / n;
    if std::env::var_os("PLOTMILL_DEBUG").is_some() {
        eprintln!("  shadow fit: slope={slope:.1} l0={l0:.1} blur={blur}");
    }
    if l0 <= 1.0 || slope >= 0.0 {
        return None;
    }
    let opacity = (-slope / l0).clamp(0.2, 1.0);
    // A real shadow darkens noticeably where it is solid.
    if -slope < 25.0 {
        return None;
    }
    let solid: Vec<[f32; 3]> = region.iter().copied().filter(|&i| p[i] > 0.8).map(|i| px[i]).collect();
    let sc = median_color(&solid).unwrap_or([0.0; 3]);
    let chroma = sc.iter().cloned().fold(f32::NEG_INFINITY, f32::max) - sc.iter().cloned().fold(f32::INFINITY, f32::min);
    let color = if chroma > 60.0 && opacity > 0.8 { sc } else { [0.0, 0.0, 0.0] };
    if std::env::var_os("PLOTMILL_DEBUG").is_some() {
        eprintln!("  shadow: d=({dx},{dy}) corr={corr:.3} blur={blur} slope={slope:.1} l0={l0:.1}");
    }
    Some(ShadowStyle { dx: dx as f32, dy: dy as f32, blur: blur as f32, color, opacity })
}

fn find_glow(w: usize, h: usize, ink: &[bool], px: &[[f32; 3]], palette: &[[f32; 3]]) -> Option<GlowStyle> {
    let d: Vec<f32> = edt_sq(w, h, ink).iter().map(|d| d.sqrt()).collect();
    let ring = |lo: f32, hi: f32| -> Vec<[f32; 3]> { (0..w * h).filter(|&i| d[i] > lo && d[i] <= hi).map(|i| px[i]).collect() };
    let inner = ring(1.5, 4.0);
    let outer = ring(16.0, 22.0);
    if inner.len() < 40 || outer.len() < 40 {
        return None;
    }
    let ci = median_color(&inner)?;
    let co = median_color(&outer)?;
    let _ = palette;
    if std::env::var_os("PLOTMILL_DEBUG").is_some() {
        let close = inner.iter().filter(|p| dist2(**p, ci).sqrt() < 70.0).count();
        eprintln!("  glow: inner {ci:?} outer {co:?} close {close}/{}", inner.len());
    }
    if dist2(ci, co).sqrt() < 60.0 {
        return None;
    }
    // Consistency: most of the inner ring is near the halo colour.
    let close = inner.iter().filter(|p| dist2(**p, ci).sqrt() < 70.0).count();
    if close * 10 < inner.len() * 6 {
        return None;
    }
    // Extent: where the ring colour has moved halfway to the far colour.
    let mut size = 4.0;
    for k in 2..16 {
        let r = ring(k as f32, k as f32 + 1.0);
        if let Some(c) = median_color(&r)
            && dist2(c, co).sqrt() < 0.5 * dist2(ci, co).sqrt()
        {
            size = (k as f32 * 2.0).max(3.0);
            break;
        }
        size = (k as f32 * 2.0).max(3.0);
    }
    Some(GlowStyle { size, color: ci, opacity: 0.9 })
}
