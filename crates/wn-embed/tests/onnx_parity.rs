//! Parity of the Rust ONNX embedder with the Python sentence-transformers reference.
//!
//! Needs an exported model directory (not in the repo: weights are distributed separately).
//! Set `WN_TEST_MODEL_DIR` to a directory with `wn-model.json`, `tokenizer.json`, the ONNX graphs
//! and `fixture.json` (texts plus reference embeddings written by the export script). Without it
//! the test is skipped.

#![cfg(feature = "onnx")]

use std::path::PathBuf;

use serde::Deserialize;
use wn_embed::embedder::Embedder;
use wn_embed::spec::dot;

#[derive(Deserialize)]
struct Case {
    formatted: String,
    embedding: Vec<f32>,
}

fn model_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("WN_TEST_MODEL_DIR")?);
    dir.join("fixture.json").is_file().then_some(dir)
}

fn check(graph: &str, min_cos: f32) {
    let Some(dir) = model_dir() else {
        eprintln!("skipped: set WN_TEST_MODEL_DIR to run ONNX parity");
        return;
    };
    if !dir.join(graph).is_file() {
        eprintln!("skipped: {graph} not in {dir:?}");
        return;
    }
    let cases: Vec<Case> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("fixture.json")).unwrap()).unwrap();
    let mut embedder = Embedder::load(&dir, Some(graph)).unwrap();
    let texts: Vec<String> = cases.iter().map(|c| c.formatted.clone()).collect();
    let got = embedder.embed(&texts, 8, None).unwrap();
    let mut worst = 1.0f32;
    for (case, vector) in cases.iter().zip(&got) {
        assert_eq!(vector.len(), case.embedding.len());
        worst = worst.min(dot(vector, &case.embedding));
    }
    eprintln!(
        "{graph}: worst cosine vs Python = {worst:.5} over {} texts",
        cases.len()
    );
    assert!(
        worst > min_cos,
        "{graph}: worst cosine {worst} <= {min_cos}"
    );
}

#[test]
fn fp32_graph_matches_python() {
    check("model.onnx", 0.999);
}

#[test]
fn int8_graph_matches_python() {
    check("model.int8.onnx", 0.99);
}
