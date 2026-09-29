//! HTTP(S) downloads for URL and Hugging Face model sources (feature `remote`).
//!
//! Files are streamed to disk (models are ~1 GB), never buffered in memory. A token in
//! `$HF_TOKEN` is sent only to `huggingface.co` for Hugging Face sources.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use crate::source::ModelSource;
use crate::store::StoreError;

/// Largest file accepted from a remote source (4 GiB), so a hostile server cannot fill the disk.
pub const MAX_FILE_BYTES: u64 = 4 << 30;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .user_agent(concat!("where-next/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Downloads `file` from `source` to `dest` (parent directories are created).
pub fn download(source: &ModelSource, file: &str, dest: &Path) -> Result<(), StoreError> {
    let url = source
        .file_url(file)
        .ok_or_else(|| StoreError::Fetch(format!("{file}: not a remote source")))?;
    let mut request = agent().get(&url);
    if let ModelSource::HuggingFace { .. } = source {
        if let Ok(token) = std::env::var("HF_TOKEN") {
            if !token.is_empty() {
                request = request.set("Authorization", &format!("Bearer {token}"));
            }
        }
    }
    let response = request.call().map_err(|e| match e {
        ureq::Error::Status(code, _) => {
            StoreError::Fetch(format!("{file}: HTTP {code} from {url}"))
        }
        other => StoreError::Fetch(format!("{file}: {other}")),
    })?;
    if let Some(len) = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
    {
        if len > MAX_FILE_BYTES {
            return Err(StoreError::Fetch(format!(
                "{file}: {len} bytes is too large"
            )));
        }
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::Fetch(e.to_string()))?;
    }
    let mut out = File::create(dest).map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
    let mut reader = io::Read::take(response.into_reader(), MAX_FILE_BYTES + 1);
    let copied =
        io::copy(&mut reader, &mut out).map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
    if copied > MAX_FILE_BYTES {
        let _ = fs::remove_file(dest);
        return Err(StoreError::Fetch(format!(
            "{file}: larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    out.flush()
        .map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
    Ok(())
}
