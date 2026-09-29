//! The embedding backend interface, plus a dependency-free lexical fallback.
//!
//! Backends receive *what* to embed — document text built by `wn-sources`, or a task plus its
//! recent context — and add whatever instruction or prompt their model family expects.

use std::fmt;

use crate::text::{query_text, Granularity};

/// A query to embed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryInput {
    /// The task or request.
    pub query: String,
    /// Recent context (conversation, last tool output); may be empty.
    pub context: String,
    /// What the query ranks.
    pub granularity: Granularity,
}

impl QueryInput {
    /// A file-granularity query without context.
    pub fn file(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            context: String::new(),
            granularity: Granularity::File,
        }
    }
}

/// Why embedding failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError(pub String);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "embedding failed: {}", self.0)
    }
}

impl std::error::Error for EncodeError {}

/// An embedding backend. Vectors must be L2-normalised and all have the same dimension.
pub trait Encoder {
    /// Stable identifier of the model and document builder; the vector cache is keyed by it.
    fn fingerprint(&self) -> String;
    /// This model's abstain calibration; `None` (the default) never abstains.
    fn calibration(&self) -> Option<crate::rank::Calibration> {
        None
    }
    /// Embeds document texts.
    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError>;
    /// Embeds queries.
    fn queries(&self, items: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError>;
}

/// A hashed bag-of-words encoder: no model, no downloads, deterministic.
///
/// Used when no model is installed (so `wn` still returns something useful offline) and in
/// tests. Identifiers are split on case and punctuation (`handleLogin` → `handle`, `login`).
#[derive(Debug, Clone, Copy)]
pub struct HashEncoder {
    /// Vector dimension.
    pub dim: usize,
}

impl Default for HashEncoder {
    fn default() -> Self {
        Self { dim: 1024 }
    }
}

/// Very common English and code words that carry no signal for locating files.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "too", "not", "but", "are", "was",
    "were", "has", "have", "had", "its", "our", "you", "your", "can", "will", "should", "when",
    "then", "than", "there", "here", "what", "which", "who", "why", "how", "all", "any", "some",
    "now", "just", "also", "very", "more", "most", "early", "late", "file", "src", "lib", "fix",
];

/// A light suffix stemmer: `sessions` → `session`, `retries` → `retry`, `timed` → `tim`.
pub fn stem(word: &str) -> &str {
    let n = word.len();
    if n > 5 && word.ends_with("ies") {
        return &word[..n - 3];
    }
    for suffix in ["ing", "ed", "es", "s"] {
        if n > suffix.len() + 3 && word.ends_with(suffix) {
            return &word[..n - suffix.len()];
        }
    }
    word
}

/// Lowercased word pieces of `text`, split on non-alphanumerics and camelCase boundaries.
pub fn word_pieces(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if c.is_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = c.is_lowercase() || c.is_numeric();
            cur.extend(c.to_lowercase());
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl HashEncoder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        for w in word_pieces(text) {
            if w.len() < 2 || STOPWORDS.contains(&w.as_str()) {
                continue;
            }
            let h = fnv1a(stem(&w));
            let sign = if h >> 63 == 0 { 1.0 } else { -1.0 };
            v[(h % self.dim as u64) as usize] += sign;
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            v.iter_mut().for_each(|x| *x /= n);
        }
        v
    }
}

impl Encoder for HashEncoder {
    fn fingerprint(&self) -> String {
        format!("hash-bow2-{}", self.dim)
    }

    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
        Ok(texts.iter().map(|t| self.embed(t)).collect())
    }

    fn queries(&self, items: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError> {
        Ok(items
            .iter()
            .map(|q| {
                // The instruction words would add the same noise to every query; embed the
                // task and context only.
                let text = query_text(&q.query, &q.context, q.granularity);
                let body = text.split_once("Query: ").map_or(text.as_str(), |x| x.1);
                self.embed(body)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_pieces_split_identifiers() {
        assert_eq!(
            word_pieces("handleLogin in auth_store.py"),
            vec!["handle", "login", "in", "auth", "store", "py"]
        );
    }

    #[test]
    fn stemming_merges_plural_and_tense() {
        assert_eq!(stem("sessions"), "session");
        assert_eq!(stem("session"), "session");
        assert_eq!(stem("retries"), "retr");
        assert_eq!(stem("uploads"), "upload");
        assert_eq!(stem("go"), "go");
    }

    #[test]
    fn hash_encoder_prefers_overlapping_words() {
        let e = HashEncoder::default();
        let docs = e
            .documents(&[
                "file: src/auth.py\nlogin token".into(),
                "file: src/ui.ts\nrender button".into(),
            ])
            .unwrap();
        let q = &e
            .queries(&[QueryInput::file("fix the login token check")])
            .unwrap()[0];
        let dot = |d: &Vec<f32>| d.iter().zip(q).map(|(a, b)| a * b).sum::<f32>();
        assert!(dot(&docs[0]) > dot(&docs[1]));
    }
}
