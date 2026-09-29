//! Document embedding throughput on a real repository's file skeletons, by batch size:
//! cargo run --release -p wn-embed --example doc_throughput -- <model-dir> <repo> [n-files]

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use wn_embed::embedder::Embedder;
use wn_sources::{file_doc, is_source, read_text, MAX_SOURCE_BYTES};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next()
            .expect("usage: doc_throughput <model-dir> <repo> [n]"),
    );
    let repo = PathBuf::from(args.next().expect("missing <repo>"));
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(512);
    let listed = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["ls-files"])
        .output()
        .expect("git ls-files");
    let docs: Vec<String> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|p| is_source(p))
        .filter_map(|p| {
            let text = read_text(&repo.join(p), MAX_SOURCE_BYTES).ok()?;
            Some(file_doc(p, &text))
        })
        .take(n)
        .collect();
    let mut e = Embedder::load(&dir, None).expect("load");
    let prefix = e.spec().document_prefix.clone();
    let docs: Vec<String> = docs.into_iter().map(|d| format!("{prefix}{d}")).collect();
    let _ = e.embed(&docs[..8.min(docs.len())], 8, None).unwrap();
    for batch in [4, 8, 16, 32, 64] {
        let t = Instant::now();
        e.embed(&docs, batch, None).unwrap();
        println!(
            "batch {batch:>2}: {:.1} docs/s ({} docs)",
            docs.len() as f64 / t.elapsed().as_secs_f64(),
            docs.len()
        );
    }
}
