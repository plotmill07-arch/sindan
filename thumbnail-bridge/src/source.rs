//! Where thumbnails come from.
//!
//! The first version reads images the user saved from ChatGPT (or any other tool) to disk:
//! no account, no API key, nothing sent anywhere. [`ImageSource`] is the seam for later
//! sources. A direct OpenAI Images API connector would implement it, read its key from the
//! environment (`OPENAI_API_KEY`, never from source code or settings files) and only run when
//! the user explicitly asks for generation — it is deliberately *not* part of this crate yet.
//! ChatGPT (the app) and the OpenAI API are different products; saving from ChatGPT needs no API.

use std::path::{Path, PathBuf};

use crate::{MAX_BATCH, Result, Rgba8, read_image};

/// A named image to decompose.
pub struct SourceImage {
    pub name: String,
    pub image: Rgba8,
}

pub trait ImageSource {
    /// Human-readable name ("Local files").
    fn label(&self) -> &str;
    /// Fetch up to [`MAX_BATCH`] images.
    fn images(&self) -> Result<Vec<SourceImage>>;
}

/// Images picked from disk.
pub struct LocalFiles {
    pub paths: Vec<PathBuf>,
}

impl ImageSource for LocalFiles {
    fn label(&self) -> &str {
        "Local files"
    }
    fn images(&self) -> Result<Vec<SourceImage>> {
        self.paths
            .iter()
            .take(MAX_BATCH)
            .map(|p| {
                Ok(SourceImage { name: stem(p), image: read_image(p)? })
            })
            .collect()
    }
}

pub fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "thumbnail".into())
}
