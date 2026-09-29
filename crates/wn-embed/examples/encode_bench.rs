//! Single-query encode latency and document throughput on CPU:
//! cargo run --release -p wn-embed --example encode_bench -- <model-dir> [graph]

use std::path::PathBuf;
use std::time::Instant;

use wn_embed::embedder::Embedder;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next()
            .expect("usage: encode_bench <model-dir> [graph]"),
    );
    let graph = args.next();
    let t = Instant::now();
    let mut e = Embedder::load(&dir, graph.as_deref()).expect("load");
    let load = t.elapsed().as_millis();
    let q = e.spec().query_prefix.clone() + "retry the request when the upstream server times out";
    let _ = e.embed(std::slice::from_ref(&q), 1, None).unwrap();
    let mut times: Vec<f64> = (0..40)
        .map(|_| {
            let t = Instant::now();
            e.embed(std::slice::from_ref(&q), 1, None).unwrap();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let docs: Vec<String> = (0..64)
        .map(|i| format!("file: src/module_{i}.rs\nHandles retries.\nretry_{i} backoff Client"))
        .collect();
    let t = Instant::now();
    e.embed(&docs, 16, None).unwrap();
    let docs_per_s = 64.0 / t.elapsed().as_secs_f64();
    println!(
        "{}: load {load} ms, query p50 {:.1} ms p90 {:.1} ms, docs {:.0}/s",
        e.graph(),
        times[20],
        times[36],
        docs_per_s
    );
}
