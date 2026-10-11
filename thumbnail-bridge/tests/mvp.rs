//! MVP acceptance test on the four synthetic Japanese thumbnails in `samples/`
//! (public-domain NASA imagery + OFL fonts, with ground truth in `*.truth.json`).
//!
//! Checks, per the MVP list: the PNG loads; Japanese text is detected; editable type layers are
//! created with the right font family, weight, size and colours; people are cut out on their own
//! layer; background and person are separate layers; the result exports as PNG; it saves as
//! `.pcraft` and `.psd` and reopens with the type layers still editable (an edit through the
//! engine's `type.edit` works after the reload).
//!
//! Needs the models from `scripts/setup.sh`; without them the test reports and skips.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use photocraft_doc::{Document, LayerContent};
use plotmill_bridge::{Engines, OutFormat, Options, open_document, read_image, save};
use serde_json::{Value, json};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rgb(h: &str) -> [f32; 3] {
    let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap() as f32;
    [v(1), v(3), v(5)]
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn type_layers(doc: &Document) -> Vec<(u64, String, String, f32)> {
    doc.walk()
        .iter()
        .filter_map(|(_, _, l)| match &l.content {
            LayerContent::Text(t) => {
                let s = t.char_runs().first().map(|r| r.style.clone()).unwrap_or_default();
                Some((l.id.0, t.text.clone(), s.font_family.clone(), s.size_pt))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn mvp_acceptance_on_japanese_samples() {
    let engines = match Engines::load() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("SKIPPED: models not installed ({e}); run scripts/setup.sh");
            return;
        }
    };
    assert!(engines.person.is_some(), "person model loaded");
    let out = std::env::temp_dir().join(format!("plotmill-mvp-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let cancel = AtomicBool::new(false);
    let (mut lines, mut fonts_ok, mut sizes_ok, mut fills_ok) = (0, 0, 0, 0);
    for n in 1..=4 {
        let png = root().join(format!("samples/thumb{n}.png"));
        let truth: Value = serde_json::from_str(&std::fs::read_to_string(root().join(format!("samples/thumb{n}.truth.json"))).unwrap()).unwrap();
        // 1. load
        let img = read_image(&png).expect("PNG loads");
        assert_eq!((img.w, img.h), (1280, 720));
        let stem = format!("thumb{n}");
        let (doc, d) = plotmill_bridge::convert_image(&engines, &stem, &img, &Options::default(), &|_| {}, &cancel).expect("decomposes");
        // 2/3. text detected and turned into type layers
        let tl = type_layers(&doc);
        let truth_texts = truth["texts"].as_array().unwrap();
        assert!(tl.len() + 1 >= truth_texts.len(), "thumb{n}: {} type layers for {} texts", tl.len(), truth_texts.len());
        for t in truth_texts {
            lines += 1;
            let want = t["text"].as_str().unwrap();
            // The line whose fill box overlaps the truth box.
            let bx: Vec<i64> = t["bbox"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
            let overlap = |l: &&plotmill_bridge::pipeline::LineResult| {
                let f = l.style.as_ref().unwrap().fill_bbox;
                (f.x1.min(bx[2]) - f.x0.max(bx[0])).max(0) * (f.y1.min(bx[3]) - f.y0.max(bx[1])).max(0)
            };
            let Some(l) = d.lines.iter().filter(|l| l.style.is_some() && !l.fits.is_empty()).filter(|l| overlap(l) > 0).max_by_key(overlap) else {
                eprintln!("thumb{n}: no layer for {want}");
                continue;
            };
            let best = &l.fits[0];
            let st = l.style.as_ref().unwrap();
            let family_ok = best.face.family == t["family"].as_str().unwrap() && (best.face.weight as i64 - t["weight"].as_i64().unwrap()).abs() < 150;
            let size_ok = (best.size_px - t["size_px"].as_f64().unwrap() as f32).abs() <= (0.04 * t["size_px"].as_f64().unwrap() as f32).max(2.0);
            let fill_ok = dist(st.fill, rgb(t["fill"].as_str().unwrap())) < 30.0;
            fonts_ok += family_ok as usize;
            sizes_ok += size_ok as usize;
            fills_ok += fill_ok as usize;
            eprintln!("thumb{n}: {} → {} {} {:.0}px (truth {want} {} {}px) font:{family_ok} size:{size_ok} fill:{fill_ok}", l.ocr.text, best.face.family, best.face.style, best.size_px, t["family"], t["size_px"]);
        }
        // 6/7. person on its own layer, background separate, original kept hidden
        let names: Vec<(String, bool)> = doc.walk().iter().map(|(_, _, l)| (l.name.clone(), l.visible)).collect();
        assert!(names.iter().any(|(n, _)| n == "人物"), "thumb{n}: person layer");
        assert!(names.iter().any(|(n, _)| n == "背景"), "thumb{n}: background layer");
        assert!(names.iter().any(|(n, v)| n == "元画像（参照）" && !v), "thumb{n}: hidden reference layer");
        // 8. PNG export; 9. editable save + reload
        for f in [OutFormat::Png, OutFormat::Pcraft, OutFormat::Psd, OutFormat::Jpeg] {
            save(&doc, &out, &stem, f, true).expect("saves");
        }
        for ext in ["pcraft", "psd"] {
            let p = out.join(format!("{stem}.{ext}"));
            let back = open_document(&p).expect("reopens");
            let tl2 = type_layers(&back);
            assert_eq!(tl2.len(), tl.len(), "thumb{n}.{ext}: type layers survive");
            for (a, b) in tl.iter().zip(&tl2) {
                assert_eq!((&a.1, &a.2), (&b.1, &b.2), "thumb{n}.{ext}: text and font survive");
                assert!((a.3 - b.3).abs() < 0.5, "thumb{n}.{ext}: size survives");
            }
            // 4. rewrite the first text through the engine after the reload
            let mut s = photocraft_engine::Session::new();
            s.add_document(back, None);
            let r = s.execute("type.edit", json!({"layer": tl2[0].0, "text": "書き換えテスト"})).expect("type.edit");
            assert_eq!(r["text"], "書き換えテスト");
        }
        let flat = read_image(&out.join(format!("{stem}.png"))).unwrap();
        assert_eq!((flat.w, flat.h), (1280, 720));
    }
    eprintln!("lines {lines}: font {fonts_ok}, size {sizes_ok}, fill {fills_ok}");
    // Quality floor (measured 13/13 for all three when this test was written).
    assert!(fonts_ok * 10 >= lines * 9, "font family+weight on ≥90 % of lines");
    assert!(sizes_ok * 10 >= lines * 9, "size within 4 % on ≥90 % of lines");
    assert!(fills_ok * 10 >= lines * 9, "fill colour on ≥90 % of lines");
    let _ = std::fs::remove_dir_all(&out);
}
