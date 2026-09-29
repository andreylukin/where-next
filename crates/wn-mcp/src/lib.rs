//! MCP server exposing where-next to coding agents (Claude Code, Codex, Cursor, …).
//!
//! Tools:
//! - `where_next(query, context?)`: at most three paths with similarity scores, or a fail-open
//!   state telling the agent to use ordinary search. Always JSON with a `state` field.
//! - `refresh_index()`: re-scan the repository now.
//! - `status()`: session and index state, document count, model provenance.
//!
//! The server holds a shared [`Service`]; the MCP process is itself the resident session, so the
//! model and index stay warm between calls.

use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Deserialize;
use wn_daemon::daemon::Service;

/// Arguments of `where_next`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WhereNextArgs {
    #[schemars(
        description = "What you are trying to do: the task, bug report, error or question."
    )]
    pub query: String,
    #[schemars(
        description = "Optional recent context: last messages, a stack trace or failing test output."
    )]
    #[serde(default)]
    pub context: Option<String>,
}

/// The MCP server.
#[derive(Clone)]
pub struct WhereNextServer {
    service: Arc<Mutex<dyn Service>>,
    tool_router: ToolRouter<Self>,
}

fn json_result(value: &impl serde::Serialize) -> Result<CallToolResult, McpError> {
    let text =
        serde_json::to_string(value).map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

#[tool_router]
impl WhereNextServer {
    pub fn new(service: Arc<Mutex<dyn Service>>) -> Self {
        Self {
            service,
            tool_router: Self::tool_router(),
        }
    }

    fn with_service<T>(&self, f: impl FnOnce(&mut dyn Service) -> T) -> Result<T, McpError> {
        let mut guard = self
            .service
            .lock()
            .map_err(|_| McpError::internal_error("where-next service lock poisoned", None))?;
        Ok(f(&mut *guard))
    }

    #[tool(
        description = "Suggest up to 3 files in this repository worth opening next for a task. Scores are similarities, not probabilities. Only state \"ok\" (or \"stale_index\") carries hints; any other state (abstain, empty_index, unsupported_scope, error) means use ordinary search instead."
    )]
    fn where_next(
        &self,
        Parameters(args): Parameters<WhereNextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let context = args.context.unwrap_or_default();
        let answer = self.with_service(|s| s.ask(&args.query, &context))?;
        json_result(&answer)
    }

    #[tool(description = "Re-scan the repository and re-embed changed files now.")]
    fn refresh_index(&self) -> Result<CallToolResult, McpError> {
        match self.with_service(|s| s.refresh())? {
            Ok(stats) => json_result(&stats),
            Err(err) => json_result(&serde_json::json!({"status": "error", "reason": err})),
        }
    }

    #[tool(description = "Session and index state, document count and model provenance.")]
    fn status(&self) -> Result<CallToolResult, McpError> {
        let status = self.with_service(|s| s.status())?;
        json_result(&status)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WhereNextServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "where-next suggests which files to open next for a coding task. Call where_next with \
             the task (and optional recent context such as an error). It returns at most 3 paths; \
             treat them as hints, and fall back to ordinary search whenever state is not ok.",
        )
    }
}

/// Serves MCP over stdin/stdout until the client disconnects.
pub async fn serve_stdio(
    service: Arc<Mutex<dyn Service>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::ServiceExt;
    let running = WhereNextServer::new(service)
        .serve(rmcp::transport::stdio())
        .await?;
    running.waiting().await?;
    Ok(())
}

/// The encoder `open_repo` chose, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncoderChoice {
    /// The verified ONNX model.
    Model,
    /// The model could not be loaded; the deterministic lexical encoder answers instead.
    LexicalFallback(String),
}

/// Production setup: the verified ONNX model (or, if it is missing or fails verification, the
/// lexical `HashEncoder`, so hints still work offline), a per-repository cache under
/// `cache_home`, and background warm-up and refresh. Returns the shared service, the refresher
/// (stop it on exit) and which encoder is in use.
#[cfg(feature = "onnx")]
pub fn open_repo(
    root: &std::path::Path,
    model_dir: &std::path::Path,
    cache_home: &std::path::Path,
    refresh_every: std::time::Duration,
) -> (
    Arc<Mutex<dyn Service>>,
    wn_daemon::daemon::background::Refresher,
    EncoderChoice,
) {
    use wn_core::encoder::HashEncoder;
    use wn_daemon::daemon::{background, Daemon};
    use wn_daemon::workspace::{SharedEncoder, Workspace};
    use wn_embed::core_encoder::OnnxEncoder;

    let (encoder, choice): (SharedEncoder, EncoderChoice) = match OnnxEncoder::open(model_dir, None)
    {
        Ok(e) => (Arc::new(e), EncoderChoice::Model),
        Err(err) => (
            Arc::new(HashEncoder::default()),
            EncoderChoice::LexicalFallback(err),
        ),
    };
    let typed = Arc::new(Mutex::new(Daemon::new(Workspace::open(
        root, cache_home, encoder,
    ))));
    let refresher = background::spawn(typed.clone(), refresh_every);
    let service: Arc<Mutex<dyn Service>> = typed;
    (service, refresher, choice)
}
