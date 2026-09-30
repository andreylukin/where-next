# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Breaking changes to the MCP tool schema are called out
explicitly.

## [Unreleased]

### Fixed

- `wn bench` fetches missing historical blobs in one request on partial clones, skips and counts
  commits whose candidate files remain unreadable, and discards vectors cached from empty blob text.

## [0.1.1] - 2026-09-30

### Added

- **`wn setup`** connects Claude Code, Codex and Cursor in one step: the where-next skill plus hooks
  that add wn's hints to the agent's context on every prompt (Claude Code, Codex) and after a
  search that found nothing or more than 30 results (all three). At most 3 confident paths, each
  once per session; silent on abstain, without an index or model, outside git, or with
  `WN_HOOKS=0`; 1.5 s budget, always exits 0. Shows every file it writes and asks once;
  `--uninstall` removes only its own entries (byte for byte what was there before, when nothing
  else changed). `wn skill sync` is the same command.
- **`wn stats`** opens with a hooks line (injections, and after how many the agent then opened a
  hinted file) or a `wn setup` nudge; a *Hooks* row shows files, median latency, quiet and timed
  out runs.
- **`wn uninstall`** removes everything wn added (agent skill and hooks, daemon, caches, models,
  source checkout, binary), after showing the list; `--keep-models`. `install.sh --uninstall` runs
  it.
- A session-start hook (all three agents) starts the daemon and loads the model in the background,
  so the first prompt's hook does not time out.
- The installer asks to connect detected agents after the model step (default yes; `--yes`
  connects; skipped without a terminal). `wn init` suggests `wn setup` when no agent is connected.

### Changed

- The default model is pinned to the Hugging Face revision that ships `calibration.json` (abstain
  thresholds, checksum-verified). Weights are unchanged; `wn model pull` upgrades an existing install.
- The old `--with-hook` Claude Code hook (first prompt only, 3,000+ files) is replaced by the
  hooks above; `wn update` moves existing installs over.

## [0.1.0] - 2026-09-30

The first release. The launch changes (#32–#39) come first, then everything else in this version.

### Launch changes

- **Readable `ask` text** (#43): no header line; one hint per line, path first with the score
  aligned at the end; abstain and fail-open states are single lines. Colors on a terminal only
  (`NO_COLOR` and `CLICOLOR_FORCE` honored); new global `--color auto|always|never`. `--json` is
  unchanged.
- **Abstention works again** (#34): with the repository adapter on, `wn ask` says "no confident hint"
  on vague or gibberish queries instead of always answering; rows that would print as `0.00` are
  never shown; an empty query is rejected (exit 2) unless `--context-file` supplies context.
- **Truthful `ask` flags and exact-match evidence** (#33): `-k` accepts 1–3 and rejects larger
  values; `--functions` reserves a hint slot for a definition (its first call indexes every
  definition). A file that literally contains a distinctive name from the query is marked `(exact)`
  in text output and `"evidence": "exact"` in JSON (similarity stays the raw cosine), and can be
  ranked higher. Test and example files are down-weighted unless the query asks for them. CLI and
  MCP share one answer path.
- **Git guard, progress and a responsive daemon** (#36): `init`, `status`, `ask`, `train`,
  `rollback`, `bench` and `mcp` refuse (exit 2) a path outside a git repository, or your home
  directory or `/`; `--any-dir` overrides. `wn mcp` started outside a repository keeps running and
  answers each call with that error. Model loading and indexing print progress on stderr. The
  daemon locks per repository, so a big first index no longer blocks other repositories, and
  `wn daemon status` reports "busy" instead of "not running". Index failures exit 1 with a message.
- **Release binaries by default** (#35, #39): `install.sh` installs a checksum-verified release
  binary into `~/.local/bin` and falls back to a source build only when no binary is published for
  the platform. Linux binaries bundle ONNX Runtime and run on glibc 2.35+ (Ubuntu 22.04+,
  Debian 12+). Older glibc and Intel Macs are refused before anything is downloaded. `wn update`
  re-runs the installer for release installs. `wn model pull` shows progress and resumes
  interrupted downloads.
- **Launch docs** (#32): the README opens with a demo recorded on kubernetes (`docs/demo.tape`),
  install, quick start and an honest status. Platforms, the ~1.2 GB model and daemon memory are
  stated up front, and LAUNCH.md was removed. The skill and the MCP tool description treat
  `--start` as opt-in and say when to use `rg`. There is a new "First run" issue template.

### Added

- `wn stats`: whether agents acted on wn's hints, from Claude Code and Codex transcripts read
  locally (each answer is exact / near / elsewhere / no files, with read / ran / edited on the hinted
  file and whether the #1 hint came first), whether you edited a hinted file within a day (git),
  the latest `wn bench` replay against grep-style search, latency and query volume. `--all` shows a
  per-repository table, `--days N` changes the window, `--json` prints everything, and
  `--share` / `--svg FILE` render a redacted card (text or a self-contained SVG) with no repository
  names, paths or queries. `--no-agents` / `WN_STATS_NO_AGENTS=1` skip transcripts. `wn bench` now
  records how many commits it scored.
- Usability docs: the README quick start states the model situation up front (a lexical fallback
  unless a model is installed; `wn status` shows which), adds `wn bench` as the
  "try it on your repo" step with real sample output, query-writing tips, and a Troubleshooting table.
  `wn --help`, `wn init`, `wn ask`, `wn bench`, `wn report` and `wn skill sync` now end with examples
  (snapshot-tested). `docs/quickstart.md` no longer claims `wn init` downloads a model.
- The default model is public: [gemma-xl1](https://huggingface.co/lukandrey/where-next-gemma-xl1)
  (Gemma Terms of Use). `wn model pull` with no arguments installs it from a pinned revision (still
  verified against its manifest), is a no-op when current and upgrades when the pin moves;
  `--check` exits 10 when a download is needed; `wn model list` shows known models to pull.
  `install.sh` offers the model after building (`--yes`, `--no-model`, `WN_MODEL_SOURCE`), `wn update`
  notes a model update without downloading it, and `wn ask` / `wn status` without a model say to run
  `wn model pull`.

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
  Terms of Use notice on first install.
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
- Model resolution: `--model`, `$WN_MODEL_DIR`, else the best installed model (`gemma-xl1` first);
  the lexical fallback says why it is in use.
- `query_format` v2 in `wn-model.json` (request, last tool output, earlier context), with golden
  parity against the reference; v1 queries drop the oldest context to fit the token window.
- Abstain calibration per model and per query kind (`calibration.json`, with built-in calibrations
  for `gemma-xl1` and earlier research models).
- Task-start hints skip repositories with fewer than 3,000 source files (`--start-min-files`).
- Resumable first index: checkpoints every 1,024 documents; ranking reads vectors in place.
- `doc_throughput` benchmark example and measured kubernetes numbers in docs/building.md.

### Changed

- Docs: the confirmatory agent trial (pre-declared, 58 tasks in repositories with 3,000+ files) found no
  cost or success benefit from automatic start hints (cost per resolved task 1.05×, 0.82–1.37). README,
  LAUNCH, FAQ and the agent-trial write-up now say so; the start hint is described as opt-in, and the
  pilot's "2.6 steps sooner" figure is corrected.

- README, LAUNCH.md and FAQ now report the agent-trial pilot honestly: hints got a cheap, capable
  agent to the right file sooner, but did not reduce cost or change success, so we make no
  agent-savings claim. The pitch is fast local navigation for people and agents.
- Default model is now the EmbeddingGemma-300M fine-tune `gemma-xl1` (preliminary ContextBench
  held-out hit@3 .76, .80 with the adapter; previously a Qwen3-Embedding-0.6B fine-tune, .72 and .78).
- The hard-coded calibrated-model constant was replaced by calibration files.
