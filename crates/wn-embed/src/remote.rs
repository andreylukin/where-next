//! HTTP(S) downloads for URL and Hugging Face model sources (feature `remote`).
//!
//! Files are streamed to disk (models are ~1 GB), never buffered in memory. A token in
//! `$HF_TOKEN` is sent only to `huggingface.co` for Hugging Face sources.

use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

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

/// Downloads `file` from `source` to `dest`, retaining a `.part` file on interruption.
pub fn download(source: &ModelSource, file: &str, dest: &Path) -> Result<(), StoreError> {
    let url = source
        .file_url(file)
        .ok_or_else(|| StoreError::Fetch(format!("{file}: not a remote source")))?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::Fetch(e.to_string()))?;
    }
    let part = dest.with_file_name(format!(
        "{}.part",
        dest.file_name().unwrap().to_string_lossy()
    ));
    let existing = fs::metadata(&part).map_or(0, |m| m.len());
    if existing > MAX_FILE_BYTES {
        return Err(StoreError::Fetch(format!(
            "{file}: partial file is too large"
        )));
    }
    let mut request = agent().get(&url);
    if existing > 0 {
        request = request.set("Range", &format!("bytes={existing}-"));
    }
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
    let resumed = existing > 0
        && response.status() == 206
        && response
            .header("Content-Range")
            .is_some_and(|range| range.starts_with(&format!("bytes {existing}-")));
    let offset = if resumed { existing } else { 0 };
    let length = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok());
    let total = length.and_then(|len| len.checked_add(offset));
    if total.is_some_and(|len| len > MAX_FILE_BYTES) {
        return Err(StoreError::Fetch(format!("{file}: file is too large")));
    }
    let mut out = OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&part)
        .map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
    let mut reader = response.into_reader();
    let mut buf = [0u8; 64 * 1024];
    let start = Instant::now();
    let mut last = Instant::now();
    let mut copied = 0u64;
    let tty = io::stderr().is_terminal();
    eprintln!(
        "downloading {file}: {} bytes{}",
        offset,
        total.map_or(String::new(), |n| format!(" / {n}"))
    );
    loop {
        let count = reader
            .read(&mut buf)
            .map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        if offset + copied > MAX_FILE_BYTES {
            drop(out);
            let _ = fs::remove_file(&part);
            return Err(StoreError::Fetch(format!(
                "{file}: larger than {MAX_FILE_BYTES} bytes"
            )));
        }
        out.write_all(&buf[..count])
            .map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
        if last.elapsed() >= Duration::from_secs(if tty { 1 } else { 10 }) {
            let rate = copied as f64 / start.elapsed().as_secs_f64().max(0.001);
            if tty {
                eprint!(
                    "\r{file}: {} / {} bytes, {:.1} MiB/s    ",
                    offset + copied,
                    total.map_or("?".into(), |n| n.to_string()),
                    rate / 1_048_576.0
                );
            } else {
                eprintln!(
                    "{file}: {} / {} bytes, {:.1} MiB/s",
                    offset + copied,
                    total.map_or("?".into(), |n| n.to_string()),
                    rate / 1_048_576.0
                );
            }
            last = Instant::now();
        }
    }
    out.flush()
        .map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
    if tty {
        eprintln!();
    }
    eprintln!(
        "{file}: {} / {} bytes downloaded",
        offset + copied,
        total.map_or("?".into(), |n| n.to_string())
    );
    fs::rename(&part, dest).map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
    Ok(())
}
