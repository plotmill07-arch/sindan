//! Locating and loading the local AI models and the ONNX Runtime library.
//!
//! Nothing here downloads anything: `scripts/setup.sh` fetches the files once (with pinned
//! SHA-256 digests) and the bridge only reads them from disk. Search order for the model folder:
//! `$PLOTMILL_MODELS_DIR`, `<exe dir>/models`, `<exe dir>/../models`, `./models`,
//! `<crate>/models` (development builds). The ONNX Runtime shared library is taken from
//! `$PLOTMILL_ORT_LIB`, `$ORT_DYLIB_PATH`, or the model folder.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ort::session::Session;

use crate::{Error, Result};

/// A model file the bridge knows about.
#[derive(Clone, Copy, Debug)]
pub struct ModelFile {
    pub file: &'static str,
    pub sha256: &'static str,
    pub what: &'static str,
    pub license: &'static str,
    pub required: bool,
}

pub const OCR_DET: ModelFile = ModelFile {
    file: "PP-OCRv6_det_small.onnx",
    sha256: "090f04abcd9d9a7498bc4ebf677e4cb9bdce1fe4197ddb7e529f1ef44e1ff94f",
    what: "PaddleOCR PP-OCRv6 text detection (DBNet)",
    license: "Apache-2.0 (PaddleOCR, via RapidOCR 3.10.0 wheel)",
    required: true,
};
pub const OCR_REC: ModelFile = ModelFile {
    file: "PP-OCRv6_rec_small.onnx",
    sha256: "6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884",
    what: "PaddleOCR PP-OCRv6 text recognition (Japanese/Chinese/English, 18,708 characters)",
    license: "Apache-2.0 (PaddleOCR, via RapidOCR 3.10.0 wheel)",
    required: true,
};
pub const PERSON_SEG: ModelFile = ModelFile {
    file: "u2net_human_seg.onnx",
    sha256: "01eb6a29a5c4d8edb30b56adad9bb3a2a0535338e480724a213e0acfd2d1c73c",
    what: "U²-Net human segmentation (person cut-out)",
    license: "Apache-2.0 (U²-Net, Qin et al.; ONNX export from rembg, MIT)",
    required: false,
};

pub const ALL: [ModelFile; 3] = [OCR_DET, OCR_REC, PERSON_SEG];

/// Candidate model folders, in search order.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("PLOTMILL_MODELS_DIR").filter(|d| !d.is_empty()) {
        v.push(PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        v.push(dir.join("models"));
        v.push(dir.join("..").join("models"));
    }
    v.push(PathBuf::from("models"));
    v.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("models"));
    v
}

/// The first folder holding `m`.
pub fn find(m: &ModelFile) -> Option<PathBuf> {
    search_dirs().into_iter().map(|d| d.join(m.file)).find(|p| p.is_file())
}

fn ort_lib_names() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["onnxruntime.dll"]
    } else if cfg!(target_os = "macos") {
        &["libonnxruntime.dylib"]
    } else {
        &["libonnxruntime.so", "libonnxruntime.so.1"]
    }
}

/// Path of the ONNX Runtime shared library, if one can be found.
pub fn find_ort_lib() -> Option<PathBuf> {
    for var in ["PLOTMILL_ORT_LIB", "ORT_DYLIB_PATH"] {
        if let Some(p) = std::env::var_os(var).filter(|p| !p.is_empty()).map(PathBuf::from)
            && p.is_file()
        {
            return Some(p);
        }
    }
    for d in search_dirs() {
        for n in ort_lib_names() {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

static ORT_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Load ONNX Runtime once per process.
pub fn init_runtime() -> Result<()> {
    let r = ORT_INIT.get_or_init(|| {
        let lib = find_ort_lib().ok_or_else(|| {
            "ONNX Runtime library not found: run scripts/setup.sh, or set PLOTMILL_ORT_LIB to libonnxruntime".to_string()
        })?;
        let builder = ort::init_from(&lib).map_err(|e| format!("loading ONNX Runtime from {}: {e}", lib.display()))?;
        // `commit` returns false when an environment already exists (fine: it is reused).
        let _ = builder.with_name("plotmill").commit();
        Ok(())
    });
    r.clone().map_err(Error::Model)
}

/// Verify a file's SHA-256 against the pinned digest.
pub fn verify(path: &Path, m: &ModelFile) -> Result<bool> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or(&[]));
    }
    let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(got == m.sha256)
}

/// An ONNX session behind a mutex (sessions need `&mut` to run).
pub struct Model {
    pub session: Mutex<Session>,
    pub path: PathBuf,
}

impl Model {
    pub fn load(m: &ModelFile) -> Result<Model> {
        init_runtime()?;
        let path = find(m).ok_or_else(|| Error::Model(format!("model `{}` ({}) not found; run scripts/setup.sh", m.file, m.what)))?;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
        let session = Session::builder()
            .map_err(|e| Error::Model(e.to_string()))?
            .with_intra_threads(threads)
            .map_err(|e| Error::Model(e.to_string()))?
            .commit_from_file(&path)
            .map_err(|e| Error::Model(format!("{}: {e}", path.display())))?;
        Ok(Model { session: Mutex::new(session), path })
    }

    /// Run with one f32 NCHW input; returns the first output's shape and data.
    pub fn run(&self, shape: [usize; 4], data: Vec<f32>) -> Result<(Vec<usize>, Vec<f32>)> {
        let t = ort::value::Tensor::from_array((shape, data)).map_err(|e| Error::Model(e.to_string()))?;
        let mut s = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let out = s.run(ort::inputs![t]).map_err(|e| Error::Model(e.to_string()))?;
        let first = out.values().next().ok_or_else(|| Error::Model("model produced no output".into()))?;
        let (shape, data) = first.try_extract_tensor::<f32>().map_err(|e| Error::Model(e.to_string()))?;
        Ok((shape.iter().map(|&d| d.max(0) as usize).collect(), data.to_vec()))
    }

    /// A custom metadata entry (PP-OCR stores its character list as `character`).
    pub fn metadata(&self, key: &str) -> Option<String> {
        let s = self.session.lock().unwrap_or_else(|e| e.into_inner());
        s.metadata().ok().and_then(|m| m.custom(key))
    }
}

/// Status report for `plotmill doctor` and the UI.
pub fn status() -> Vec<(String, String)> {
    let mut v = Vec::new();
    v.push((
        "ONNX Runtime".to_string(),
        match find_ort_lib() {
            Some(p) => format!("ok: {}", p.display()),
            None => "missing (run scripts/setup.sh)".into(),
        },
    ));
    for m in ALL {
        let s = match find(&m) {
            Some(p) => match verify(&p, &m) {
                Ok(true) => format!("ok: {}", p.display()),
                Ok(false) => format!("checksum mismatch: {}", p.display()),
                Err(e) => format!("unreadable: {e}"),
            },
            None if m.required => "missing (required; run scripts/setup.sh)".into(),
            None => "missing (optional: no person cut-out)".into(),
        };
        v.push((format!("{} — {}", m.file, m.what), s));
    }
    v
}
