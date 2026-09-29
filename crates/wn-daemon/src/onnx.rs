//! Production [`Encoder`]: a verified ONNX model from `wn-embed`.

use std::path::Path;

use wn_embed::embedder::Embedder;
use wn_embed::store::{ModelSource, ModelStore};

use crate::engine::Encoder;

/// Revision of the document builder (`wn-sources` skeletons). Part of the index key: changing
/// how documents are built must invalidate stored vectors.
pub const DOC_REVISION: &str = "skeleton-v1";

pub struct OnnxEncoder {
    embedder: Embedder,
    fingerprint: String,
    name: String,
    batch: usize,
}

impl OnnxEncoder {
    /// Verifies the model directory against its manifest, then loads it. `graph` picks
    /// `model.onnx` or `model.int8.onnx` (default: int8 when present).
    pub fn open(dir: &Path, graph: Option<&str>) -> Result<Self, String> {
        let mut store = ModelStore::new(dir, ModelSource::LocalDir(dir.to_path_buf()));
        store.ensure().map_err(|e| e.to_string())?;
        let manifest_fp = store
            .manifest()
            .map(|m| m.fingerprint())
            .ok_or("model not verified")?;
        let embedder = Embedder::load(dir, graph).map_err(|e| e.to_string())?;
        let name = format!("{} ({})", embedder.spec().name, embedder.graph());
        let fingerprint = format!("{manifest_fp}-{}-{DOC_REVISION}", embedder.graph());
        Ok(Self {
            embedder,
            fingerprint,
            name,
            batch: 16,
        })
    }
}

impl Encoder for OnnxEncoder {
    fn dim(&self) -> usize {
        self.embedder.spec().dim
    }

    fn fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn encode_documents(&mut self, docs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let spec = self.embedder.spec().clone();
        let texts: Vec<String> = docs.iter().map(|d| spec.format_document(d)).collect();
        self.embedder
            .embed(&texts, self.batch, None)
            .map_err(|e| e.to_string())
    }

    fn encode_query(&mut self, query: &str, context: &str) -> Result<Vec<f32>, String> {
        let text = self.embedder.spec().format_query(query, context);
        self.embedder
            .embed(&[text], 1, None)
            .map_err(|e| e.to_string())?
            .pop()
            .ok_or_else(|| "empty embedding".into())
    }
}
