//! External MCP latency: spawns `wn-mcp-server` over stdio like an agent would, waits until the
//! session serves, then times `where_next` round trips (JSON-RPC, stdio, encode, rank).
//!
//! cargo build --release -p wn-mcp --bin wn-mcp-server
//! cargo run --release -p wn-mcp --example mcp_bench -- <server-bin> <repo> <model-dir> <cache> [n]

use std::time::{Duration, Instant};

use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::{json, Value};

fn text(r: &rmcp::model::CallToolResult) -> Value {
    serde_json::from_str(&r.content[0].as_text().unwrap().text).unwrap()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 4 {
        return Err("usage: mcp_bench <server-bin> <repo> <model-dir> <cache> [n]".into());
    }
    let n: usize = a.get(4).map(|s| s.parse()).transpose()?.unwrap_or(100);
    let t0 = Instant::now();
    let server = tokio::process::Command::new(&a[0]).configure(|c| {
        c.args([&a[1], &a[2], &a[3]]);
    });
    let client = ().serve(TokioChildProcess::new(server)?).await?;
    let handshake_ms = t0.elapsed().as_millis();
    loop {
        let s = client
            .call_tool(CallToolRequestParams::new("status"))
            .await?;
        if text(&s)["session"] == "Serving" {
            break;
        }
        if t0.elapsed() > Duration::from_secs(1800) {
            return Err("server never started serving".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let ready_ms = t0.elapsed().as_millis();
    let queries = [
        "retry the request when the server times out",
        "where is authentication handled",
        "fix the flaky cache eviction test",
        "log the error with the request id",
    ];
    let mut times = Vec::with_capacity(n);
    let mut last = Value::Null;
    for i in 0..n {
        let args = json!({"query": queries[i % queries.len()]})
            .as_object()
            .unwrap()
            .clone();
        let t = Instant::now();
        let r = client
            .call_tool(CallToolRequestParams::new("where_next").with_arguments(args))
            .await?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        last = text(&r);
    }
    times.sort_by(f64::total_cmp);
    let p = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    println!(
        "{}",
        json!({
            "handshake_ms": handshake_ms, "ready_ms": ready_ms, "calls": n,
            "round_trip_ms": {"p50": p(0.5), "p95": p(0.95), "max": p(1.0)},
            "last_state": last["state"], "files_indexed": last["provenance"]["files_indexed"],
        })
    );
    client.cancel().await?;
    Ok(())
}
