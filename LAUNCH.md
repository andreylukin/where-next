# Launch plan

How where-next goes from a research prototype to a tool people install and keep using. This is a
working document: each item is either done, in progress, or a gate we have not passed yet.

## The one thing that decides it

A stranger installs `wn`, runs it on **their own repository**, and gets a surprisingly good answer in
under 60 seconds. Everything below either makes that moment happen or amplifies it. If strangers
cannot reproduce a good answer without help, we postpone publicity and fix the first run.

## P0: first answer in 60 seconds

- `wn init` indexes the repository and fits the per-repo adapter from its git history.
- `wn ask "where does auth happen?"` returns at most 3 ranked files (with line references where we
  have them), a one-line reason for each, and timing.
- It abstains instead of guessing, and falls back to lexical search when the model is unavailable.
- No account, no network after the one-time model download, no telemetry.

**Gate:** tested from a clean machine on macOS arm64 and Linux, on repositories the model has never
seen, by people who did not build it.

## P1: evidence

1. **Agent trial (launch blocker).** A controlled trial of the same agent with and without `wn`
   hints, on tasks from repositories held out from training. We report success rate, billed tokens,
   cost per resolved task, wall time, search and read calls, and wrong-hint detours, including
   failures and timeouts. The pre-declared bar: at least 15% lower cost per resolved task, with no
   more than 2 points of success-rate harm. If the trial does not clear it, we do not claim agent
   savings.
2. **Public, reproducible benchmark.** Pinned repositories and tasks, baselines (ripgrep, BM25,
   zero-shot embeddings, SweRankEmbed), cold and warm timings, and the cases where we lose. See
   [benchmarks/](benchmarks/README.md).
3. **Honest limitations** in the README: short conversational follow-ups are weak; exact strings and
   symbols are often better served by `rg`.

Preliminary numbers we can already stand behind (hit@k = at least one file the real fix changed is
in the top k):

| Benchmark | Zero-shot | Fine-tuned | + per-repo adapter |
|---|---|---|---|
| ContextBench, 994 tasks in repositories held out from fine-tuning (hit@3) | .54 | .72 | .78 |
| ContextBench, all 1,136 tasks (hit@3) | .54 | .74 | .79 |
| History replay, one private repository, matched 1,510 commits (hit@3) | .32 | .65 | .80 |

These come from the Python research prototype (Qwen3-Embedding-0.6B fine-tune). The 300M-parameter
EmbeddingGemma fine-tune scores .70, or .76 with the adapter, on all 1,136 ContextBench tasks, up
from .49 zero-shot. SweRankEmbed-Small, the closest published retriever, scores .62 zero-shot.

## P2: zero-friction install

- Signed, checksummed release binaries for macOS (arm64, x86_64), Linux (x86_64, arm64) and Windows.
- `brew install where-next`, `cargo install where-next`, and an `npx` launcher that runs a pinned,
  verified binary.
- A `curl | sh` installer with checksum verification and package-manager alternatives next to it.
- Tested, copy-paste configuration for Claude Code, Cursor and VS Code, committed to the repository.
- Listed in the MCP registry.
- SBOM and build provenance attestations for every release.

## P3: the README

In this order:

1. A terminal recording on a real repository: the question, the three answers, the elapsed time.
2. Install (one line), then the Claude Code line.
3. The benchmark table with baselines and the cases where we lose.
4. Privacy and licensing, stated exactly.
5. How it works and the architecture, for people who want to contribute.

## Launch waves

Each wave is its own story. We do not post the same copy everywhere, and we follow each community's
self-promotion rules.

1. **Launch:** Show HN (the working tool, the pain, one honest before/after, the benchmark with
   losses); r/rust (how it's built: single binary, ONNX inference, state machines); r/ClaudeAI (a
   copy-paste Claude Code setup and the measured token savings); r/LocalLLaMA (local model, CPU
   speed, model license).
2. **Integration news:** MCP registry listing, Cursor and VS Code guides, editor plugins.
3. **Training write-up:** how the model was trained from outcome labels, plus the dataset and model
   published on the Hugging Face Hub.
4. **Newsletters** (This Week in Rust, TLDR, Console) only after independent users have confirmed it
   works for them.

## Community

- Issues and Discussions on GitHub, so answers stay searchable.
- Triage targets: acknowledge new issues within 2 business days; review small pull requests within
  1 week. If volunteer capacity changes, we say so.
- `good first issue` labels only on bounded tasks with a clear definition of done (for example a new
  source plugin or a language extractor).
- A changelog entry for every release; breaking MCP schema changes are called out.

## Metrics that matter

Stars are a discovery signal, not adoption. We track:

- install completion (download to first answer),
- first correct answer on the user's own repository,
- 7-day return use,
- integrations turned on (MCP configured, editor plugin installed),
- independently reported bugs and pull requests.

## Trust

- No account and no login. Local-first. No telemetry; any future opt-in telemetry will have a
  published event schema.
- Exact license boundaries: code is Apache-2.0; the default model is distributed separately under the
  Gemma Terms of Use; datasets are published per source under their original licenses; no private
  data is ever included.
- Security reporting through [SECURITY.md](SECURITY.md); a threat model for the MCP server
  (filesystem scope, prompt-injection text in indexed files) ships with the first release.

## Common ways this fails, and what we do instead

| Failure | Instead |
|---|---|
| A vague "AI-powered" claim | One concrete job: which file to open next, with a number |
| A cherry-picked microbenchmark | The full task set, baselines, and loss cases |
| Long setup | One command to a first answer |
| A prerelease marketed as production | Clear status labels on every release |
| Too many backends for one maintainer | A small, supported surface; scope cut before burnout |
