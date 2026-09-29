//! Standalone MCP server over stdio (the `wn mcp` subcommand wraps the same library call).
//!
//! wn-mcp-server <repo-root> <model-dir> <cache-home>

use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(
        args.next()
            .ok_or("usage: wn-mcp-server <repo> <model-dir> <cache-home>")?,
    );
    let model = PathBuf::from(args.next().ok_or("missing <model-dir>")?);
    let cache = PathBuf::from(args.next().ok_or("missing <cache-home>")?);
    let (service, refresher) = wn_mcp::open_repo(&root, &model, &cache, Duration::from_secs(10))?;
    let result = wn_mcp::serve_stdio(service).await;
    refresher.stop();
    result
}
