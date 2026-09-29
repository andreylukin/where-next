//! Model store: puts model files in the cache directory, verifies them, and drives the
//! [`ModelLifecycle`] so callers can only embed with a verified model.

use std::fs;
use std::path::{Path, PathBuf};

use crate::lifecycle::{ModelEvent, ModelLifecycle, ModelState};
use crate::verify::{Manifest, VerifyError, MANIFEST_FILE};

/// Where model files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// A local directory containing the model files and a `wn-manifest.json`.
    LocalDir(PathBuf),
    /// A remote base URL (for example a Hugging Face repo). Not implemented yet.
    Url(String),
}

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
                fs::create_dir_all(&self.dir).map_err(|e| StoreError::Fetch(e.to_string()))?;
                for file in manifest
                    .files
                    .keys()
                    .map(String::as_str)
                    .chain([MANIFEST_FILE])
                {
                    fs::copy(src.join(file), self.dir.join(file))
                        .map_err(|e| StoreError::Fetch(format!("{file}: {e}")))?;
                }
                Ok(())
            }
            ModelSource::Url(url) => Err(StoreError::Fetch(format!(
                "remote model sources are not supported yet: {url}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn unsupported_url_fails_back_to_missing() {
        let cache = tempfile::tempdir().unwrap();
        let mut store = ModelStore::new(cache.path().join("m"), ModelSource::Url("hf://x".into()));
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
