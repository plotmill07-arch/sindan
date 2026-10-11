//! Small, dependency-free image helpers used by the analysis passes: an RGBA8 buffer, masks,
//! bilinear sampling, morphology, an exact Euclidean distance transform, connected components and
//! a deterministic k-means. Everything is bounds-checked; nothing here panics on odd sizes.

use rayon::prelude::*;

/// Interleaved 8-bit RGBA image, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Rgba8 {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 4]>,
}

impl Rgba8 {
    pub fn new(w: usize, h: usize, fill: [u8; 4]) -> Self {
        Self { w, h, px: vec![fill; w * h] }
    }

    #[inline]
    pub fn get(&self, x: i64, y: i64) -> [u8; 4] {
        let x = x.clamp(0, self.w as i64 - 1) as usize;
        let y = y.clamp(0, self.h as i64 - 1) as usize;
        self.px.get(y * self.w + x).copied().unwrap_or([0, 0, 0, 0])
    }

    #[inline]
    pub fn rgb_f(&self, x: i64, y: i64) -> [f32; 3] {
        let p = self.get(x, y);
        [p[0] as f32, p[1] as f32, p[2] as f32]
    }

    /// Bilinear sample at a continuous position (pixel centres at `i + 0.5`), edge-clamped.
    pub fn sample(&self, fx: f32, fy: f32) -> [f32; 4] {
        let x = fx - 0.5;
        let y = fy - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let (tx, ty) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let a = self.get(x0, y0);
        let b = self.get(x0 + 1, y0);
        let c = self.get(x0, y0 + 1);
        let d = self.get(x0 + 1, y0 + 1);
        let mut out = [0.0; 4];
        for i in 0..4 {
            let top = a[i] as f32 * (1.0 - tx) + b[i] as f32 * tx;
            let bot = c[i] as f32 * (1.0 - tx) + d[i] as f32 * tx;
            out[i] = top * (1.0 - ty) + bot * ty;
        }
        out
    }

    /// Area-averaging (when shrinking) / bilinear (when growing) resize to `w × h`, as f32 RGB.
    pub fn resize_rgb(&self, w: usize, h: usize) -> Vec<[f32; 3]> {
        let sx = self.w as f32 / w.max(1) as f32;
        let sy = self.h as f32 / h.max(1) as f32;
        (0..h)
            .into_par_iter()
            .flat_map_iter(|y| {
                (0..w).map(move |x| {
                    if sx <= 1.0 && sy <= 1.0 {
                        let p = self.sample((x as f32 + 0.5) * sx, (y as f32 + 0.5) * sy);
                        [p[0], p[1], p[2]]
                    } else {
                        // Box filter over the source footprint (sampled on a small grid).
                        let n = 3;
                        let mut acc = [0.0f32; 3];
                        for j in 0..n {
                            for i in 0..n {
                                let fx = (x as f32 + (i as f32 + 0.5) / n as f32) * sx;
                                let fy = (y as f32 + (j as f32 + 0.5) / n as f32) * sy;
                                let p = self.sample(fx, fy);
                                acc[0] += p[0];
                                acc[1] += p[1];
                                acc[2] += p[2];
                            }
                        }
                        let k = (n * n) as f32;
                        [acc[0] / k, acc[1] / k, acc[2] / k]
                    }
                })
            })
            .collect()
    }

    /// Copy of the pixels with `alpha` (0..1 per pixel) as their alpha channel.
    pub fn with_alpha(&self, alpha: &Mask) -> Rgba8 {
        let px = self
            .px
            .iter()
            .zip(&alpha.v)
            .map(|(p, a)| [p[0], p[1], p[2], (a.clamp(0.0, 1.0) * 255.0).round() as u8])
            .collect();
        Rgba8 { w: self.w, h: self.h, px }
    }
}

/// Rec. 601 luma of an 8-bit pixel.
#[inline]
pub fn luma(p: [f32; 3]) -> f32 {
    0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]
}

#[inline]
pub fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
}

/// Integer pixel rectangle `[x0, x1) × [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct IRect {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl IRect {
    pub fn w(&self) -> i64 {
        (self.x1 - self.x0).max(0)
    }
    pub fn h(&self) -> i64 {
        (self.y1 - self.y0).max(0)
    }
    pub fn is_empty(&self) -> bool {
        self.w() == 0 || self.h() == 0
    }
    pub fn area(&self) -> i64 {
        self.w() * self.h()
    }
    pub fn grow(&self, d: i64) -> IRect {
        IRect { x0: self.x0 - d, y0: self.y0 - d, x1: self.x1 + d, y1: self.y1 + d }
    }
    pub fn clamp_to(&self, w: usize, h: usize) -> IRect {
        IRect { x0: self.x0.clamp(0, w as i64), y0: self.y0.clamp(0, h as i64), x1: self.x1.clamp(0, w as i64), y1: self.y1.clamp(0, h as i64) }
    }
    pub fn union(&self, o: &IRect) -> IRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        IRect { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }
    pub fn intersect(&self, o: &IRect) -> IRect {
        let r = IRect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) };
        if r.x1 <= r.x0 || r.y1 <= r.y0 { IRect::default() } else { r }
    }
}

/// A float mask (coverage 0..1), row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub v: Vec<f32>,
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, v: vec![0.0; w * h] }
    }
    pub fn from_bools(w: usize, h: usize, b: &[bool]) -> Self {
        Self { w, h, v: b.iter().map(|&x| if x { 1.0 } else { 0.0 }).collect() }
    }
    #[inline]
    pub fn at(&self, x: i64, y: i64) -> f32 {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return 0.0;
        }
        self.v.get(y as usize * self.w + x as usize).copied().unwrap_or(0.0)
    }
    #[inline]
    pub fn set(&mut self, x: i64, y: i64, val: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = y as usize * self.w + x as usize;
        if let Some(p) = self.v.get_mut(i) {
            *p = val;
        }
    }
    pub fn bools(&self, thresh: f32) -> Vec<bool> {
        self.v.iter().map(|&a| a >= thresh).collect()
    }
    pub fn count(&self, thresh: f32) -> usize {
        self.v.iter().filter(|&&a| a >= thresh).count()
    }
    pub fn bbox(&self, thresh: f32) -> IRect {
        bbox_of(self.w, self.h, |i| self.v.get(i).is_some_and(|&a| a >= thresh))
    }
    pub fn max_with(&mut self, o: &Mask) {
        for (a, b) in self.v.iter_mut().zip(&o.v) {
            *a = a.max(*b);
        }
    }
}

/// Bounding box of the set pixels of a `w × h` predicate.
pub fn bbox_of(w: usize, h: usize, f: impl Fn(usize) -> bool) -> IRect {
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for y in 0..h {
        for x in 0..w {
            if f(y * w + x) {
                x0 = x0.min(x as i64);
                y0 = y0.min(y as i64);
                x1 = x1.max(x as i64 + 1);
                y1 = y1.max(y as i64 + 1);
            }
        }
    }
    if x0 == i64::MAX { IRect::default() } else { IRect { x0, y0, x1, y1 } }
}

/// Squared Euclidean distance from every pixel to the nearest `true` pixel (Felzenszwalb &
/// Huttenlocher 2012, two separable passes). Pixels with no feature at all get `f32::INFINITY`.
pub fn edt_sq(w: usize, h: usize, feature: &[bool]) -> Vec<f32> {
    const INF: f32 = 1e20;
    let mut f: Vec<f32> = feature.iter().map(|&b| if b { 0.0 } else { INF }).collect();
    // Columns.
    let cols: Vec<Vec<f32>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let col: Vec<f32> = (0..h).map(|y| f.get(y * w + x).copied().unwrap_or(INF)).collect();
            edt_1d(&col)
        })
        .collect();
    for (x, col) in cols.iter().enumerate() {
        for (y, v) in col.iter().enumerate() {
            if let Some(p) = f.get_mut(y * w + x) {
                *p = *v;
            }
        }
    }
    // Rows.
    f.par_chunks_mut(w.max(1)).for_each(|row| {
        let r = edt_1d(row);
        row.copy_from_slice(&r);
    });
    f.iter().map(|&d| if d >= INF * 0.5 { f32::INFINITY } else { d }).collect()
}

fn edt_1d(f: &[f32]) -> Vec<f32> {
    let n = f.len();
    let mut d = vec![0.0f32; n];
    if n == 0 {
        return d;
    }
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f32; n + 1];
    let mut k = 0usize;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * q as f32 - 2.0 * p as f32);
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                // k == 0: replace the first parabola.
                v[0] = q;
                z[0] = f32::NEG_INFINITY;
                z[1] = f32::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, dq) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let dx = q as f32 - p as f32;
        *dq = dx * dx + f[p];
    }
    d
}

/// Disk dilation by radius `r` (pixels within distance `r` of a set pixel).
pub fn dilate(w: usize, h: usize, m: &[bool], r: f32) -> Vec<bool> {
    if r <= 0.0 {
        return m.to_vec();
    }
    let d = edt_sq(w, h, m);
    let r2 = r * r;
    d.iter().map(|&x| x <= r2).collect()
}

/// Disk erosion by radius `r`.
pub fn erode(w: usize, h: usize, m: &[bool], r: f32) -> Vec<bool> {
    let inv: Vec<bool> = m.iter().map(|b| !b).collect();
    dilate(w, h, &inv, r).iter().map(|b| !b).collect()
}

/// Separable box blur of a float field (radius `r`, edge-clamped), applied `passes` times
/// (3 passes ≈ Gaussian with σ ≈ r).
pub fn box_blur(w: usize, h: usize, v: &[f32], r: usize, passes: usize) -> Vec<f32> {
    if r == 0 || w == 0 || h == 0 {
        return v.to_vec();
    }
    let mut a = v.to_vec();
    for _ in 0..passes {
        // Horizontal.
        let mut b = vec![0.0f32; w * h];
        b.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let src = &a[y * w..(y + 1) * w];
            blur_line(src, row, r);
        });
        // Vertical.
        let mut c = vec![0.0f32; w * h];
        let cols: Vec<Vec<f32>> = (0..w)
            .into_par_iter()
            .map(|x| {
                let col: Vec<f32> = (0..h).map(|y| b[y * w + x]).collect();
                let mut out = vec![0.0; h];
                blur_line(&col, &mut out, r);
                out
            })
            .collect();
        for (x, col) in cols.iter().enumerate() {
            for (y, val) in col.iter().enumerate() {
                c[y * w + x] = *val;
            }
        }
        a = c;
    }
    a
}

fn blur_line(src: &[f32], dst: &mut [f32], r: usize) {
    let n = src.len();
    if n == 0 {
        return;
    }
    let get = |i: i64| src[i.clamp(0, n as i64 - 1) as usize];
    let ri = r as i64;
    let mut acc: f32 = (-ri..=ri).map(get).sum();
    let k = (2 * r + 1) as f32;
    for (i, d) in dst.iter_mut().enumerate() {
        *d = acc / k;
        let i = i as i64;
        acc += get(i + ri + 1) - get(i - ri);
    }
}

/// 8-connected components of `m`. Returns per-pixel labels (0 = background) and the count.
pub fn components(w: usize, h: usize, m: &[bool]) -> (Vec<u32>, u32) {
    let mut lab = vec![0u32; w * h];
    let mut n = 0u32;
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..w * h {
        if !m.get(start).copied().unwrap_or(false) || lab[start] != 0 {
            continue;
        }
        n += 1;
        lab[start] = n;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            for dy in -1..=1i64 {
                for dx in -1..=1i64 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if m[j] && lab[j] == 0 {
                        lab[j] = n;
                        stack.push(j);
                    }
                }
            }
        }
    }
    (lab, n)
}

/// Deterministic k-means (k-means++ seeding with a fixed hash RNG). Returns centres and
/// per-point assignments. `k` is reduced to the number of points when there are fewer.
pub fn kmeans(points: &[[f32; 3]], k: usize, iters: usize) -> (Vec<[f32; 3]>, Vec<usize>) {
    if points.is_empty() || k == 0 {
        return (Vec::new(), vec![0; points.len()]);
    }
    let k = k.min(points.len());
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        rng = rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut centres = vec![points[(next() % points.len() as u64) as usize]];
    let mut d: Vec<f32> = points.iter().map(|p| dist2(*p, centres[0])).collect();
    while centres.len() < k {
        let total: f32 = d.iter().sum();
        if total <= 0.0 {
            break;
        }
        let mut t = (next() % 1_000_000) as f32 / 1_000_000.0 * total;
        let mut pick = points.len() - 1;
        for (i, di) in d.iter().enumerate() {
            t -= di;
            if t <= 0.0 {
                pick = i;
                break;
            }
        }
        let c = points[pick];
        centres.push(c);
        for (di, p) in d.iter_mut().zip(points) {
            *di = di.min(dist2(*p, c));
        }
    }
    let mut assign = vec![0usize; points.len()];
    for _ in 0..iters {
        let mut changed = false;
        for (a, p) in assign.iter_mut().zip(points) {
            let best = nearest(&centres, *p);
            if best != *a {
                *a = best;
                changed = true;
            }
        }
        let mut sum = vec![[0.0f64; 3]; centres.len()];
        let mut cnt = vec![0usize; centres.len()];
        for (a, p) in assign.iter().zip(points) {
            for c in 0..3 {
                sum[*a][c] += p[c] as f64;
            }
            cnt[*a] += 1;
        }
        for (i, c) in centres.iter_mut().enumerate() {
            if cnt[i] > 0 {
                *c = [(sum[i][0] / cnt[i] as f64) as f32, (sum[i][1] / cnt[i] as f64) as f32, (sum[i][2] / cnt[i] as f64) as f32];
            }
        }
        if !changed {
            break;
        }
    }
    for (a, p) in assign.iter_mut().zip(points) {
        *a = nearest(&centres, *p);
    }
    (centres, assign)
}

pub fn nearest(centres: &[[f32; 3]], p: [f32; 3]) -> usize {
    let mut best = (f32::INFINITY, 0usize);
    for (i, c) in centres.iter().enumerate() {
        let d = dist2(*c, p);
        if d < best.0 {
            best = (d, i);
        }
    }
    best.1
}

/// Median of a slice (sorted copy); `None` when empty.
pub fn median(v: &[f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    let mut s: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if s.is_empty() {
        return None;
    }
    s.sort_by(|a, b| a.total_cmp(b));
    s.get(s.len() / 2).copied()
}

/// Per-channel median colour of a set of pixels.
pub fn median_color(px: &[[f32; 3]]) -> Option<[f32; 3]> {
    let r: Vec<f32> = px.iter().map(|p| p[0]).collect();
    let g: Vec<f32> = px.iter().map(|p| p[1]).collect();
    let b: Vec<f32> = px.iter().map(|p| p[2]).collect();
    Some([median(&r)?, median(&g)?, median(&b)?])
}

pub fn hex(c: [f32; 3]) -> String {
    let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edt_matches_brute_force() {
        let (w, h) = (17, 11);
        let mut m = vec![false; w * h];
        m[3 * w + 4] = true;
        m[9 * w + 15] = true;
        let d = edt_sq(w, h, &m);
        for y in 0..h {
            for x in 0..w {
                let a = ((x as f32 - 4.0).powi(2) + (y as f32 - 3.0).powi(2)).min((x as f32 - 15.0).powi(2) + (y as f32 - 9.0).powi(2));
                assert!((d[y * w + x] - a).abs() < 1e-3, "{x},{y}");
            }
        }
        assert!(edt_sq(3, 3, &[false; 9]).iter().all(|v| v.is_infinite()));
    }

    #[test]
    fn components_and_kmeans() {
        let m = [true, false, true, true, false, false, false, false, true];
        let (_, n) = components(3, 3, &m);
        assert_eq!(n, 2);
        let pts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [250.0, 250.0, 250.0], [251.0, 250.0, 250.0]];
        let (c, a) = kmeans(&pts, 2, 10);
        assert_eq!(c.len(), 2);
        assert_eq!(a[0], a[1]);
        assert_ne!(a[0], a[2]);
        assert_eq!(kmeans(&[], 3, 5).0.len(), 0);
    }

    #[test]
    fn blur_keeps_constant() {
        let v = vec![2.0; 50];
        assert!(box_blur(10, 5, &v, 2, 3).iter().all(|x| (x - 2.0).abs() < 1e-5));
    }
}
