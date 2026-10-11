//! `plotmill`: command-line front end of the AI Thumbnail Bridge.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use plotmill_bridge::document::{self, record};
use plotmill_bridge::{Canvas, Engines, MAX_BATCH, OutFormat, Options, Stage, models, open_document, read_image, safe_stem, save};
use photocraft_doc::{Document, LayerContent};
use serde_json::{Value, json};

const USAGE: &str = "plotmill — Plotmill AI Thumbnail Bridge (PhotoCraft)

USAGE:
  plotmill convert <image>… --out <dir> [--formats pcraft,psd,png,jpg] [--canvas youtube|original]
                   [--no-person] [--no-decor] [--fonts <dir>] [--force]
      Decompose up to 4 thumbnails (PNG/JPEG/WebP saved from ChatGPT, …) into editable PhotoCraft
      documents. Each image is processed independently. Writes <name>.<ext> per format plus
      <name>.report.json into <dir>.
  plotmill inspect <file.pcraft|file.psd>
      List the layers, the editable text layers and their styles.
  plotmill edit-text <file> --layer <name or id> --text <new text> --out <file>
      Replace a text layer's text through PhotoCraft's type.edit command and save (round-trip check).
  plotmill export <file.pcraft|file.psd> --out <file.png|.jpg|.psd|.pcraft> [--force]
      Export a saved document.
  plotmill doctor
      Check the ONNX Runtime, the AI models and the Japanese fonts.

All processing is local. Nothing is uploaded and no API key is used.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("convert") => convert(&args[1..]),
        Some("inspect") => inspect(&args[1..]),
        Some("edit-text") => edit_text(&args[1..]),
        Some("export") => export(&args[1..]),
        Some("doctor") => doctor(),
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    pos: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(a: &[String], with_value: &[&str], switches: &[&str]) -> Result<Args, String> {
        let mut pos = Vec::new();
        let mut flags = Vec::new();
        let mut it = a.iter();
        while let Some(x) = it.next() {
            if with_value.contains(&x.as_str()) {
                let v = it.next().ok_or_else(|| format!("{x} needs a value"))?;
                flags.push((x.clone(), Some(v.clone())));
            } else if switches.contains(&x.as_str()) {
                flags.push((x.clone(), None));
            } else if x.starts_with("--") {
                return Err(format!("unknown option {x}"));
            } else {
                pos.push(x.clone());
            }
        }
        Ok(Args { pos, flags })
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.iter().rev().find(|f| f.0 == k).and_then(|f| f.1.as_deref())
    }
    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f.0 == k)
    }
}

fn e2s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn convert(a: &[String]) -> Result<(), String> {
    let a = Args::parse(a, &["--out", "--formats", "--canvas", "--fonts"], &["--no-person", "--no-decor", "--force"])?;
    if a.pos.is_empty() {
        return Err("convert needs at least one image".into());
    }
    if a.pos.len() > MAX_BATCH {
        return Err(format!("at most {MAX_BATCH} images per run (got {})", a.pos.len()));
    }
    let out = PathBuf::from(a.get("--out").ok_or("convert needs --out <dir>")?);
    std::fs::create_dir_all(&out).map_err(e2s)?;
    let formats: Vec<OutFormat> = a
        .get("--formats")
        .unwrap_or("pcraft,psd,png")
        .split(',')
        .map(|f| OutFormat::parse(f).ok_or_else(|| format!("unknown format `{f}`")))
        .collect::<Result<_, _>>()?;
    let canvas = match a.get("--canvas").unwrap_or("youtube") {
        "youtube" => Canvas::Youtube,
        "original" => Canvas::Original,
        c => return Err(format!("unknown canvas `{c}` (youtube|original)")),
    };
    let opts = Options { canvas, person: !a.has("--no-person"), decorations: !a.has("--no-decor"), ..Default::default() };
    if let Some(dir) = a.get("--fonts") {
        let files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(e2s)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| matches!(e.to_string_lossy().to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc")))
            .collect();
        let added = plotmill_bridge::fontmatch::register_font_files(&files);
        eprintln!("fonts: registered {} families from {dir}", added.len());
    }
    let t = std::time::Instant::now();
    let engines = Engines::load().map_err(e2s)?;
    eprintln!("models loaded in {:.1}s", t.elapsed().as_secs_f32());
    let cancel = AtomicBool::new(false);
    let mut failures = 0;
    for input in &a.pos {
        let path = Path::new(input);
        let stem = safe_stem(&plotmill_bridge::source::stem(path));
        eprintln!("== {input}");
        let t = std::time::Instant::now();
        let result = (|| -> Result<Value, String> {
            let img = read_image(path).map_err(e2s)?;
            let progress = |s: Stage| eprintln!("   {}", s.label_ja());
            let (doc, d) = plotmill_bridge::convert_image(&engines, &stem, &img, &opts, &progress, &cancel).map_err(e2s)?;
            let mut written = Vec::new();
            for f in &formats {
                let (p, warnings) = save(&doc, &out, &stem, *f, a.has("--force")).map_err(e2s)?;
                for w in warnings {
                    eprintln!("   warning ({}): {w}", f.ext());
                }
                written.push(p.display().to_string());
            }
            let report = report(&doc, &d, &written, t.elapsed().as_secs_f32());
            let rp = out.join(format!("{stem}.report.json"));
            if rp.exists() && !a.has("--force") {
                return Err(format!("{} exists (use --force)", rp.display()));
            }
            std::fs::write(&rp, serde_json::to_string_pretty(&report).map_err(e2s)?).map_err(e2s)?;
            Ok(report)
        })();
        match result {
            Ok(r) => {
                for t in r["texts"].as_array().into_iter().flatten() {
                    eprintln!("   text: {}  [{} {} {:.0}px, score {:.2}]", t["text"], t["font"], t["fontStyle"], t["sizePx"].as_f64().unwrap_or(0.0), t["score"].as_f64().unwrap_or(0.0));
                }
                eprintln!("   done in {:.1}s", t.elapsed().as_secs_f32());
            }
            Err(e) => {
                failures += 1;
                eprintln!("   failed: {e}");
            }
        }
    }
    if failures > 0 { Err(format!("{failures} image(s) failed")) } else { Ok(()) }
}

fn report(doc: &Document, d: &plotmill_bridge::Decomposition, written: &[String], secs: f32) -> Value {
    let texts: Vec<Value> = d
        .lines
        .iter()
        .filter_map(|l| {
            let st = l.style.as_ref()?;
            let best = l.fits.first()?;
            Some(json!({
                "ocrText": l.ocr.text,
                "text": best.text,
                "confidence": l.ocr.score,
                "font": best.face.family,
                "fontStyle": best.face.style,
                "weight": best.face.weight,
                "sizePx": best.size_px,
                "tracking": best.tracking,
                "score": best.score,
                "anchor": best.anchor,
                "angleDeg": best.angle.to_degrees(),
                "vertical": best.vertical,
                "fill": plotmill_bridge::image::hex(st.fill),
                "colorRuns": st.runs.iter().map(|r| json!({"start": r.start, "end": r.end, "color": plotmill_bridge::image::hex(r.color)})).collect::<Vec<_>>(),
                "stroke": st.stroke.as_ref().map(|s| json!({"width": s.width, "color": plotmill_bridge::image::hex(s.color)})),
                "shadow": st.shadow.as_ref().map(|s| json!({"dx": s.dx, "dy": s.dy, "blur": s.blur, "opacity": s.opacity, "color": plotmill_bridge::image::hex(s.color)})),
                "glow": st.glow.as_ref().map(|g| json!({"size": g.size, "color": plotmill_bridge::image::hex(g.color)})),
                "fillBox": [st.fill_bbox.x0, st.fill_bbox.y0, st.fill_bbox.x1, st.fill_bbox.y1],
                "candidates": l.fits.iter().map(|f| json!({"font": f.face.family, "fontStyle": f.face.style, "weight": f.face.weight, "score": f.score})).collect::<Vec<_>>(),
            }))
        })
        .collect();
    json!({
        "size": [d.source.w, d.source.h],
        "seconds": secs,
        "timings": d.timings.iter().map(|(s, t)| json!({"stage": format!("{s:?}"), "seconds": t})).collect::<Vec<_>>(),
        "person": d.person.is_some(),
        "decorations": d.plates.iter().map(|p| json!({"kind": p.kind.name(), "box": [p.bbox.x0, p.bbox.y0, p.bbox.x1, p.bbox.y1], "color": plotmill_bridge::image::hex(p.color), "outline": p.outline.map(|o| json!({"color": plotmill_bridge::image::hex(o.0), "width": o.1}))})).collect::<Vec<_>>(),
        "texts": texts,
        "notes": d.notes,
        "layers": layer_tree(doc),
        "files": written,
    })
}

fn layer_tree(doc: &Document) -> Vec<Value> {
    doc.walk()
        .iter()
        .rev()
        .map(|(_, depth, l)| {
            let mut v = json!({"name": l.name, "kind": l.content.kind_name(), "depth": depth, "visible": l.visible, "id": l.id.0});
            if let LayerContent::Text(t) = &l.content {
                let r = t.char_runs();
                let s = r.first().map(|r| r.style.clone()).unwrap_or_default();
                v["text"] = json!(t.text);
                v["font"] = json!(s.font_family);
                v["fontStyle"] = json!(s.font_style);
                v["sizePt"] = json!(s.size_pt);
                v["tracking"] = json!(s.tracking);
                v["runs"] = json!(r.len());
                v["effects"] = json!(l.effects.items.iter().map(|e| format!("{e:?}").split(['(', ' ', '{']).next().unwrap_or("").to_string()).collect::<Vec<_>>());
            }
            v
        })
        .collect()
}

fn inspect(a: &[String]) -> Result<(), String> {
    let a = Args::parse(a, &[], &[])?;
    let [file] = a.pos.as_slice() else { return Err("inspect needs one file".into()) };
    let doc = open_document(Path::new(file)).map_err(e2s)?;
    let v = json!({
        "size": [doc.size.width, doc.size.height],
        "layers": layer_tree(&doc),
        "bridge": record(&doc),
    });
    println!("{}", serde_json::to_string_pretty(&v).map_err(e2s)?);
    Ok(())
}

fn edit_text(a: &[String]) -> Result<(), String> {
    let a = Args::parse(a, &["--layer", "--text", "--out"], &["--force"])?;
    let [file] = a.pos.as_slice() else { return Err("edit-text needs one file".into()) };
    let which = a.get("--layer").ok_or("--layer required")?;
    let text = a.get("--text").ok_or("--text required")?;
    let out = PathBuf::from(a.get("--out").ok_or("--out required")?);
    let doc = open_document(Path::new(file)).map_err(e2s)?;
    let id = doc
        .walk()
        .iter()
        .find(|(_, _, l)| matches!(l.content, LayerContent::Text(_)) && (l.name == which || l.id.0.to_string() == which))
        .map(|(_, _, l)| l.id.0)
        .ok_or_else(|| format!("no text layer named `{which}`"))?;
    let mut s = photocraft_engine::Session::new();
    s.add_document(doc, None);
    let r = s.execute("type.edit", json!({"layer": id, "text": text})).map_err(e2s)?;
    eprintln!("type.edit → {r}");
    let doc = s.active().map(|d| (*d.doc).clone()).ok_or("no document")?;
    let dir = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let fmt = out.extension().and_then(|e| OutFormat::parse(&e.to_string_lossy())).ok_or("unknown output format")?;
    let (p, _) = save(&doc, dir, &stem, fmt, a.has("--force")).map_err(e2s)?;
    eprintln!("saved {}", p.display());
    Ok(())
}

fn export(a: &[String]) -> Result<(), String> {
    let a = Args::parse(a, &["--out"], &["--force"])?;
    let [file] = a.pos.as_slice() else { return Err("export needs one file".into()) };
    let out = PathBuf::from(a.get("--out").ok_or("--out required")?);
    let doc = open_document(Path::new(file)).map_err(e2s)?;
    let dir = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let fmt = out.extension().and_then(|e| OutFormat::parse(&e.to_string_lossy())).ok_or("unknown output format")?;
    let (p, w) = save(&doc, dir, &stem, fmt, a.has("--force")).map_err(e2s)?;
    for w in w {
        eprintln!("warning: {w}");
    }
    eprintln!("saved {}", p.display());
    Ok(())
}

fn doctor() -> Result<(), String> {
    for (k, v) in models::status() {
        println!("{k}: {v}");
    }
    let faces = plotmill_bridge::fontmatch::japanese_faces();
    println!("Japanese fonts: {} faces", faces.len());
    for f in &faces {
        println!("  {} / {} ({})", f.family, f.style, f.weight);
    }
    let _ = document::META_KEY;
    Ok(())
}
