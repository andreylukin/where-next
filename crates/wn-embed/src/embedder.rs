//! ONNX Runtime embedder: tokenize, run the exported graph, return unit vectors.
//!
//! The exported graph already contains pooling, any Dense projection and normalisation, so this
//! only tokenizes (right padding, truncation to `max_seq`) and batches.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use tokenizers::{PaddingDirection, PaddingParams, Tokenizer, TruncationParams};

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
        tokenizer.with_padding(Some(PaddingParams {
            direction: PaddingDirection::Right,
            pad_id,
            ..Default::default()
        }));
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

    /// Embeds already-formatted texts (see [`ModelSpec::format_query`] and
    /// [`ModelSpec::format_document`]) in batches of `batch`. Returns unit vectors of the model's
    /// full dimension, or of `dim` when given (Matryoshka truncation, re-normalised).
    pub fn embed(
        &mut self,
        texts: &[String],
        batch: usize,
        dim: Option<usize>,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(batch.max(1)) {
            let encodings = self
                .tokenizer
                .encode_batch(chunk.to_vec(), true)
                .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
            let rows = encodings.len();
            let cols = encodings.iter().map(|e| e.len()).max().unwrap_or(0);
            let mut ids = Vec::with_capacity(rows * cols);
            let mut mask = Vec::with_capacity(rows * cols);
            for e in &encodings {
                ids.extend(e.get_ids().iter().map(|&x| x as i64));
                mask.extend(e.get_attention_mask().iter().map(|&x| x as i64));
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
            for row in data.chunks(width) {
                out.push(match dim {
                    Some(d) if d < width => truncate_normalize(row, d),
                    _ => row.to_vec(),
                });
            }
        }
        Ok(out)
    }
}
