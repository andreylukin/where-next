//! Model manifests: the expected SHA-256 of every model file, and verification against it.
//!
//! A model directory is only used after every listed file hashes to its expected value. The
//! manifest also yields the model fingerprint that keys the vector cache, so replacing weights at
//! the same path can never reuse incompatible document vectors.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// File name of the manifest inside a model directory.
pub const MANIFEST_FILE: &str = "wn-manifest.json";

/// Expected SHA-256 (lowercase hex) of each model file, by path relative to the model directory.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub files: BTreeMap<String, String>,
}

/// Why a model directory failed verification.
#[derive(Debug)]
pub enum VerifyError {
    /// A listed file does not exist.
    Missing(String),
    /// A listed file exists but hashes to something else.
    Mismatch {
        file: String,
        expected: String,
        actual: String,
    },
    /// The manifest lists no files, so nothing can be trusted.
    EmptyManifest,
    Io(io::Error),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::Missing(file) => write!(f, "model file missing: {file}"),
            VerifyError::Mismatch {
                file,
                expected,
                actual,
            } => write!(
                f,
                "checksum mismatch for {file}: expected {expected}, got {actual}"
            ),
            VerifyError::EmptyManifest => write!(f, "manifest lists no files"),
            VerifyError::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for VerifyError {}

impl From<io::Error> for VerifyError {
    fn from(err: io::Error) -> Self {
        VerifyError::Io(err)
    }
}

/// SHA-256 of a file, streamed (model files can be gigabytes).
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Manifest {
    /// Hashes the given files (relative to `dir`) into a new manifest.
    pub fn compute(dir: &Path, files: &[&str]) -> io::Result<Manifest> {
        let mut out = BTreeMap::new();
        for file in files {
            out.insert((*file).to_string(), sha256_file(&dir.join(file))?);
        }
        Ok(Manifest { files: out })
    }

    /// Checks every listed file. Stops at the first problem.
    pub fn verify(&self, dir: &Path) -> Result<(), VerifyError> {
        if self.files.is_empty() {
            return Err(VerifyError::EmptyManifest);
        }
        for (file, expected) in &self.files {
            let path = dir.join(file);
            if !path.is_file() {
                return Err(VerifyError::Missing(file.clone()));
            }
            let actual = sha256_file(&path)?;
            if &actual != expected {
                return Err(VerifyError::Mismatch {
                    file: file.clone(),
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Stable fingerprint of the model: a hash over every (file, checksum) pair. Keys the vector
    /// cache together with the document-builder revision.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        for (file, sum) in &self.files {
            hasher.update(file.as_bytes());
            hasher.update(b"\0");
            hasher.update(sum.as_bytes());
            hasher.update(b"\n");
        }
        hex(&hasher.finalize())[..16].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            fs::write(dir.path().join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn known_sha256() {
        let dir = dir_with(&[("a.txt", "abc")]);
        assert_eq!(
            sha256_file(&dir.path().join("a.txt")).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_accepts_matching_files() {
        let dir = dir_with(&[("model.onnx", "weights"), ("tokenizer.json", "{}")]);
        let manifest = Manifest::compute(dir.path(), &["model.onnx", "tokenizer.json"]).unwrap();
        manifest.verify(dir.path()).unwrap();
    }

    #[test]
    fn verify_rejects_tampered_and_missing_files() {
        let dir = dir_with(&[("model.onnx", "weights")]);
        let manifest = Manifest::compute(dir.path(), &["model.onnx"]).unwrap();
        fs::write(dir.path().join("model.onnx"), "other weights").unwrap();
        assert!(matches!(
            manifest.verify(dir.path()),
            Err(VerifyError::Mismatch { .. })
        ));
        fs::remove_file(dir.path().join("model.onnx")).unwrap();
        assert!(matches!(
            manifest.verify(dir.path()),
            Err(VerifyError::Missing(_))
        ));
    }

    #[test]
    fn empty_manifest_is_never_trusted() {
        let dir = dir_with(&[]);
        assert!(matches!(
            Manifest::default().verify(dir.path()),
            Err(VerifyError::EmptyManifest)
        ));
    }

    #[test]
    fn fingerprint_changes_with_any_checksum() {
        let dir = dir_with(&[("m", "1")]);
        let a = Manifest::compute(dir.path(), &["m"]).unwrap();
        fs::write(dir.path().join("m"), "2").unwrap();
        let b = Manifest::compute(dir.path(), &["m"]).unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint(), a.clone().fingerprint());
    }
}
