//! Model description (`wn-model.json`) and the exact query/document text each model family expects.
//!
//! Query text (task + recent context) is built by `wn_core::text`; this module only knows each
//! family's prefixes, which must match what the model saw in training byte for byte.

use serde::{Deserialize, Serialize};
pub use wn_core::text::QueryFormat;

/// Model family: decides pooling expectations and Matryoshka support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Qwen,
    Gemma,
}

/// How token vectors were pooled inside the exported graph (informational: the graph already
/// returns pooled, normalised embeddings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pooling {
    Mean,
    Lasttoken,
}

/// Contents of `wn-model.json`, written next to the exported ONNX graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub name: String,
    pub family: Family,
    pub pooling: Pooling,
    pub dim: usize,
    pub max_seq: usize,
    /// Query layout the model was trained on (`v1` when absent).
    #[serde(default)]
    pub query_format: QueryFormat,
    pub query_prefix: String,
    pub document_prefix: String,
    #[serde(default)]
    pub matryoshka: bool,
}

/// Matryoshka dimensions EmbeddingGemma was trained to support.
pub const GEMMA_MRL_DIMS: [usize; 4] = [768, 512, 256, 128];

impl ModelSpec {
    /// Parses `wn-model.json`.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// Full document text for a candidate document as built by `wn-sources`
    /// (`file: <skeleton>` or `function: <path>::<name>`): the family prefix plus that text.
    pub fn format_document(&self, doc_text: &str) -> String {
        format!("{}{}", self.document_prefix, doc_text)
    }

    /// Output dimensions this model may be truncated to.
    pub fn allowed_dims(&self) -> Vec<usize> {
        if self.matryoshka && self.family == Family::Gemma {
            GEMMA_MRL_DIMS
                .into_iter()
                .filter(|d| *d <= self.dim)
                .collect()
        } else {
            vec![self.dim]
        }
    }
}

/// Keeps the first `dim` values and re-normalises to unit length (Matryoshka truncation).
pub fn truncate_normalize(vector: &[f32], dim: usize) -> Vec<f32> {
    let mut out: Vec<f32> = vector.iter().take(dim).copied().collect();
    let norm = out.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        out.iter_mut().for_each(|x| *x /= norm);
    }
    out
}

/// Cosine similarity of two unit vectors (a dot product).
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn qwen() -> ModelSpec {
        ModelSpec {
            name: "v2b".into(),
            family: Family::Qwen,
            pooling: Pooling::Lasttoken,
            dim: 1024,
            max_seq: 384,
            query_format: QueryFormat::V1,
            query_prefix: "Instruct: Given a coding task and recent context, find the file that must be read or edited next\nQuery: ".into(),
            document_prefix: String::new(),
            matryoshka: false,
        }
    }

    #[test]
    fn document_uses_family_prefix() {
        let mut gemma = qwen();
        gemma.document_prefix = "title: none | text: ".into();
        assert_eq!(
            gemma.format_document("file: src/a.rs\nfn main"),
            "title: none | text: file: src/a.rs\nfn main"
        );
    }

    #[test]
    fn gemma_allows_matryoshka_dims() {
        let mut gemma = qwen();
        gemma.family = Family::Gemma;
        gemma.dim = 768;
        gemma.matryoshka = true;
        assert_eq!(gemma.allowed_dims(), vec![768, 512, 256, 128]);
        assert_eq!(qwen().allowed_dims(), vec![1024]);
    }

    #[test]
    fn spec_parses_exported_json() {
        let json = r#"{"name":"gemma-g1","family":"gemma","pooling":"mean","dim":768,"max_seq":384,
            "padding":"right","query_prefix":"task: code retrieval | query: ",
            "document_prefix":"title: none | text: ","inputs":["input_ids","attention_mask"],
            "output":"embeddings","normalized":true,"matryoshka":true}"#;
        let spec = ModelSpec::from_json(json).unwrap();
        assert_eq!(spec.family, Family::Gemma);
        assert_eq!(spec.pooling, Pooling::Mean);
        assert!(spec.matryoshka);
        assert_eq!(spec.query_format, QueryFormat::V1, "absent means v1");
        let v2 = json.replace(
            "\"max_seq\":384",
            "\"max_seq\":1024,\"query_format\":\"v2\"",
        );
        assert_eq!(
            ModelSpec::from_json(&v2).unwrap().query_format,
            QueryFormat::V2
        );
    }

    proptest! {
        #[test]
        fn truncation_yields_unit_vectors(v in prop::collection::vec(-1.0f32..1.0, 768), d in 1usize..768) {
            let t = truncate_normalize(&v, d);
            prop_assert_eq!(t.len(), d);
            let n: f32 = t.iter().map(|x| x * x).sum();
            prop_assert!(n == 0.0 || (n - 1.0).abs() < 1e-4);
        }
    }
}
