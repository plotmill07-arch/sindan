//! Plotmill AI Thumbnail Bridge.
//!
//! Turns a flat thumbnail image (for example a PNG saved from ChatGPT) into a layered, editable
//! PhotoCraft document: background, person cut-out, decorations and **editable Japanese text
//! layers** whose font, size, spacing, colour, outline and shadow are estimated from the image.
//! Everything runs locally: no image ever leaves the machine and no API key is needed.
//!
//! Entry points: [`pipeline::Engines::load`] once, then [`convert_image`] per image (or the
//! lower-level [`pipeline::decompose`] + [`document::build`]). [`source`] holds the extension
//! point for image sources other than local files (e.g. a future OpenAI Images connector).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo)]

pub mod decor;
pub mod document;
pub mod fontmatch;
pub mod image;
pub mod inpaint;
pub mod models;
pub mod ocr;
pub mod pipeline;
pub mod segment;
pub mod source;
pub mod style;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use photocraft_doc::Document;

pub use image::Rgba8;
pub use pipeline::{Canvas, Decomposition, Engines, Options, Stage};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Input(String),
    Model(String),
    Engine(String),
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Input(s) => write!(f, "input: {s}"),
            Error::Model(s) => write!(f, "AI model: {s}"),
            Error::Engine(s) => write!(f, "PhotoCraft engine: {s}"),
            Error::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Largest input file read (AI thumbnails are a few MB).
pub const MAX_INPUT_BYTES: u64 = 64 << 20;
/// Most images taken in one batch.
pub const MAX_BATCH: usize = 4;
/// Image types accepted as thumbnails.
pub const INPUT_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp"];

/// Decode image bytes (any format PhotoCraft reads) into RGBA8.
pub fn decode_image(name: &str, bytes: &[u8]) -> Result<Rgba8> {
    let opened = photocraft_automation::files::open_bytes(name, bytes).map_err(|e| Error::Input(e.to_string()))?;
    Ok(flatten(&opened.document))
}

/// The document's composite as RGBA8.
pub fn flatten(doc: &Document) -> Rgba8 {
    let side = doc.size.width.max(doc.size.height).max(1);
    let t = photocraft_compose::thumbnail(doc, side);
    let px = t.pixels.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    Rgba8 { w: t.width as usize, h: t.height as usize, px }
}

/// Read a thumbnail from disk, with the bridge's input checks (regular file, image extension,
/// size cap).
pub fn read_image(path: &Path) -> Result<Rgba8> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if !INPUT_EXTENSIONS.contains(&ext.as_str()) {
        return Err(Error::Input(format!("{}: not a PNG/JPEG/WebP image", path.display())));
    }
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(Error::Input(format!("{}: not a regular file", path.display())));
    }
    if meta.len() > MAX_INPUT_BYTES {
        return Err(Error::Input(format!("{}: larger than {} MiB", path.display(), MAX_INPUT_BYTES >> 20)));
    }
    let bytes = std::fs::read(path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    decode_image(&name, &bytes)
}

/// Decompose and build in one go.
pub fn convert_image(engines: &Engines, name: &str, img: &Rgba8, opts: &Options, progress: &(dyn Fn(Stage) + Sync), cancel: &AtomicBool) -> Result<(Document, Decomposition)> {
    let d = pipeline::decompose(engines, img, opts, progress, cancel)?;
    let doc = document::build(name, &d)?;
    Ok((doc, d))
}

/// Output formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutFormat {
    Pcraft,
    Psd,
    Png,
    Jpeg,
}

impl OutFormat {
    pub fn ext(&self) -> &'static str {
        match self {
            OutFormat::Pcraft => "pcraft",
            OutFormat::Psd => "psd",
            OutFormat::Png => "png",
            OutFormat::Jpeg => "jpg",
        }
    }
    pub fn parse(s: &str) -> Option<OutFormat> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pcraft" => Some(OutFormat::Pcraft),
            "psd" => Some(OutFormat::Psd),
            "png" => Some(OutFormat::Png),
            "jpg" | "jpeg" => Some(OutFormat::Jpeg),
            _ => None,
        }
    }
}

/// A file-name stem safe to use inside an output folder (no separators, no dot-dot, bounded).
pub fn safe_stem(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_alphanumeric() || "-_ ()（）".contains(c) { c } else { '_' }).take(80).collect();
    let t = t.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if t.is_empty() { "thumbnail".into() } else { t }
}

/// Write `doc` as `<dir>/<stem>.<ext>`. Only writes inside `dir`; refuses to replace an existing
/// file unless `overwrite`. Returns the path and any format warnings.
pub fn save(doc: &Document, dir: &Path, stem: &str, fmt: OutFormat, overwrite: bool) -> Result<(PathBuf, Vec<String>)> {
    let path = dir.join(format!("{}.{}", safe_stem(stem), fmt.ext()));
    if path.parent() != Some(dir) {
        return Err(Error::Input("output name escapes the output folder".into()));
    }
    if path.exists() && !overwrite {
        return Err(Error::Input(format!("{} exists (use --force to replace it)", path.display())));
    }
    let mut opts = photocraft_io::ExportOptions::default();
    opts.encode.jpeg_quality = 92;
    let w = photocraft_automation::files::save(doc, &path, None, &opts, None).map_err(|e| Error::Engine(e.to_string()))?;
    Ok((path, w))
}

/// Open a saved document (`.pcraft`, `.psd`, …).
pub fn open_document(path: &Path) -> Result<Document> {
    photocraft_automation::files::open(path).map(|o| o.document).map_err(|e| Error::Input(e.to_string()))
}
