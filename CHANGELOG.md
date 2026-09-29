# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Breaking changes to the MCP tool schema are called out
explicitly.

## [Unreleased]

### Added

- Usability docs: the README quick start states the model situation up front (a lexical fallback
  unless a model is installed; `wn status` shows which), adds `wn bench` as the
  "try it on your repo" step with real sample output, query-writing tips, and a Troubleshooting table.
  `wn --help`, `wn init`, `wn ask`, `wn bench`, `wn report` and `wn skill sync` now end with examples
  (snapshot-tested). `docs/quickstart.md` no longer claims `wn init` downloads a model.
- CLI + skill are now the primary agent interface (MCP stays as an optional adapter):
  - `skills/where-next/SKILL.md`, a short general skill for Claude Code, Codex and Cursor.
  - `wn skill sync` (`--agent`, `--project`, `--dry-run`, `--yes`, `--uninstall`, `--from-state`)
    installs it idempotently with version markers, shows a diff and asks, and never overwrites
    files it did not write. Optional `--with-hook` adds a Claude Code `UserPromptSubmit` hook that
    injects `wn ask --start` hints on a session's first prompt in large repositories.
  - An auto-started per-user daemon for `wn ask` / `wn status`: Unix socket, version handshake,
    background rescans, 15-minute idle exit, fail-open to in-process; `wn daemon start|stop|status`,
    `--no-daemon` / `WN_NO_DAEMON`. The resident, client-connection and skill-sync lifecycles are
    explicit state machines with exhaustive and model-based tests. See [docs/skill.md](docs/skill.md).

- One-line install and update from source: `curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh`
  clones into a private `$WN_HOME/src`, builds with `cargo install --locked`, and on later runs
  rebuilds only when the ref moved (`--ref`, `--yes`, `--force`, `--dry-run`, `--uninstall`;
  asks before installing Rust). The checksum-verified release path moved behind `WN_FROM=release`.
- `wn update` (`--check` exits 10 when an update is available, `--ref`, `--yes`, `--force`):
  fetches the source clone and rebuilds, as an explicit state machine with exhaustive and
  model-based tests. `wn --version` now includes the commit and date it was built from.
- `wn report`: an opt-in, anonymous usage report. It is built from the local usage log as numbers,
  fixed labels and buckets only (no repository names, paths, messages or queries), shown in full,
  and posted as a GitHub issue only after an explicit `y` (via `gh` or a pre-filled browser form).
  `--json` and `--dry-run` never post. The flow is a state machine; canary, property and snapshot
  tests guard the privacy promise. An issue form and an Action validate posted reports and
  aggregate them on the `stats` branch.
- Local usage log (`wn-daemon::usage`): `wn ask`, `wn mcp`, `wn init` and `wn bench` record answer
  states, latencies, index stats and bench hit rates under the cache directory; `wn ask --no-log`
  and `WN_NO_LOG` turn it off; query events expire after 30 days; query text is never stored.

- `wn bench` (`--history`, the default): replays the repository's own commits against their parent
  trees and reports hit@1/3/10 and MRR for lexical BM25, the model and the rolling, ancestry-safe
  personal adapter, by repository size and era; vectors cached between runs. `wn bench --contextbench
  <dir>` runs prepared ContextBench tasks. Matches the research prototype on ripgrep (lexical and
  model exactly, adapter within 0.3 points).
- `wn-git`: read-only history plumbing for replay (commits with parents, source trees, batched blob
  reads, ancestry among commits).
- `wn model pull | list | remove`: install models from a local directory, an `https://` base URL or
  `hf:owner/repo[@revision]`, verified against their SHA-256 manifest before use; shows the Gemma
  Terms of Use notice on first install. No default source yet (`--source` is required).
- Release pipeline in dry-run form: signed-provenance (on tags), checksummed binaries for macOS
  (Apple silicon), Linux (x86_64, arm64) and Windows, SBOMs, `install.sh` with checksum
  verification, a Homebrew formula template, and crates publishable as `where-next`.
- Rust workspace skeleton with seven crates and a `wn` binary placeholder.
- Index lifecycle state machine with exhaustive transition tests and model-based tests.
- Documentation: quickstart, how it works, privacy and licensing, adding a source, FAQ.
- Benchmark protocols: ContextBench, history replay, agent trial, and metric definitions.
- Launch plan (LAUNCH.md), security policy, and issue and pull request templates.
- `wn-core`: query text, ranking, per-repo adapter, abstain logic and hint budget, with golden parity
  tests against the research prototype; adapter and query lifecycle state machines; repository index
  and query runtime.
- `wn-git`: repository scan (tracked and untracked files), history mining and co-change.
- `wn-sources`: file kinds, multi-language skeletons and symbols, config files, with golden parity tests.
- `wn-embed`: ONNX Runtime inference with manifest verification and a model lifecycle state machine;
  parity with the Python reference, including quantized graphs.
- `wn-daemon`: resident session with background warm-up and refresh, failing open.
- `wn-mcp`: stdio MCP server (`where_next`, `refresh_index`, `status`) and the `wn-mcp-server` binary;
  latency benchmarks.
- Docs: building and testing guide; agent-trial pilot 1 results.
- `wn` command: `init`, `ask` (`--json`, `--context-file`, `--start`), `status`, `train`, `rollback`
  and `mcp` (MCP server over stdio), sharing one index per repository and model with the server.
- Model resolution: `--model`, `$WN_MODEL_DIR`, else the best installed model: `gemma-xl1`, then
  `gemma-g2r`, then `v2b`; the lexical fallback says why it is in use.
- `query_format` v2 in `wn-model.json` (request, last tool output, earlier context), with golden
  parity against the reference; v1 queries drop the oldest context to fit the token window.
- Abstain calibration per model and per query kind (`calibration.json`, with built-in calibrations
  for `v2b`, `gemma-g2r` and `gemma-xl1`); issue-style task starts never abstain.
- Task-start hints skip repositories with fewer than 3,000 source files (`--start-min-files`).
- Resumable first index: checkpoints every 1,024 documents; ranking reads vectors in place.
- `doc_throughput` benchmark example and measured kubernetes numbers in docs/building.md.

### Changed

- README, LAUNCH.md and FAQ now report the agent-trial pilot honestly: hints got a cheap, capable
  agent to the right file sooner, but did not reduce cost or change success, so we make no
  agent-savings claim. The pitch is fast local navigation for people and agents.
- Default model is now the EmbeddingGemma-300M fine-tune `gemma-xl1` (preliminary ContextBench
  held-out hit@3 .76, .80 with the adapter; previously Qwen3-Embedding-0.6B `v2b`, .72 and .78).
- The hard-coded calibrated-model constant was replaced by calibration files.
