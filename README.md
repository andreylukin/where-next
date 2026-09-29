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

From the research prototype. Numbers are **hit@k**: the share of tasks where *at least one* of the
files the real fix changed appears in the top k. They measure a good first pointer, not complete
localization.

| Benchmark | Zero-shot embedding | Fine-tuned | + per-repo adapter |
|---|---|---|---|
| ContextBench, repositories held out from fine-tuning (hit@3) | .54 | .72–.74 | .78–.79 |
| Full-history replay of one multi-language repository, matched set (hit@3) | .32 | .65 | .80 |

Caveats we are still working through:

- **Not yet proven in live agent trials.** A controlled trial measuring whether hints save agents
  time, tokens and cost without hurting success is the next milestone.
- The history replay covers a single project; more repositories are needed.
- Short conversational follow-up requests ("now do the same for the other handler") are much harder
  than issue-style descriptions, and results there are weak so far.

## Licensing

- **Code:** Apache-2.0 ([LICENSE](LICENSE)).
- **Model weights:** distributed separately, not in this repository. The default model is fine-tuned
  from `google/embeddinggemma-300m` and is subject to the
  [Gemma Terms of Use](https://ai.google.dev/gemma/terms). A fully Apache-licensed alternative model is
  planned. See [NOTICE](NOTICE).
- **Datasets:** published separately, each source under its original license.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md): tests first, and anything with a lifecycle is an explicit
state machine tested over every state and event. Please follow the [Code of Conduct](CODE_OF_CONDUCT.md).
