//! MCP round trip over an in-memory transport: list tools, call where_next / status / refresh.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use rmcp::model::CallToolRequestParams;
use rmcp::ServiceExt;
use serde_json::Value;
use wn_core::encoder::HashEncoder;
use wn_daemon::daemon::{Daemon, Service};
use wn_daemon::workspace::Workspace;
use wn_mcp::WhereNextServer;

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .unwrap()
        .success());
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    fs::write(
        dir.path().join("billing.py"),
        "\"\"\"Invoices.\"\"\"\n\ndef charge_invoice():\n    pass\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("auth.py"),
        "def verify_token():\n    pass\n",
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

fn text(result: &rmcp::model::CallToolResult) -> Value {
    serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
}

fn args(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn tools_answer_over_mcp() {
    let dir = repo();
    let cache = tempfile::tempdir().unwrap();
    let mut ws = Workspace::open(dir.path(), cache.path(), Arc::new(HashEncoder { dim: 128 }));
    ws.options.no_abstain = true;
    let typed = Arc::new(Mutex::new(Daemon::new(ws)));
    // Before warm-up the tool fails open rather than returning an empty hint list.
    let service: Arc<Mutex<dyn Service>> = typed.clone();
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let server = WhereNextServer::new(service.clone());
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.unwrap();

    let tools = client.list_tools(None).await.unwrap();
    let mut names: Vec<_> = tools.tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["refresh_index", "status", "where_next"]);

    let cold = client
        .call_tool(
            CallToolRequestParams::new("where_next")
                .with_arguments(args(serde_json::json!({"query": "charge an invoice"}))),
        )
        .await
        .unwrap();
    assert_eq!(text(&cold)["state"], "error");

    typed.lock().unwrap().warm().unwrap();

    let warm = client
        .call_tool(
            CallToolRequestParams::new("where_next").with_arguments(args(serde_json::json!({
                "query": "charge an invoice",
                "context": "TypeError in charge_invoice"
            }))),
        )
        .await
        .unwrap();
    let answer = text(&warm);
    assert_eq!(answer["state"], "ok");
    assert_eq!(answer["files"][0]["path"], "billing.py");
    assert!(answer["files"].as_array().unwrap().len() <= 3);

    let status = client
        .call_tool(CallToolRequestParams::new("status"))
        .await
        .unwrap();
    assert_eq!(text(&status)["session"], "Serving");

    let refresh = client
        .call_tool(CallToolRequestParams::new("refresh_index"))
        .await
        .unwrap();
    assert_eq!(text(&refresh)["encoded"], 0);

    client.cancel().await.unwrap();
    server_task.abort();
}

#[cfg(feature = "onnx")]
#[test]
fn missing_model_falls_back_to_lexical_and_still_serves() {
    use std::time::{Duration, Instant};
    let dir = repo();
    let cache = tempfile::tempdir().unwrap();
    let (service, refresher, choice) = wn_mcp::open_repo(
        dir.path(),
        Path::new("/nonexistent/model"),
        cache.path(),
        Duration::from_millis(100),
    );
    assert!(matches!(choice, wn_mcp::EncoderChoice::LexicalFallback(_)));
    let deadline = Instant::now() + Duration::from_secs(10);
    while service.lock().unwrap().status().session != "Serving" {
        assert!(Instant::now() < deadline, "never warmed");
        std::thread::sleep(Duration::from_millis(20));
    }
    let reply = service.lock().unwrap().ask("charge the invoice", "");
    assert!(
        reply.provenance.model.contains("hash"),
        "{}",
        reply.provenance.model
    );
    refresher.stop();
}
