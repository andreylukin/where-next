//! ONNX Runtime embedder: tokenize, run the exported graph, return unit vectors.
//!
//! The exported graph already contains pooling, any Dense projection and normalisation, so this
//! only tokenizes (right padding, truncation to `max_seq`) and batches.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use tokenizers::{Tokenizer, TruncationParams};

use crate::spec::{truncate_normalize, ModelSpec};

/// Graph file preference: quantized first (smaller, faster on CPU), then fp32.
pub const GRAPH_FILES: [&str; 2] = ["model.int8.onnx", "model.onnx"];

/// Errors from loading or running the embedder.
#[derive(Debug)]
pub enum EmbedError {
    Spec(String),
    Tokenizer(String),
    Runtime(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedError::Spec(m) => write!(f, "model spec: {m}"),
            EmbedError::Tokenizer(m) => write!(f, "tokenizer: {m}"),
            EmbedError::Runtime(m) => write!(f, "onnx runtime: {m}"),
        }
    }
}

impl std::error::Error for EmbedError {}

fn rt<E: std::fmt::Display>(e: E) -> EmbedError {
    EmbedError::Runtime(e.to_string())
}

/// A loaded model: its spec, tokenizer and ONNX session.
pub struct Embedder {
    spec: ModelSpec,
    tokenizer: Tokenizer,
    pad_id: u32,
    session: Session,
    graph: String,
}

impl Embedder {
    /// Loads a model directory containing `wn-model.json`, `tokenizer.json` and a graph.
    /// `graph` picks a specific file; `None` prefers the int8 graph.
    pub fn load(dir: &Path, graph: Option<&str>) -> Result<Self, EmbedError> {
        let spec_text = std::fs::read_to_string(dir.join("wn-model.json"))
            .map_err(|e| EmbedError::Spec(e.to_string()))?;
        let spec = ModelSpec::from_json(&spec_text).map_err(|e| EmbedError::Spec(e.to_string()))?;
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: spec.max_seq,
                ..Default::default()
            }))
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        let pad_id = tokenizer
            .get_padding()
            .map(|p| p.pad_id)
            .or_else(|| tokenizer.token_to_id("<pad>"))
            .or_else(|| tokenizer.token_to_id("<|endoftext|>"))
            .unwrap_or(0);
        // Padding is done per batch in `embed` (right side), after sorting by length.
        tokenizer.with_padding(None);
        let graph = match graph {
            Some(g) => g.to_string(),
            None => GRAPH_FILES
                .iter()
                .find(|g| dir.join(g).is_file())
                .ok_or_else(|| EmbedError::Spec("no ONNX graph in model directory".into()))?
                .to_string(),
        };
        let session = Session::builder()
            .map_err(rt)?
            .commit_from_file(dir.join(&graph))
            .map_err(rt)?;
        Ok(Self {
            spec,
            tokenizer,
            pad_id,
            session,
            graph,
        })
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    /// The graph file in use (`model.int8.onnx` or `model.onnx`).
    pub fn graph(&self) -> &str {
        &self.graph
    }

    /// Embeds already-formatted texts (see [`ModelSpec::format_document`] and
    /// `wn_core::text::query_text`) in batches of `batch`. Texts are grouped by token length so
    /// batches carry little padding; results come back in input order. Returns unit vectors of
    /// the model's full dimension, or of `dim` when given (Matryoshka truncation, re-normalised).
    pub fn embed(
        &mut self,
        texts: &[String],
        batch: usize,
        dim: Option<usize>,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        let order = length_order(&encodings.iter().map(|e| e.len()).collect::<Vec<_>>());
        let mut out: Vec<Vec<f32>> = vec![Vec::new(); texts.len()];
        for chunk in order.chunks(batch.max(1)) {
            let rows = chunk.len();
            let cols = chunk
                .iter()
                .map(|&i| encodings[i].len())
                .max()
                .unwrap_or(0)
                .max(1);
            let mut ids = Vec::with_capacity(rows * cols);
            let mut mask = Vec::with_capacity(rows * cols);
            for &i in chunk {
                let e = &encodings[i];
                ids.extend(e.get_ids().iter().map(|&x| x as i64));
                mask.extend(e.get_attention_mask().iter().map(|&x| x as i64));
                let pad = cols - e.len();
                ids.resize(ids.len() + pad, self.pad_id as i64);
                mask.resize(mask.len() + pad, 0);
            }
            let ids = Tensor::from_array(([rows, cols], ids)).map_err(rt)?;
            let mask = Tensor::from_array(([rows, cols], mask)).map_err(rt)?;
            let outputs = self
                .session
                .run(ort::inputs!["input_ids" => ids, "attention_mask" => mask])
                .map_err(rt)?;
            let (shape, data) = outputs["embeddings"]
                .try_extract_tensor::<f32>()
                .map_err(rt)?;
            let width = shape[1] as usize;
            for (&i, row) in chunk.iter().zip(data.chunks(width)) {
                out[i] = match dim {
                    Some(d) if d < width => truncate_normalize(row, d),
                    _ => row.to_vec(),
                };
            }
        }
        Ok(out)
    }
}

/// Indices of `lengths` sorted by length (stable), so consecutive batches have similar lengths.
pub fn length_order(lengths: &[usize]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lengths.len()).collect();
    order.sort_by_key(|&i| lengths[i]);
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_order_is_a_stable_permutation() {
        let order = length_order(&[5, 1, 5, 3, 1]);
        assert_eq!(order, vec![1, 4, 3, 0, 2]);
        let mut seen = order.clone();
        seen.sort();
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    }
}
