//! A verified ONNX model as a [`wn_core::encoder::Encoder`].
//!
//! Query text comes from [`wn_core::text::query_text`] (Qwen layout: instruction + task +
//! context tail). Families trained with other prompts swap the instruction for their own prefix.

use std::path::Path;
use std::sync::Mutex;

use wn_core::encoder::{EncodeError, Encoder, QueryInput};
use wn_core::text::{query_text, INSTRUCT_FILE, INSTRUCT_FUNCTION};

use crate::embedder::Embedder;
use crate::spec::{Family, ModelSpec};
use crate::store::{ModelSource, ModelStore};

/// Revision of the document builder (`wn-sources`). Part of the fingerprint: changing how
/// documents are built must invalidate stored vectors.
pub const DOC_REVISION: &str = "wn-sources-v1";

/// Model whose abstain thresholds were calibrated (see `wn_core::rank`).
pub const CALIBRATED_MODEL: &str = "v2b";

pub struct OnnxEncoder {
    embedder: Mutex<Embedder>,
    spec: ModelSpec,
    fingerprint: String,
    batch: usize,
}

/// The text a model of `spec`'s family embeds for `item`.
pub fn query_for(spec: &ModelSpec, item: &QueryInput) -> String {
    let full = query_text(&item.query, &item.context, item.granularity);
    match spec.family {
        Family::Qwen => full,
        Family::Gemma => {
            let body = full
                .strip_prefix(INSTRUCT_FILE)
                .or_else(|| full.strip_prefix(INSTRUCT_FUNCTION))
                .unwrap_or(&full);
            format!("{}{body}", spec.query_prefix)
        }
    }
}

impl OnnxEncoder {
    /// Verifies `dir` against its manifest, then loads it (`graph`: `model.onnx` or
    /// `model.int8.onnx`; default int8 when present).
    pub fn open(dir: &Path, graph: Option<&str>) -> Result<Self, String> {
        let mut store = ModelStore::new(dir, ModelSource::LocalDir(dir.to_path_buf()));
        store.ensure().map_err(|e| e.to_string())?;
        let manifest = store
            .manifest()
            .map(|m| m.fingerprint())
            .ok_or("model not verified")?;
        let embedder = Embedder::load(dir, graph).map_err(|e| e.to_string())?;
        let spec = embedder.spec().clone();
        let fingerprint = format!(
            "{}-{manifest}-{}-{DOC_REVISION}",
            spec.name,
            embedder.graph().trim_end_matches(".onnx")
        );
        Ok(Self {
            embedder: Mutex::new(embedder),
            spec,
            fingerprint,
            batch: 16,
        })
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    fn run(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
        let mut embedder = self
            .embedder
            .lock()
            .map_err(|_| EncodeError("embedder lock poisoned".into()))?;
        embedder
            .embed(texts, self.batch, None)
            .map_err(|e| EncodeError(e.to_string()))
    }
}

impl Encoder for OnnxEncoder {
    fn fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    fn calibrated(&self) -> bool {
        self.spec.name == CALIBRATED_MODEL
    }

    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
        let docs: Vec<String> = texts.iter().map(|t| self.spec.format_document(t)).collect();
        self.run(&docs)
    }

    fn queries(&self, items: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError> {
        let texts: Vec<String> = items.iter().map(|i| query_for(&self.spec, i)).collect();
        self.run(&texts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Pooling;
    use wn_core::text::Granularity;

    fn spec(family: Family, prefix: &str) -> ModelSpec {
        ModelSpec {
            name: "m".into(),
            family,
            pooling: Pooling::Mean,
            dim: 8,
            max_seq: 384,
            query_prefix: prefix.into(),
            document_prefix: String::new(),
            matryoshka: false,
        }
    }

    #[test]
    fn qwen_uses_the_granularity_instruction() {
        let s = spec(Family::Qwen, "ignored");
        let q = QueryInput::file("fix retry");
        assert_eq!(query_for(&s, &q), format!("{INSTRUCT_FILE}fix retry"));
        let f = QueryInput {
            granularity: Granularity::Function,
            ..q
        };
        assert_eq!(query_for(&s, &f), format!("{INSTRUCT_FUNCTION}fix retry"));
    }

    #[test]
    fn gemma_swaps_instruction_for_its_prefix() {
        let s = spec(Family::Gemma, "task: code retrieval | query: ");
        let q = QueryInput {
            query: "fix retry".into(),
            context: "Traceback".into(),
            granularity: Granularity::File,
        };
        assert_eq!(
            query_for(&s, &q),
            "task: code retrieval | query: fix retry\nRecent context:\nTraceback"
        );
    }
}
