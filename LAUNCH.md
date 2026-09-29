# Launch plan

How where-next goes from a research prototype to a tool people install and keep using. This is a
working document: each item is either done, in progress, or a gate we have not passed yet.

## The one thing that decides it

A stranger installs `wn`, runs it on **their own repository**, and gets a surprisingly good answer in
under 60 seconds. Everything below either makes that moment happen or amplifies it. If strangers
cannot reproduce a good answer without help, we postpone publicity and fix the first run.

**The pitch:** fast, local, private navigation for people and agents. It finds the file you mean in
milliseconds, and learns your repository's history. We don't pitch it as an agent cost saver: our first
controlled trial did not show one (see below).

## P0: first answer in 60 seconds

- `wn init` indexes the repository and fits the per-repo adapter from its git history.
- `wn ask "where does auth happen?"` returns at most 3 ranked files (with line references where we
  have them), a one-line reason for each, and timing.
- It abstains instead of guessing, and falls back to lexical search when the model is unavailable.
- No account, no network after the one-time model download, no telemetry.

**Gate:** tested from a clean machine on macOS arm64 and Linux, on repositories the model has never
seen, by people who did not build it.

## P1: evidence

1. **A demonstrated benefit in real use (launch blocker).** Three agent trials with a cheap, capable
   agent found none: a 50-task pilot, a 50-task large-repository trial, and a pre-declared confirmatory
   trial on 58 new tasks from repositories with 3,000–13,600 files (cost per resolved task 1.05×,
   interval 0.82–1.37; 27 vs 30 solved). Hints roughly halved the tokens spent before the agent first read
   a right file, but not cost or success. So the pitch is **fast local navigation for people, and an
   opt-in tool for agents**, never "saves agent cost". Details:
   [benchmarks/agent-trial.md](benchmarks/agent-trial.md). What could still show a benefit:
   - **People navigating by hand:** time to the first right file on real tasks, since hints did shorten
     that for the agent.
   - **More expensive agents,** where each saved turn is worth more.
2. **Public, reproducible benchmark.** Pinned repositories and tasks, baselines (ripgrep, BM25,
   zero-shot embeddings, SweRankEmbed), cold and warm timings, and the cases where we lose. See
   [benchmarks/](benchmarks/README.md).
3. **Honest limitations** in the README: no demonstrated agent savings yet; short conversational
   follow-ups are weak; exact strings and symbols are often better served by `rg`.

Preliminary numbers we can already stand behind (hit@k = at least one file the real fix changed is
in the top k):

| Benchmark | Zero-shot | Qwen3-Emb-0.6B fine-tuned (+ adapter) | **EmbeddingGemma-300M fine-tuned, the default** (+ adapter) |
|---|---|---|---|
| ContextBench, official 500-task subset | .49 | .73 (.80) | **.76 (.81)** |
| ContextBench, 994 tasks in repositories held out from fine-tuning | .52 | .72 (.78) | **.76 (.80)** |
| History replay, one private multi-language repository with 8 architecture rewrites (1,510 matched commits) | .32 | .65 (.80) | **.70 (.83)** |

All hit@3, preliminary. On the clean ContextBench tasks, symbol-aware lexical search scores .53, 20
points below the Qwen fine-tune. SweRankEmbed-Small, the closest published retriever, scores .62
zero-shot on all 1,136 tasks.

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
   copy-paste Claude Code setup, and what the agent trials did and did not show); r/LocalLLaMA (local model, CPU
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
