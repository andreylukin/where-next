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
    kind: String,
    text: String,
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

/// The Rust text builders reproduce the exact strings the Python export embedded.
#[test]
fn text_builders_match_python() {
    use wn_core::encoder::QueryInput;
    use wn_embed::core_encoder::query_for;
    use wn_embed::spec::ModelSpec;
    let Some(dir) = model_dir() else {
        eprintln!("skipped: set WN_TEST_MODEL_DIR to run ONNX parity");
        return;
    };
    let spec =
        ModelSpec::from_json(&std::fs::read_to_string(dir.join("wn-model.json")).unwrap()).unwrap();
    let cases: Vec<Case> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("fixture.json")).unwrap()).unwrap();
    for case in &cases {
        if case.kind == "query" {
            // The export fixture embedded prefix + raw text; production strips surrounding
            // whitespace and keeps the format's request length (v1: 2400 characters as in
            // Python nav2.query_text; v2: 900 as in nav2.query_text_v2).
            let built = query_for(&spec, &QueryInput::file(case.text.clone()));
            let body = built
                .strip_prefix(spec.query_prefix.as_str())
                .expect("prefix");
            let raw = case
                .formatted
                .strip_prefix(spec.query_prefix.as_str())
                .expect("fixture prefix");
            let keep = match spec.query_format {
                wn_embed::spec::QueryFormat::V1 => wn_core::text::QUERY_LIMIT,
                wn_embed::spec::QueryFormat::V2 => wn_core::text::QUERY_REQUEST_CHARS_V2,
            };
            let expected: String = raw.trim().chars().take(keep).collect();
            assert_eq!(body, expected, "query body differs");
        } else {
            let built = spec.format_document(&format!("file: {}", case.text));
            assert_eq!(built, case.formatted, "document text differs");
        }
    }
}

/// A long context never pushes the newest tool output out of the model's window.
#[test]
fn long_context_keeps_the_newest_output() {
    use wn_core::encoder::QueryInput;
    use wn_embed::core_encoder::OnnxEncoder;
    let Some(dir) = model_dir() else {
        eprintln!("skipped: set WN_TEST_MODEL_DIR to run ONNX parity");
        return;
    };
    let enc = OnnxEncoder::open(&dir, None).unwrap();
    let old = "earlier turn about unrelated refactoring. ".repeat(400);
    let item = QueryInput {
        query: "the upload keeps timing out".into(),
        context: format!("{old}\nLast tool output:\nTimeoutError: storage PUT exceeded 30s"),
        granularity: wn_core::text::Granularity::File,
    };
    let text = enc.query_text(&item);
    assert!(
        text.contains("TimeoutError: storage PUT exceeded 30s"),
        "{text}"
    );
    assert!(text.contains("the upload keeps timing out"));
}

#[test]
fn fp32_graph_matches_python() {
    check("model.onnx", 0.999);
}

#[test]
fn q8_graph_matches_python() {
    check("model.q8.onnx", 0.99);
}
