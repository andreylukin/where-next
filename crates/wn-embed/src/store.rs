//! Model store: puts model files in the cache directory, verifies them, and drives the
//! [`ModelLifecycle`] so callers can only embed with a verified model.

use std::fs;
use std::path::{Path, PathBuf};

use crate::lifecycle::{ModelEvent, ModelLifecycle, ModelState};
use crate::verify::{Manifest, VerifyError, MANIFEST_FILE};

use crate::source::safe_file_name;
pub use crate::source::ModelSource;

/// Why the store could not produce a verified model.
#[derive(Debug)]
pub enum StoreError {
    Fetch(String),
    Verify(VerifyError),
    Illegal(crate::lifecycle::IllegalTransition),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Fetch(msg) => write!(f, "model fetch failed: {msg}"),
            StoreError::Verify(err) => write!(f, "model verification failed: {err}"),
            StoreError::Illegal(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<crate::lifecycle::IllegalTransition> for StoreError {
    fn from(err: crate::lifecycle::IllegalTransition) -> Self {
        StoreError::Illegal(err)
    }
}

/// A model directory under the cache, plus the lifecycle that guards it.
#[derive(Debug)]
pub struct ModelStore {
    dir: PathBuf,
    source: ModelSource,
    lifecycle: ModelLifecycle,
    manifest: Option<Manifest>,
}

fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(dir.join(MANIFEST_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

impl ModelStore {
    pub fn new(dir: impl Into<PathBuf>, source: ModelSource) -> Self {
        Self {
            dir: dir.into(),
            source,
            lifecycle: ModelLifecycle::default(),
            manifest: None,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn state(&self) -> ModelState {
        self.lifecycle.state()
    }

    /// The verified manifest, only available while [`ModelState::Loaded`].
    pub fn manifest(&self) -> Option<&Manifest> {
        self.lifecycle.state().can_embed().then_some(())?;
        self.manifest.as_ref()
    }

    /// Ensures a verified model is in the cache directory, fetching it if needed. Returns the
    /// resulting state; errors leave the lifecycle in `Missing` or `Corrupt`, never `Loaded`.
    pub fn ensure(&mut self) -> Result<ModelState, StoreError> {
        match self.state() {
            ModelState::Loaded => return Ok(ModelState::Loaded),
            ModelState::Missing if read_manifest(&self.dir).is_some() => {
                self.lifecycle.handle(ModelEvent::LocalFound)?;
            }
            ModelState::Missing | ModelState::Corrupt => {
                self.lifecycle.handle(ModelEvent::Fetch)?;
                match self.fetch() {
                    Ok(()) => self.lifecycle.handle(ModelEvent::DownloadDone)?,
                    Err(err) => {
                        self.lifecycle.handle(ModelEvent::DownloadFailed)?;
                        return Err(err);
                    }
                };
            }
            ModelState::Downloading | ModelState::Verifying => {}
        }
        self.verify()
    }

    /// Re-verifies after the files on disk changed (for example a model update).
    pub fn files_changed(&mut self) -> Result<ModelState, StoreError> {
        self.lifecycle.handle(ModelEvent::FilesChanged)?;
        self.verify()
    }

    /// Removes corrupt files so the next [`ModelStore::ensure`] starts clean.
    pub fn reset(&mut self) -> Result<ModelState, StoreError> {
        let state = self.lifecycle.handle(ModelEvent::Reset)?;
        let _ = fs::remove_dir_all(&self.dir);
        self.manifest = None;
        Ok(state)
    }

    fn verify(&mut self) -> Result<ModelState, StoreError> {
        let result = read_manifest(&self.dir)
            .ok_or(VerifyError::EmptyManifest)
            .and_then(|m| m.verify(&self.dir).map(|()| m));
        match result {
            Ok(manifest) => {
                self.manifest = Some(manifest);
                Ok(self.lifecycle.handle(ModelEvent::ChecksumOk)?)
            }
            Err(err) => {
                self.manifest = None;
                self.lifecycle.handle(ModelEvent::ChecksumMismatch)?;
                Err(StoreError::Verify(err))
            }
        }
    }

    fn fetch(&self) -> Result<(), StoreError> {
        match &self.source {
            ModelSource::LocalDir(src) => {
                let manifest = read_manifest(src)
                    .ok_or_else(|| StoreError::Fetch(format!("no {MANIFEST_FILE} in {src:?}")))?;
                check_names(&manifest)?;
                fs::create_dir_all(&self.dir).map_err(|e| StoreError::Fetch(e.to_string()))?;
                for file in manifest
                    .files
                    .keys()
                    .map(String::as_str)
                    .chain([MANIFEST_FILE])
                {
                    let dest = self.dir.join(file);
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).map_err(|e| StoreError::Fetch(e.to_string()))?;
                    }
                    fs::copy(src.join(file), dest)
                        .map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
                }
                Ok(())
            }
            remote => self.fetch_remote(remote),
        }
    }

    /// Downloads the manifest, then every listed file, into a sibling `.download` directory and
    /// renames it into place, so an interrupted download never leaves a half-filled model
    /// directory. Checksums are verified afterwards by the lifecycle (`Verifying`).
    #[cfg(feature = "remote")]
    fn fetch_remote(&self, source: &ModelSource) -> Result<(), StoreError> {
        let staging = staging_dir(&self.dir, source);
        fs::create_dir_all(&staging).map_err(|e| StoreError::Fetch(e.to_string()))?;
        let result = (|| {
            crate::remote::download(source, MANIFEST_FILE, &staging.join(MANIFEST_FILE))?;
            let manifest = read_manifest(&staging).ok_or_else(|| {
                StoreError::Fetch(format!(
                    "{MANIFEST_FILE} from {} is not valid",
                    source.describe()
                ))
            })?;
            check_names(&manifest)?;
            for file in manifest.files.keys() {
                crate::remote::download(source, file, &staging.join(file))?;
            }
            let _ = fs::remove_dir_all(&self.dir);
            if let Some(parent) = self.dir.parent() {
                fs::create_dir_all(parent).map_err(|e| StoreError::Fetch(e.to_string()))?;
            }
            fs::rename(&staging, &self.dir).map_err(|e| StoreError::Fetch(e.to_string()))
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    #[cfg(not(feature = "remote"))]
    fn fetch_remote(&self, source: &ModelSource) -> Result<(), StoreError> {
        Err(StoreError::Fetch(format!(
            "remote model sources need the `remote` feature: {}",
            source.describe()
        )))
    }
}

/// Rejects manifests that name files outside the model directory.
fn check_names(manifest: &Manifest) -> Result<(), StoreError> {
    match manifest.files.keys().find(|f| !safe_file_name(f)) {
        Some(bad) => Err(StoreError::Fetch(format!(
            "unsafe file name in manifest: {bad:?}"
        ))),
        None => Ok(()),
    }
}

#[cfg(feature = "remote")]
fn staging_dir(dir: &Path, source: &ModelSource) -> PathBuf {
    use sha2::{Digest, Sha256};
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    let revision = Sha256::digest(source.describe().as_bytes());
    name.push(format!(".download.{revision:x}"));
    dir.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "remote")]
    #[test]
    fn staging_is_keyed_by_revision() {
        let dir = Path::new("/tmp/model");
        let first = ModelSource::parse("hf:owner/model@revision-one").unwrap();
        let second = ModelSource::parse("hf:owner/model@revision-two").unwrap();
        assert_eq!(staging_dir(dir, &first), staging_dir(dir, &first));
        assert_ne!(staging_dir(dir, &first), staging_dir(dir, &second));
    }

    fn model_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            fs::write(dir.path().join(name), body).unwrap();
        }
        let names: Vec<&str> = files.iter().map(|(n, _)| *n).collect();
        let manifest = Manifest::compute(dir.path(), &names).unwrap();
        fs::write(
            dir.path().join(MANIFEST_FILE),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn local_source_is_copied_and_verified() {
        let src = model_dir(&[("model.onnx", "w"), ("tokenizer.json", "{}")]);
        let cache = tempfile::tempdir().unwrap();
        let mut store = ModelStore::new(
            cache.path().join("m"),
            ModelSource::LocalDir(src.path().into()),
        );
        assert_eq!(store.ensure().unwrap(), ModelState::Loaded);
        assert!(store.manifest().is_some());
        assert_eq!(store.ensure().unwrap(), ModelState::Loaded);
    }

    #[test]
    fn existing_cache_is_verified_without_fetching() {
        let dir = model_dir(&[("model.onnx", "w")]);
        let mut store = ModelStore::new(dir.path(), ModelSource::Url("unused".into()));
        assert_eq!(store.ensure().unwrap(), ModelState::Loaded);
    }

    #[test]
    fn tampered_cache_becomes_corrupt_and_never_loaded() {
        let dir = model_dir(&[("model.onnx", "w")]);
        fs::write(dir.path().join("model.onnx"), "tampered").unwrap();
        let mut store = ModelStore::new(dir.path(), ModelSource::Url("x".into()));
        assert!(matches!(store.ensure(), Err(StoreError::Verify(_))));
        assert_eq!(store.state(), ModelState::Corrupt);
        assert!(store.manifest().is_none());
    }

    #[test]
    fn unreachable_source_fails_back_to_missing() {
        let cache = tempfile::tempdir().unwrap();
        let mut store = ModelStore::new(
            cache.path().join("m"),
            ModelSource::Url("http://127.0.0.1:9/nothing".into()),
        );
        assert!(matches!(store.ensure(), Err(StoreError::Fetch(_))));
        assert_eq!(store.state(), ModelState::Missing);
    }

    #[test]
    fn corrupt_then_reset_then_refetch() {
        let src = model_dir(&[("model.onnx", "w")]);
        let cache = model_dir(&[("model.onnx", "w")]);
        fs::write(cache.path().join("model.onnx"), "bad").unwrap();
        let mut store = ModelStore::new(cache.path(), ModelSource::LocalDir(src.path().into()));
        assert!(store.ensure().is_err());
        // Corrupt re-fetches from the source on the next ensure.
        assert_eq!(store.ensure().unwrap(), ModelState::Loaded);
        store.files_changed().unwrap();
        assert_eq!(store.state(), ModelState::Loaded);
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn local_manifest_with_traversal_is_refused() {
        let src = tempfile::tempdir().unwrap();
        let mut files = std::collections::BTreeMap::new();
        files.insert("../escape".to_string(), "00".repeat(32));
        fs::write(
            src.path().join(MANIFEST_FILE),
            serde_json::to_string(&Manifest { files }).unwrap(),
        )
        .unwrap();
        let cache = tempfile::tempdir().unwrap();
        let mut store = ModelStore::new(
            cache.path().join("m"),
            ModelSource::LocalDir(src.path().into()),
        );
        let err = store.ensure().unwrap_err();
        assert!(err.to_string().contains("unsafe file name"), "{err}");
        assert_eq!(store.state(), ModelState::Missing);
    }
}
