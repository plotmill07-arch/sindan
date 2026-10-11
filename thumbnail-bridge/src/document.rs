//! Build the editable PhotoCraft document from a [`Decomposition`].
//!
//! Pixel layers (reference, background, person, decorations) are plain `photocraft-doc` data.
//! Text layers are made the way a user or agent would make them: through the engine's own
//! `type.create`, `type.edit` and `layer.layerStyle.replace` commands on a headless session, so
//! they get the engine's validation, defaults and caches and stay fully editable.
//!
//! Layer stack, bottom to top:
//! `元画像（参照）` (hidden, locked) · `背景` · `人物` · group `装飾` · group `テキスト`.

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerContent, LayerId};
use photocraft_engine::Session;
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::image::{Rgba8, hex};
use crate::pipeline::Decomposition;
use crate::{Error, Result};

pub const REFERENCE_LAYER: &str = "元画像（参照）";
pub const BACKGROUND_LAYER: &str = "背景";
pub const PERSON_LAYER: &str = "人物";
pub const DECOR_GROUP: &str = "装飾";
pub const TEXT_GROUP: &str = "テキスト";
/// Key of the bridge's record in `Document::metadata.text`.
pub const META_KEY: &str = "plotmill.thumbnail";

/// What the bridge records about a text layer (kept in the document so the UI can offer the
/// other font candidates later, also after save and reload).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TextRecord {
    pub layer: u64,
    pub name: String,
    pub ocr_text: String,
    pub text: String,
    pub confidence: f32,
    pub candidates: Vec<FontCandidate>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FontCandidate {
    pub family: String,
    pub style: String,
    pub weight: u16,
    pub size_pt: f32,
    pub tracking: f32,
    pub score: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Record {
    pub version: u32,
    pub source: String,
    pub reference_layer: Option<u64>,
    pub texts: Vec<TextRecord>,
    pub notes: Vec<String>,
}

/// Read the bridge's record back from a document.
pub fn record(doc: &Document) -> Option<Record> {
    doc.metadata.text.iter().rev().find(|(k, _)| k == META_KEY).and_then(|(_, v)| serde_json::from_str(v).ok())
}

fn surface(img: &Rgba8) -> Surface {
    let fmt = photocraft_color::PixelFormat::new(ColorMode::Rgb, SampleType::U8, true);
    let bytes: Vec<u8> = img.px.iter().flat_map(|p| p.iter().copied()).collect();
    let mut s = Surface::from_interleaved(fmt, Rect::new(0, 0, img.w as i32, img.h as i32), &bytes);
    s.prune();
    s
}

fn raster(name: &str, img: &Rgba8) -> Layer {
    Layer::new(name, LayerContent::Raster(surface(img)))
}

/// Pixels of `src` inside the soft `alpha` mask, cropped to nothing outside it.
fn cutout(src: &Rgba8, alpha: &[f32]) -> Rgba8 {
    let mut out = src.clone();
    for (p, a) in out.px.iter_mut().zip(alpha) {
        let a = a.clamp(0.0, 1.0);
        if a <= 0.004 {
            *p = [0, 0, 0, 0];
        } else {
            p[3] = (p[3] as f32 * a).round() as u8;
        }
    }
    out
}

fn run(s: &mut Session, id: &str, p: Value) -> Result<Value> {
    s.execute(id, p).map_err(|e| Error::Engine(format!("{id}: {e}")))
}

/// Build the layered document. `name` is the document title (usually the file stem).
pub fn build(name: &str, d: &Decomposition) -> Result<Document> {
    let (w, h) = (d.source.w, d.source.h);
    let mut doc = Document::new(name, Size::new(w as u32, h as u32), ColorMode::Rgb, SampleType::U8);
    doc.resolution_dpi = 72.0;

    let mut reference = raster(REFERENCE_LAYER, &d.source);
    reference.visible = false;
    reference.locks.all = true;
    let reference_id = reference.id;
    doc.layers.push(reference);
    doc.layers.push(raster(BACKGROUND_LAYER, &d.background));
    if let Some(p) = &d.person {
        doc.layers.push(raster(PERSON_LAYER, &cutout(&d.clean, &p.v)));
    }
    if !d.plates.is_empty() {
        let children: Vec<Layer> = d
            .plates
            .iter()
            .enumerate()
            .map(|(i, p)| raster(&format!("{} {}", p.kind.name(), i + 1), &cutout(&d.clean, &p.mask.v)))
            .collect();
        doc.layers.push(Layer::group(DECOR_GROUP, children));
    }

    // Text through engine commands.
    let mut session = Session::new();
    session.add_document(doc, None);
    let mut texts = Vec::new();
    for line in &d.lines {
        let (Some(st), Some(best)) = (&line.style, line.fits.first()) else { continue };
        let text = best.text.clone();
        let mut p = json!({
            "x": best.anchor[0],
            "y": best.anchor[1],
            "text": text,
            "font": best.face.family,
            "fontStyle": best.face.style,
            "size": best.size_px,
            "color": hex(st.fill),
            "tracking": best.tracking.round(),
            "orientation": if best.vertical { "vertical" } else { "horizontal" },
        });
        if best.face.style.is_empty() {
            p["weight"] = json!(best.face.weight);
        }
        let created = run(&mut session, "type.create", p)?;
        let Some(layer) = created["layer"].as_u64() else { continue };
        // Per-character colours.
        if st.runs.len() > 1 {
            let n = text.chars().count();
            let runs: Vec<Value> = st
                .runs
                .iter()
                .filter(|r| r.start < n)
                .map(|r| json!({"start": r.start, "end": if r.end >= line.ocr.text.chars().count() { n } else { r.end.min(n) }, "color": hex(r.color)}))
                .collect();
            run(&mut session, "type.edit", json!({"layer": layer, "runs": runs}))?;
        }
        if best.angle != 0.0 {
            let (s, c) = (best.angle as f64).sin_cos();
            let (x, y) = (best.anchor[0] as f64, best.anchor[1] as f64);
            run(&mut session, "type.edit", json!({"layer": layer, "transform": [c, s, -s, c, x, y]}))?;
        }
        let mut fx = Vec::new();
        if let Some(sk) = &st.stroke {
            fx.push(json!({"kind": "stroke", "params": {"size": sk.width, "position": "outside", "color": hex(sk.color), "opacity": 100, "blend": "normal"}}));
        }
        if let Some(sh) = &st.shadow {
            let dist = (sh.dx * sh.dx + sh.dy * sh.dy).sqrt();
            let angle = sh.dy.atan2(-sh.dx).to_degrees();
            fx.push(json!({"kind": "dropShadow", "params": {
                "color": hex(sh.color), "opacity": (sh.opacity * 100.0).round(), "blend": "multiply",
                "angle": angle.round(), "useGlobalLight": false, "distance": dist.round(), "spread": 0, "size": (sh.blur * 2.0).round()
            }}));
        }
        if let Some(g) = &st.glow {
            fx.push(json!({"kind": "outerGlow", "params": {
                "color": hex(g.color), "opacity": (g.opacity * 100.0).round(), "blend": "screen", "technique": "softer", "spread": 10, "size": g.size.round()
            }}));
        }
        if !fx.is_empty() {
            run(&mut session, "layer.layerStyle.replace", json!({"layer": layer, "effects": fx}))?;
        }
        let name = session.active().and_then(|s| s.doc.layer(LayerId(layer))).map(|l| l.name.clone()).unwrap_or_default();
        let candidates = line
            .fits
            .iter()
            .map(|f| FontCandidate {
                family: f.face.family.clone(),
                style: f.face.style.clone(),
                weight: f.face.weight,
                size_pt: f.size_px,
                tracking: f.tracking,
                score: f.score,
            })
            .collect();
        texts.push(TextRecord { layer, name, ocr_text: line.ocr.text.clone(), text, confidence: line.ocr.score, candidates });
    }

    let st = session.active().ok_or_else(|| Error::Engine("document vanished".into()))?;
    let mut doc: Document = (*st.doc).clone();
    // Gather the text layers (created at the top of the stack) into their group.
    let ids: Vec<LayerId> = texts.iter().map(|t| LayerId(t.layer)).collect();
    let mut text_layers = Vec::new();
    for id in &ids {
        if let Some(l) = doc.remove(*id) {
            text_layers.push(l);
        }
    }
    // Reading order is top to bottom; the Layers panel lists top first, so the first line ends
    // up on top.
    text_layers.reverse();
    if !text_layers.is_empty() {
        doc.layers.push(Layer::group(TEXT_GROUP, text_layers));
    }
    let rec = Record { version: 1, source: name.to_string(), reference_layer: Some(reference_id.0), texts, notes: d.notes.clone() };
    doc.metadata.text.push((META_KEY.to_string(), serde_json::to_string(&rec).map_err(|e| Error::Engine(e.to_string()))?));
    Ok(doc)
}
