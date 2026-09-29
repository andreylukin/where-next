//! Writes `wn-manifest.json` (SHA-256 of every model file) into a model directory.
//!
//! cargo run -p wn-embed --example manifest -- <model-dir>

use std::path::PathBuf;

use wn_embed::verify::{Manifest, MANIFEST_FILE};

const CANDIDATES: [&str; 6] = [
    "wn-model.json",
    "tokenizer.json",
    "model.onnx",
    "model.onnx.data",
    "model.int8.onnx",
    "fixture.json",
];

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: manifest <model-dir>"),
    );
    let present: Vec<&str> = CANDIDATES
        .into_iter()
        .filter(|f| dir.join(f).is_file())
        .collect();
    let manifest = Manifest::compute(&dir, &present).expect("hash model files");
    let json = serde_json::to_string_pretty(&manifest).expect("serialize manifest");
    std::fs::write(dir.join(MANIFEST_FILE), json).expect("write manifest");
    println!(
        "{} files, fingerprint {}",
        present.len(),
        manifest.fingerprint()
    );
}
