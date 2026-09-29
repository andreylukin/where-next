//! A verified ONNX model as a [`wn_core::encoder::Encoder`].
//!
//! Query text comes from [`wn_core::text::query_text_for`] in the model's `query_format` (v1:
//! task + context tail; v2: request, last tool output, earlier context), with the Qwen
//! instruction; families trained with other prompts swap the instruction for their prefix.

use std::path::Path;
use std::sync::Mutex;

use wn_core::encoder::{EncodeError, Encoder, QueryInput};
use wn_core::rank::Calibration;
use wn_core::text::{
    fit_query, query_text_for, QueryFormat, TokenCounter, INSTRUCT_FILE, INSTRUCT_FUNCTION,
};

use crate::embedder::Embedder;
use crate::spec::{Family, ModelSpec};
use crate::store::{ModelSource, ModelStore};

/// Revision of the document builder (`wn-sources`). Part of the fingerprint: changing how
/// documents are built must invalidate stored vectors.
pub const DOC_REVISION: &str = "wn-sources-v1";

/// Abstain calibration shipped next to a model (see [`wn_core::rank::Calibration`]).
pub const CALIBRATION_FILE: &str = "calibration.json";

pub struct OnnxEncoder {
    embedder: Mutex<Embedder>,
    spec: ModelSpec,
    fingerprint: String,
    batch: usize,
    calibration: Option<Calibration>,
}

/// The calibration for the model in `dir`: its `calibration.json`, else the built-in one for
/// the reference model `v2b`, else none (the model never abstains).
pub fn load_calibration(dir: &Path, spec: &ModelSpec) -> Result<Option<Calibration>, String> {
    let path = dir.join(CALIBRATION_FILE);
    if path.is_file() {
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let c: Calibration =
            serde_json::from_str(&text).map_err(|e| format!("{CALIBRATION_FILE}: {e}"))?;
        return Ok(Some(c));
    }
    Ok((spec.name == "v2b").then(Calibration::v2b))
}

/// The text a model of `spec`'s family embeds for `item` (no token budget applied).
pub fn query_for(spec: &ModelSpec, item: &QueryInput) -> String {
    let full = query_text_for(
        spec.query_format,
        &item.query,
        &item.context,
        item.granularity,
    );
    with_family_prefix(spec, full)
}

/// Swaps the Qwen instruction for the family's own query prefix.
fn with_family_prefix(spec: &ModelSpec, full: String) -> String {
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
    /// `model.q8.onnx`; default fp32 when present).
    pub fn open(dir: &Path, graph: Option<&str>) -> Result<Self, String> {
        let mut store = ModelStore::new(dir, ModelSource::LocalDir(dir.to_path_buf()));
        store.ensure().map_err(|e| e.to_string())?;
        let manifest = store
            .manifest()
            .map(|m| m.fingerprint())
            .ok_or("model not verified")?;
        let embedder = Embedder::load(dir, graph).map_err(|e| e.to_string())?;
        let spec = embedder.spec().clone();
        let calibration = load_calibration(dir, &spec)?;
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
            calibration,
        })
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    /// The query text within the model's token window. v2 queries already put the least useful
    /// part last, so truncation is safe; v1 queries end with the newest context, so the oldest
    /// context is dropped until the text fits instead of letting the tokenizer cut the newest.
    pub fn query_text(&self, item: &QueryInput) -> String {
        if self.spec.query_format != QueryFormat::V1 || item.context.is_empty() {
            return query_for(&self.spec, item);
        }
        let Ok(embedder) = self.embedder.lock() else {
            return query_for(&self.spec, item);
        };
        let (text, _) = fit_query(
            Some(&Counter(&embedder)),
            &item.query,
            &item.context,
            item.granularity,
            self.spec.max_seq,
        );
        with_family_prefix(&self.spec, text)
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

    fn calibration(&self) -> Option<Calibration> {
        self.calibration.clone()
    }

    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
        let docs: Vec<String> = texts.iter().map(|t| self.spec.format_document(t)).collect();
        self.run(&docs)
    }

    fn queries(&self, items: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError> {
        let texts: Vec<String> = items.iter().map(|i| self.query_text(i)).collect();
        self.run(&texts)
    }
}

struct Counter<'a>(&'a Embedder);

impl TokenCounter for Counter<'_> {
    fn count_tokens(&self, text: &str) -> usize {
        self.0.count_tokens(text)
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
            query_format: Default::default(),
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
    fn calibration_file_wins_and_v2b_has_a_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = spec(Family::Gemma, "p");
        assert_eq!(load_calibration(dir.path(), &s).unwrap(), None);
        s.name = "v2b".into();
        assert_eq!(
            load_calibration(dir.path(), &s).unwrap(),
            Some(Calibration::v2b())
        );
        std::fs::write(
            dir.path().join(CALIBRATION_FILE),
            r#"{"model": "v2b", "kinds": {}}"#,
        )
        .unwrap();
        let c = load_calibration(dir.path(), &s).unwrap().unwrap();
        assert!(c.kinds.is_empty());
        std::fs::write(dir.path().join(CALIBRATION_FILE), "not json").unwrap();
        assert!(load_calibration(dir.path(), &s).is_err());
    }

    #[test]
    fn v2_models_get_the_v2_layout() {
        let mut s = spec(Family::Gemma, "task: code retrieval | query: ");
        s.query_format = crate::spec::QueryFormat::V2;
        let q = QueryInput {
            query: "fix retry".into(),
            context: "opened a.py\nLast tool output:\nTraceback".into(),
            granularity: Granularity::File,
        };
        assert_eq!(
            query_for(&s, &q),
            "task: code retrieval | query: fix retry\nLast tool output:\nTraceback\nEarlier context:\nopened a.py"
        );
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
