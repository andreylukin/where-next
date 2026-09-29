//! Model description (`wn-model.json`) and the exact query/document text each model family expects.
//!
//! The text builders must match what the model saw in training byte for byte; the reference
//! implementation is the Python `nav2.query_text` / `nav2.doc_text` pair in the research repo.
//! Lengths are counted in Unicode scalar values, like Python string slicing.

use serde::{Deserialize, Serialize};

/// Characters of query plus context kept before prefixing (matches training).
pub const QUERY_LIMIT: usize = 2400;

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
    pub query_prefix: String,
    pub document_prefix: String,
    #[serde(default)]
    pub matryoshka: bool,
}

/// Matryoshka dimensions EmbeddingGemma was trained to support.
pub const GEMMA_MRL_DIMS: [usize; 4] = [768, 512, 256, 128];

fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn last_chars(s: &str, n: usize) -> &str {
    let len = s.chars().count();
    if n >= len {
        return s;
    }
    match s.char_indices().nth(len - n) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

/// The query body shared by every family: the query, then the tail of the recent context, within
/// [`QUERY_LIMIT`] characters. Mirrors Python `nav2.query_text` without the instruction prefix.
pub fn query_body(query: &str, context: &str) -> String {
    let query = query.trim();
    let context = context.trim();
    let limit = QUERY_LIMIT;
    if context.is_empty() {
        return take_chars(query, limit).to_string();
    }
    let qlen = query.chars().count();
    let clen = context.chars().count();
    let room = 400.max(limit - qlen.min(limit / 2));
    let keep = limit - room.min(clen);
    format!(
        "{}\nRecent context:\n{}",
        take_chars(query, keep),
        last_chars(context, room)
    )
}

impl ModelSpec {
    /// Parses `wn-model.json`.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// Full query text for this model.
    pub fn format_query(&self, query: &str, context: &str) -> String {
        format!("{}{}", self.query_prefix, query_body(query, context))
    }

    /// Full document text for a file skeleton (path, first doc line, symbol names).
    pub fn format_document(&self, skeleton: &str) -> String {
        format!("{}{}", self.document_prefix, skeleton)
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
            query_prefix: "Instruct: Given a coding task and recent context, find the file that must be read or edited next\nQuery: ".into(),
            document_prefix: "file: ".into(),
            matryoshka: false,
        }
    }

    #[test]
    fn query_without_context_matches_python() {
        assert_eq!(
            qwen().format_query("  fix the retry loop ", ""),
            "Instruct: Given a coding task and recent context, find the file that must be read or edited next\nQuery: fix the retry loop"
        );
    }

    #[test]
    fn query_with_context_matches_python() {
        // Python: nav2.query_text("q"*3000, "c"*3000) keeps 1200 query chars and the last 1200
        // context chars (room = max(400, 2400 - min(3000, 1200)) = 1200).
        let body = query_body(&"q".repeat(3000), &"c".repeat(3000));
        let (q, c) = body.split_once("\nRecent context:\n").unwrap();
        assert_eq!(q.len(), 1200);
        assert_eq!(c.len(), 1200);
        // Short query, long context: room = 2400 - 10 = 2390; query kept whole.
        let body = query_body("short fix!", &"c".repeat(5000));
        let (q, c) = body.split_once("\nRecent context:\n").unwrap();
        assert_eq!((q, c.len()), ("short fix!", 2390));
    }

    #[test]
    fn slicing_counts_characters_not_bytes() {
        let body = query_body(&"é".repeat(3000), "");
        assert_eq!(body.chars().count(), QUERY_LIMIT);
    }

    #[test]
    fn document_uses_family_prefix() {
        let mut gemma = qwen();
        gemma.document_prefix = "title: none | text: file: ".into();
        assert_eq!(
            gemma.format_document("src/a.rs\nfn main"),
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
            "document_prefix":"title: none | text: file: ","inputs":["input_ids","attention_mask"],
            "output":"embeddings","normalized":true,"matryoshka":true}"#;
        let spec = ModelSpec::from_json(json).unwrap();
        assert_eq!(spec.family, Family::Gemma);
        assert_eq!(spec.pooling, Pooling::Mean);
        assert!(spec.matryoshka);
    }

    proptest! {
        #[test]
        fn body_never_exceeds_limit(q in ".{0,4000}", c in ".{0,4000}") {
            let body = query_body(&q, &c);
            let overhead = if c.trim().is_empty() { 0 } else { "\nRecent context:\n".len() };
            prop_assert!(body.chars().count() <= QUERY_LIMIT + overhead);
        }

        #[test]
        fn context_tail_is_kept(q in "[a-z]{0,50}", c in "[a-z]{1,3000}") {
            let body = query_body(&q, &c);
            let tail = body.split_once("\nRecent context:\n").unwrap().1;
            prop_assert!(c.ends_with(tail));
        }

        #[test]
        fn truncation_yields_unit_vectors(v in prop::collection::vec(-1.0f32..1.0, 768), d in 1usize..768) {
            let t = truncate_normalize(&v, d);
            prop_assert_eq!(t.len(), d);
            let n: f32 = t.iter().map(|x| x * x).sum();
            prop_assert!(n == 0.0 || (n - 1.0).abs() < 1e-4);
        }
    }
}
