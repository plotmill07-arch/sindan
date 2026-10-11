//! Person cut-out with U²-Net (human segmentation weights), refined against the image with a
//! guided filter (K. He, J. Sun, X. Tang, *Guided Image Filtering*, ECCV 2010) so the alpha
//! follows hair and suit edges rather than the 320×320 network grid.

use crate::image::{Mask, Rgba8, box_blur};
use crate::models::{Model, PERSON_SEG};
use crate::Result;

const SIDE: usize = 320;

pub struct PersonSegmenter {
    model: Model,
}

impl PersonSegmenter {
    pub fn load() -> Result<Self> {
        Ok(Self { model: Model::load(&PERSON_SEG)? })
    }

    /// Soft person matte (0..1) at the image size.
    pub fn matte(&self, img: &Rgba8) -> Result<Mask> {
        let rgb = img.resize_rgb(SIDE, SIDE);
        let mx = rgb.iter().flat_map(|p| p.iter().copied()).fold(1e-6f32, f32::max);
        let mean = [0.485f32, 0.456, 0.406];
        let std = [0.229f32, 0.224, 0.225];
        let plane = SIDE * SIDE;
        let mut data = vec![0.0f32; 3 * plane];
        for (i, p) in rgb.iter().enumerate() {
            for c in 0..3 {
                data[c * plane + i] = (p[c] / mx - mean[c]) / std[c];
            }
        }
        let (shape, out) = self.model.run([1, 3, SIDE, SIDE], data)?;
        let (oh, ow) = match shape.as_slice() {
            [_, _, h, w] => (*h, *w),
            _ => (SIDE, SIDE),
        };
        let pred = out.get(..oh * ow).unwrap_or(&[]);
        let (lo, hi) = pred.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |a, &v| (a.0.min(v), a.1.max(v)));
        let span = (hi - lo).max(1e-6);
        // Upsample bilinearly to the image size.
        let mut m = Mask::new(img.w, img.h);
        for y in 0..img.h {
            let fy = ((y as f32 + 0.5) * oh as f32 / img.h as f32 - 0.5).clamp(0.0, oh as f32 - 1.0);
            let y0 = fy.floor() as usize;
            let y1 = (y0 + 1).min(oh - 1);
            let ty = fy - y0 as f32;
            for x in 0..img.w {
                let fx = ((x as f32 + 0.5) * ow as f32 / img.w as f32 - 0.5).clamp(0.0, ow as f32 - 1.0);
                let x0 = fx.floor() as usize;
                let x1 = (x0 + 1).min(ow - 1);
                let tx = fx - x0 as f32;
                let g = |yy: usize, xx: usize| (pred.get(yy * ow + xx).copied().unwrap_or(lo) - lo) / span;
                let v = (g(y0, x0) * (1.0 - tx) + g(y0, x1) * tx) * (1.0 - ty) + (g(y1, x0) * (1.0 - tx) + g(y1, x1) * tx) * ty;
                m.v[y * img.w + x] = v;
            }
        }
        let refined = guided_filter(img, &m, 6, 1e-3);
        // Tighten the soft ramp a little; keep only the confident body of the matte.
        let v = refined.v.iter().map(|&a| ((a - 0.2) / 0.6).clamp(0.0, 1.0)).collect();
        Ok(Mask { v, ..refined })
    }
}

/// Grey-guided filter of `p` (radius `r`, regulariser `eps` on 0..1 intensities).
pub fn guided_filter(img: &Rgba8, p: &Mask, r: usize, eps: f32) -> Mask {
    let (w, h) = (img.w, img.h);
    let i: Vec<f32> = img.px.iter().map(|q| (0.299 * q[0] as f32 + 0.587 * q[1] as f32 + 0.114 * q[2] as f32) / 255.0).collect();
    let blur = |v: &[f32]| box_blur(w, h, v, r, 1);
    let mean_i = blur(&i);
    let mean_p = blur(&p.v);
    let ii: Vec<f32> = i.iter().map(|x| x * x).collect();
    let ip: Vec<f32> = i.iter().zip(&p.v).map(|(a, b)| a * b).collect();
    let corr_i = blur(&ii);
    let corr_ip = blur(&ip);
    let n = w * h;
    let mut a = vec![0.0f32; n];
    let mut b = vec![0.0f32; n];
    for k in 0..n {
        let var = corr_i[k] - mean_i[k] * mean_i[k];
        let cov = corr_ip[k] - mean_i[k] * mean_p[k];
        a[k] = cov / (var + eps);
        b[k] = mean_p[k] - a[k] * mean_i[k];
    }
    let ma = blur(&a);
    let mb = blur(&b);
    let v = (0..n).map(|k| (ma[k] * i[k] + mb[k]).clamp(0.0, 1.0)).collect();
    Mask { w, h, v }
}
