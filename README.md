# where-next

A fast, local "where next" model for coding agents and developers. Given what you are working on,
`wn` ranks the files, functions, configs and docs you are most likely to need next, and it learns
your repositories from their git history, on your machine.

> **Status: early.** This repository is the new home of a research prototype. The Rust tool is a
> skeleton; see [PLAN.md](PLAN.md) for the roadmap. Nothing here is ready to install yet.

## What it will do

- **Hint, don't drive.** Agents such as Claude Code or Codex ask `wn` where to look; it answers with
  at most 3 paths (under 250 tokens) or abstains when it isn't confident. The agent stays in control.
- **Local-first.** A small embedding model (about 300M parameters) runs on your laptop. Code, logs and
  usage stay on your machine. No telemetry.
- **Learns your repos.** A tiny per-repository adapter is fitted from your commit history in about a
  second on CPU, without re-indexing, and keeps up as the codebase changes.
- **Beyond code.** Configs, logs, docs and CLI help sections are planned resource types.

Planned setup:

```sh
brew install where-next
wn init                                  # index the repo, fit the adapter from git history
wn ask "retry the upload when S3 times out"
claude mcp add where-next -- wn mcp      # expose it to Claude Code
```

## Preliminary results

From the research prototype. Numbers are **hit@3**: the share of tasks where *at least one* of the
files the real change touched appears in the top 3. They measure a good first pointer, not complete
localization. Protocols and metric definitions are in [benchmarks/](benchmarks/README.md).

| Benchmark | BM25 | Zero-shot | SweRankEmbed-Small | Fine-tuned | + per-repo adapter |
|---|---|---|---|---|---|
| [ContextBench](benchmarks/contextbench.md), all 1,136 tasks | .37 | .54 | .62 | .74 | .79 |
| ContextBench, 994 tasks in repositories held out from fine-tuning | | .54 | | .72 | .78 |
| [History replay](benchmarks/history-replay.md) of one private multi-language repository with 8 architecture rewrites (1,510 matched commits) | | .32 | | .65 | .80 |

Fine-tuned is Qwen3-Embedding-0.6B trained on outcome labels (commit message to changed files, issue
to the files the fix edited). The smaller EmbeddingGemma-300M fine-tune scores .70, or .76 with the
adapter, on all 1,136 ContextBench tasks. On the history replay, ranking by recency alone scores .37,
so the adapter learns more than "what changed recently". Warm MCP queries took 82 to 198 ms at p95 on
repositories of 1,000 to 20,000 files.

Caveats we are still working through:

- **Not yet proven in live agent trials.** A [controlled trial](benchmarks/agent-trial.md) measuring
  whether hints save agents time, tokens and cost without hurting success is the gate before we claim
  savings.
- The history replay covers a single project; more repositories are needed.
- Short conversational follow-up requests ("now do the same for the other handler") are much harder
  than issue-style descriptions, and results there are weak so far.
- For exact strings and identifiers, `rg` is usually the better tool. See the [FAQ](docs/faq.md).

## Licensing

- **Code:** Apache-2.0 ([LICENSE](LICENSE)).
- **Model weights:** distributed separately, not in this repository. The default model is fine-tuned
  from `google/embeddinggemma-300m` and is subject to the
  [Gemma Terms of Use](https://ai.google.dev/gemma/terms). A fully Apache-licensed alternative model is
  planned. See [NOTICE](NOTICE).
- **Datasets:** published separately, each source under its original license.

## Documentation

[Quickstart](docs/quickstart.md) · [How it works](docs/how-it-works.md) ·
[Privacy and licensing](docs/privacy-and-licensing.md) · [Adding a source](docs/adding-a-source.md) ·
[FAQ](docs/faq.md) · [Launch plan](LAUNCH.md) · [Changelog](CHANGELOG.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md): tests first, and anything with a lifecycle is an explicit
state machine tested over every state and event. Look for issues labelled
[good first issue](https://github.com/andreylukin/where-next/labels/good%20first%20issue). Please follow
the [Code of Conduct](CODE_OF_CONDUCT.md), and report security issues as described in [SECURITY.md](SECURITY.md).
