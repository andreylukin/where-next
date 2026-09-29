//! Latency benchmark of the resident service with a real model.
//!
//! cargo run --release -p wn-mcp --example latency -- <repo> <model-dir> <cache-home> [queries]
//!
//! Reports warm-up (scan + embed + adapter fit, or snapshot load), then p50/p95/max of warm
//! `where_next` calls in-process (encode + rank + JSON), plus one incremental refresh after
//! touching nothing (the steady-state background cost).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use wn_daemon::daemon::{Daemon, Service};
use wn_daemon::workspace::Workspace;
use wn_embed::core_encoder::OnnxEncoder;

const QUERIES: [&str; 8] = [
    "retry the request when the server times out",
    "where is authentication handled",
    "fix the flaky test for the cache eviction",
    "add a flag to the command line parser",
    "the config file is not reloaded after changes",
    "memory leak in the worker pool",
    "log the error with the request id",
    "update the database migration for the new column",
];

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let repo = PathBuf::from(
        args.next()
            .ok_or("usage: latency <repo> <model> <cache> [n]")?,
    );
    let model = PathBuf::from(args.next().ok_or("missing <model-dir>")?);
    let cache = PathBuf::from(args.next().ok_or("missing <cache-home>")?);
    let n: usize = args.next().map(|s| s.parse()).transpose()?.unwrap_or(200);

    let t = Instant::now();
    let encoder = Arc::new(OnnxEncoder::open(&model, None)?);
    let load_ms = t.elapsed().as_millis();
    let mut daemon = Daemon::new(Workspace::open(&repo, &cache, encoder));
    let t = Instant::now();
    let warm = daemon.warm()?;
    let warm_ms = t.elapsed().as_millis();
    let service: Arc<Mutex<dyn Service>> = Arc::new(Mutex::new(daemon));

    let mut times = Vec::with_capacity(n);
    for i in 0..n {
        let q = QUERIES[i % QUERIES.len()];
        let t = Instant::now();
        let reply = service.lock().unwrap().ask(q, "");
        let _json = serde_json::to_string(&reply)?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    let t = Instant::now();
    let refresh = service.lock().unwrap().refresh()?;
    let refresh_ms = t.elapsed().as_millis();
    let status = service.lock().unwrap().status();
    println!(
        "{}",
        serde_json::json!({
            "repo": repo.display().to_string(),
            "model_load_ms": load_ms,
            "warm_ms": warm_ms,
            "warm": warm,
            "files_indexed": status.provenance.files_indexed,
            "queries": n,
            "query_ms": {"p50": percentile(&times, 0.5), "p95": percentile(&times, 0.95), "max": times[times.len() - 1]},
            "noop_refresh_ms": refresh_ms,
            "noop_refresh": refresh,
        })
    );
    Ok(())
}
