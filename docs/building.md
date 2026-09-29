# Building and testing

What exists in the workspace today, and how to build, test and benchmark it. For where the project is
headed, see [PLAN.md](../PLAN.md).

## Status by crate

| Crate | What it does today |
|---|---|
| `wn-core` | Shared types, the query text builder, ranking, the per-repo adapter (fit and apply), abstain logic and the 3-hint budget. Contains the index, adapter and query lifecycle state machines and the runtime that ties them together. Behaviour the model depends on is pinned to the research prototype by golden tests. |
| `wn-git` | Which files to index (tracked plus untracked, respecting `.gitignore`), commit history as "message → changed files", and co-change. Shells out to `git`, and walks the directory when there is no repository. |
| `wn-sources` | File kinds and the text each file is embedded as: multi-language skeletons and symbols for code, function documents, and config files ranked separately. A line-for-line port of the reference, checked by golden tests. |
| `wn-embed` | ONNX Runtime inference: loads a model directory, verifies every file against its manifest, and embeds queries and documents. Model lifecycle state machine: `Missing → Downloading → Verifying → Loaded / Corrupt`. See [crates/wn-embed/README.md](../crates/wn-embed/README.md). |
| `wn-daemon` | The resident session: keeps model and index warm, refreshes in the background, and fails open. Session lifecycle state machine: `Starting → Warming → Serving / Degraded → ShuttingDown → Stopped`. |
| `wn-mcp` | MCP server over stdio with three tools: `where_next(query, context?)` (at most 3 paths, or a fail-open state), `refresh_index()` and `status()`. Ships the `wn-mcp-server` binary. If the model is missing or fails verification, it falls back to a lexical encoder and says so. |
| `wn-cli` | The `wn` command. **Not wired up yet**: it prints a placeholder. `init`, `ask`, `status` and `mcp` arrive with milestone M1. |

Model weights are not in this repository. Tests that need a real model skip unless you point them at one.

## Build

```sh
cargo build --workspace
```

The `onnx` feature (on by default in `wn-embed` and `wn-mcp`) downloads prebuilt ONNX Runtime binaries
at build time.

## Test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs exactly these on macOS arm64, Linux x86_64 and Linux arm64.

What the tests cover:

- **State machines:** every (state, event) pair against an independently written transition table,
  plus model-based property tests that run random event sequences against a reference model.
- **Golden parity:** query text, document text, skeletons, adapter behaviour and abstain decisions
  match fixtures generated from the research prototype.
- **Fixture repositories:** small git repositories built inside the tests, with renames, deletions and
  untracked files.
- **MCP round trip:** an in-memory client calls the server's tools.

With a real exported model:

```sh
WN_TEST_MODEL_DIR=/path/to/model cargo test --release -p wn-embed --test onnx_parity
```

This checks Rust embeddings and text builders against the Python reference.

## Run the MCP server

Until `wn mcp` exists, run the server binary directly:

```sh
cargo build --release -p wn-mcp --bin wn-mcp-server
./target/release/wn-mcp-server <repo-root> <model-dir> <cache-home>
```

## Benchmarks

| Command | Measures |
|---|---|
| `cargo bench -p wn-daemon` | Query-path overhead (ranking, abstain logic, reply) on a synthetic 20k-file index. No weights needed. |
| `cargo run --release -p wn-embed --example encode_bench -- <model-dir>` | Single-query latency and document throughput on CPU |
| `cargo run --release -p wn-mcp --example latency -- <repo> <model-dir> <cache-home>` | Warm-up, then p50/p95/max of warm `where_next` calls in-process |
| `cargo run --release -p wn-mcp --example mcp_bench -- <server-bin> <repo> <model-dir> <cache-home>` | Round trips through `wn-mcp-server` over stdio, as an agent would call it |

Write a model manifest with `cargo run -p wn-embed --example manifest -- <model-dir>`.
