//! ONNX Runtime embedder: tokenize, run the exported graph, return unit vectors.
//!
//! The exported graph already contains pooling, any Dense projection and normalisation, so this
//! only tokenizes (right padding, truncation to `max_seq`) and batches.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use tokenizers::{Tokenizer, TruncationParams};

use crate::spec::{truncate_normalize, ModelSpec};

/// Graph file preference: fp32, then the weight-only int8 graph (`MatMulNBits`, 8 bits). The q8
/// graph is ~half the size but ~3x slower per query on Apple CPUs, so it is used only when it is the
/// graph shipped. Dynamic activation int8 and 4-bit graphs fail the parity gate for these models.
pub const GRAPH_FILES: [&str; 2] = ["model.onnx", "model.q8.onnx"];

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
    /// Same tokenizer without truncation, to measure how long a text really is.
    counter: Tokenizer,
    pad_id: u32,
    session: Session,
    graph: String,
}

impl Embedder {
    /// Loads a model directory containing `wn-model.json`, `tokenizer.json` and a graph.
    /// `graph` picks a specific file; `None` prefers fp32.
    pub fn load(dir: &Path, graph: Option<&str>) -> Result<Self, EmbedError> {
        let spec_text = std::fs::read_to_string(dir.join("wn-model.json"))
            .map_err(|e| EmbedError::Spec(e.to_string()))?;
        let spec = ModelSpec::from_json(&spec_text).map_err(|e| EmbedError::Spec(e.to_string()))?;
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        let mut counter = tokenizer.clone();
        counter
            .with_truncation(None)
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
            counter,
            pad_id,
            session,
            graph,
        })
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    /// Tokens `text` really has (special tokens included; no truncation).
    pub fn count_tokens(&self, text: &str) -> usize {
        self.counter
            .encode(text, true)
            .map(|e| e.len())
            .unwrap_or(usize::MAX)
    }

    /// The graph file in use (`model.q8.onnx` or `model.onnx`).
    pub fn graph(&self) -> &str {
        &self.graph
    }

    /// Embeds already-formatted texts (see [`ModelSpec::format_document`] and
    /// `wn_core::text::query_text`). Texts are sorted by token length and grouped into batches of
    /// at most `batch` rows and at most [`TOKEN_BUDGET`] padded tokens (rows × longest), so long
    /// documents (up to `max_seq`, 1,024 for gemma-xl1) run in smaller batches and ONNX Runtime's
    /// peak activation memory stays bounded. Results come back in input order. Returns unit vectors
    /// of the model's full dimension, or of `dim` when given (Matryoshka truncation, re-normalised).
    pub fn embed(
        &mut self,
        texts: &[String],
        batch: usize,
        dim: Option<usize>,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        // Keep only ids and lengths: full `Encoding`s also carry offsets, token strings and the
        // truncated overflow, which for 1,024 long documents is most of the tokenizer's memory.
        let ids: Vec<Vec<u32>> = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?
            .into_iter()
            .map(|e| e.get_ids().to_vec())
            .collect();
        let lengths: Vec<usize> = ids.iter().map(Vec::len).collect();
        let order = length_order(&lengths);
        let mut out: Vec<Vec<f32>> = vec![Vec::new(); texts.len()];
        for range in plan_batches(&order, &lengths, batch, TOKEN_BUDGET) {
            let chunk = &order[range];
            let rows = chunk.len();
            let cols = chunk.iter().map(|&i| lengths[i]).max().unwrap_or(0).max(1);
            let mut input = Vec::with_capacity(rows * cols);
            let mut mask: Vec<i64> = Vec::with_capacity(rows * cols);
            for &i in chunk {
                input.extend(ids[i].iter().map(|&x| x as i64));
                mask.resize(mask.len() + lengths[i], 1);
                let pad = cols - lengths[i];
                input.resize(input.len() + pad, self.pad_id as i64);
                mask.resize(mask.len() + pad, 0);
            }
            let input = Tensor::from_array(([rows, cols], input)).map_err(rt)?;
            let mask = Tensor::from_array(([rows, cols], mask)).map_err(rt)?;
            let outputs = self
                .session
                .run(ort::inputs!["input_ids" => input, "attention_mask" => mask])
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

/// Most padded tokens (rows × longest row) in one ONNX batch. 16 rows of 512 tokens: short
/// documents still run 16 at a time (the measured CPU sweet spot), long ones in smaller batches.
pub const TOKEN_BUDGET: usize = 16 * 512;

/// Splits `order` (indices sorted by ascending length) into consecutive batches of at most
/// `max_rows` rows whose padded size (rows × longest length) stays within `token_budget`. A single
/// text longer than the budget gets a batch of its own.
pub fn plan_batches(
    order: &[usize],
    lengths: &[usize],
    max_rows: usize,
    token_budget: usize,
) -> Vec<std::ops::Range<usize>> {
    let max_rows = max_rows.max(1);
    let mut batches = Vec::new();
    let mut start = 0;
    while start < order.len() {
        let mut end = start + 1;
        // Sorted ascending, so the newest row is always the longest.
        while end < order.len()
            && end - start < max_rows
            && (end - start + 1) * lengths[order[end]].max(1) <= token_budget
        {
            end += 1;
        }
        batches.push(start..end);
        start = end;
    }
    batches
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
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn batches_cover_every_text_once_within_limits(
            lengths in proptest::collection::vec(1usize..1100, 0..300),
            max_rows in 1usize..40,
            budget in 1usize..20_000,
        ) {
            let order = length_order(&lengths);
            let batches = plan_batches(&order, &lengths, max_rows, budget);
            let mut seen: Vec<usize> = batches.iter().flat_map(|r| order[r.clone()].to_vec()).collect();
            seen.sort();
            prop_assert_eq!(seen, (0..lengths.len()).collect::<Vec<_>>());
            let mut next = 0;
            for r in &batches {
                prop_assert_eq!(r.start, next);
                next = r.end;
                let rows = r.len();
                prop_assert!(rows >= 1 && rows <= max_rows);
                let longest = order[r.clone()].iter().map(|&i| lengths[i]).max().unwrap();
                prop_assert!(rows == 1 || rows * longest <= budget);
            }
        }
    }

    #[test]
    fn short_texts_fill_rows_long_texts_shrink_batches() {
        let lengths = vec![100; 40];
        let order = length_order(&lengths);
        let b = plan_batches(&order, &lengths, 16, TOKEN_BUDGET);
        assert_eq!(
            b.iter().map(|r| r.len()).collect::<Vec<_>>(),
            vec![16, 16, 8]
        );
        let lengths = vec![1024; 20];
        let order = length_order(&lengths);
        let b = plan_batches(&order, &lengths, 16, TOKEN_BUDGET);
        assert_eq!(b.iter().map(|r| r.len()).collect::<Vec<_>>(), vec![8, 8, 4]);
    }

    #[test]
    fn length_order_is_a_stable_permutation() {
        let order = length_order(&[5, 1, 5, 3, 1]);
        assert_eq!(order, vec![1, 4, 3, 0, 2]);
        let mut seen = order.clone();
        seen.sort();
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    }
}
