# where-next plan

## Goal

An open-source, local-first "where next" tool that coding agents and developers can install in one
command. It ranks the resources worth opening next (files, functions, configs, logs, docs, CLI help)
and adapts to each repository from its own history. It should be fast, private, and honest about
when it has nothing useful to say.

## Product principles

1. **Hint-only.** At most 3 paths and 250 tokens per answer. The agent decides what to open.
2. **Abstain when unsure.** When the top result isn't confidently better than the agent's next search,
   return no hint. A wrong hint costs a detour.
3. **Fail open.** If the index is missing, stale or broken, say so in machine-readable form and let
   the caller fall back to ordinary search. Never present "no results" as "nothing relevant".
4. **Local-first.** The model, index, adapter and usage log live on the user's machine. No telemetry.
5. **Scores are similarities, not probabilities.** Output says so.
6. **Every claim is measured.** Quality and latency regressions fail CI like bugs.

## Architecture

One Rust workspace; one static binary, `wn`.

| Crate | Responsibility |
|---|---|
| `wn-core` | Shared types, ranking, abstain logic, the personal adapter, and all state machines |
| `wn-embed` | Embedding backends: ONNX Runtime (`ort`) by default, `candle` for Apple Metal; model download, checksum verification, fingerprinting |
| `wn-git` | History mining with `gix`: commit message to changed files and functions, co-change pairs, ancestry-safe history windows |
| `wn-sources` | Resource plugins: code (tree-sitter skeletons), configs, logs, docs, CLI `--help` sections; untracked files included, deletions removed |
| `wn-daemon` | Resident process keeping the model and index warm; file watching; atomic index snapshots |
| `wn-mcp` | MCP server (`rmcp`) for Claude Code, Codex, Cursor and other agents |
| `wn-cli` | `wn init`, `wn ask`, `wn status`, `wn train`, `wn rollback`, `wn bench`, `wn mcp`, `wn model`, `wn skill`, `wn daemon`, `wn update`, `wn stats`, `wn report` |

Other top-level folders:

- `training/`: the Rust trainer (milestone M2).
- `benchmarks/`: reproducible evaluations and the CI gate.
- `datasets/`: dataset cards and build scripts (data published on the Hugging Face Hub).
- `docs/`: user and contributor documentation.

### How knowledge is stored

- **Model weights** hold a general skill: judging which resource is relevant to a task. They know
  nothing about any particular repository.
- **The index** is the memory: one vector per file or function skeleton, stored per repository and
  updated only for changed files.
- **The personal adapter** is a small query-side linear map fitted on the repository's past commits
  (and later on local usage), in seconds on CPU. It never invalidates the index, because it
  only transforms queries.
- **The usage log** records suggestions and what was actually opened and used, for future adapter
  fits. It is local, opt-in for raw text, supports `--no-log`, and expires after 30 days.

## State machines

Anything with a lifecycle or I/O is an explicit state machine with a complete transition table:

| Machine | States (sketch) |
|---|---|
| Index | Uninitialized → Indexing → Ready ⇄ Stale → Refreshing → Error |
| Model | Missing → Downloading → Verifying → Loaded / Corrupt |
| Adapter | None → Fitting → Active → Invalidated |
| Daemon / MCP session | Starting → Serving → Draining → Stopped |
| Query | Received → Answer / Abstain / FailOpen |

Pure functions (ranking maths, tokenizing, normalization) get property tests instead.

## Testing

- **Tests first** for every change.
- **State machines:** an exhaustive test over every `(state, event)` pair against an independently
  written specification, plus model-based tests (`proptest-state-machine`) that run random event
  sequences against a reference model and check invariants, for example "never serve results from a
  model whose fingerprint doesn't match the index".
- **Snapshots:** `insta` for CLI and MCP output.
- **Git fixtures:** small repositories generated inside tests (renames, deletions, untracked files,
  history windows).
- **CI quality gate:** a fixed benchmark subset with cached vectors runs on every pull request. It fails
  if hit@3 drops more than 1 point or warm p95 latency exceeds its budget (target: 200 ms for an
  unchanged repository through MCP).
- **CI matrix:** macOS arm64, Linux x86_64 and Linux arm64.

## Evaluation

- **Retrieval:** hit@k, recall@k, precision@k and MRR on ContextBench (repositories held out from
  training), with repository-cluster confidence intervals. The history replay ranks each commit's
  changed files against the tree just before that commit, with adapters fitted only on earlier history.
- **Baselines:** symbol-aware lexical search, retrieval of similar past commits, recency, frequency,
  co-change, and a shuffled-query control for the adapter.
- **Live agent trial (the product test):** the same agent with and without hints, on fresh tasks in
  unseen repositories, measuring success, cost, wall time, reads and wrong-hint detours. The gate:
  at least 15% lower cost per resolved task with no more than 2 points of success lost.
  **Status: not met** in three trials with a cheap, capable agent (see
  [benchmarks/agent-trial.md](benchmarks/agent-trial.md)), so `wn` is positioned as navigation for
  people and an opt-in tool for agents, never as an agent cost-saver.

## Licensing

- **Code:** Apache-2.0.
- **Default model:** fine-tuned from `google/embeddinggemma-300m`, distributed separately on the Hugging
  Face Hub under the Gemma Terms of Use. The model card and the download step carry the required notice
  ("Gemma is provided under and subject to the Gemma Terms of Use found at ai.google.dev/gemma/terms"),
  pass the use restrictions on to users, state that the weights were modified, and credit the base model
  without implying Google's endorsement. The weights are never relicensed as Apache.
- **Alternative model:** a fully Apache-licensed base, offered for users who need OSI-only licensing.
- **Datasets:** published separately; each source keeps its original license, listed in its card.
  Private repositories, session logs and usage data are never published.

## Milestones

1. **M1: CLI and MCP at parity with the research prototype.** Fine-tuned model plus per-repo adapter plus
   abstain, running from ONNX, with index, model and adapter state machines, and the CI quality gate.
2. **M2: Rust trainer.** A `candle` trainer for the embedding model and adapter that matches the Python
   reference implementation (same data and seed, hit@3 within 1 point) before Python is dropped.
3. **M3: More resources and many repositories.** Docs, CLI help and logs; route-then-rank across
   repositories (first which repository or service, then which file), with permissions respected.
4. **M4: Public release.** Model and dataset on the Hugging Face Hub, `brew install where-next`, docs site,
   and published benchmark results including the live agent trial.

## Roadmap

- **Evidence still missing:** whether hints help people navigating by hand (time to the first right
  file), and whether they help more expensive agents.
- **Install:** Homebrew, `cargo install where-next`, an `npx` launcher that runs a pinned, verified
  binary; Intel Mac, musl and pre-2.35 glibc Linux builds (all need ONNX Runtime built from source); tested
  MCP configuration for Cursor and VS Code; an MCP registry listing; SBOM and provenance attestations
  on every release.
- **Trust:** a threat model for the MCP server (filesystem scope, prompt-injection text in indexed
  files).
- **Community:** acknowledge new issues within 2 business days and review small pull requests within
  a week; `good first issue` only on bounded tasks with a clear definition of done; a changelog entry
  for every release, with breaking MCP schema changes called out.
