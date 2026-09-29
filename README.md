# where-next

A fast, local "where next" model for coding agents and developers. Given what you are working on,
`wn` ranks the files, functions, configs and docs you are most likely to need next, and it learns
your repositories from their git history, on your machine.

> **Status: early.** This repository is the new home of a research prototype. `wn init`, `wn ask`
> and `wn mcp` work end to end when built from source (the one-line installer below does that) with
> a model directory installed locally; there are no release binaries or published model weights yet. See [PLAN.md](PLAN.md) and
> [docs/building.md](docs/building.md).

## What it will do

- **Hint, don't drive.** You, or an agent such as Claude Code or Codex, ask `wn` where to look; it answers with
  at most 3 paths (under 250 tokens) or abstains when it isn't confident. The agent stays in control.
- **Local-first.** A small embedding model (about 300M parameters) runs on your laptop. Code, logs and
  usage stay on your machine. No telemetry.
- **Learns your repos.** A tiny per-repository adapter is fitted from your commit history in about a
  second on CPU, without re-indexing, and keeps up as the codebase changes.
- **Beyond code.** Configs, logs, docs and CLI help sections are planned resource types.

Quick start:

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
wn init                                  # index the repo, fit the adapter from git history
wn skill sync                            # teach Claude Code / Codex / Cursor when to call `wn`
wn ask "retry the upload when S3 times out"
```

Agents use `wn` through its CLI and a short [skill](skills/where-next/SKILL.md) (see
[docs/skill.md](docs/skill.md)). A background daemon starts on first use and keeps the model and
index warm, so repeated calls are fast; it exits after 15 idle minutes.

### Other clients (MCP)

For clients without skills, `wn mcp` is a thin MCP adapter over the same engine:

```sh
claude mcp add where-next -- wn mcp
```

## Install

One command installs `wn`, and re-running it updates to the latest `main`:

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
```

It clones this repository into a private directory (`~/.local/share/where-next/src`), builds it
with `cargo install --path crates/wn-cli --locked` (asks before installing Rust if `cargo` is
missing), and installs `wn` into `~/.cargo/bin`. The first build takes a few minutes. After that:

```sh
wn update            # fetch main and rebuild if it moved (wn update --check: exit 10 if an update exists)
wn --version         # wn 0.0.1 (abc1234 2026-09-29): the commit it was built from
```

Options: `--ref <branch|tag|sha>`, `--yes`, `--dry-run`, `--uninstall`
(`curl -fsSL …/install.sh | sh -s -- --uninstall`). Models are separate: there are no public
weights yet, so without a model `wn` uses a lexical fallback; install one with
`wn model pull <name> --source <path|url|hf:owner/repo>`. Prebuilt, checksum-verified release
binaries, Homebrew and crates.io are set up but not published yet. Details:
[docs/install.md](docs/install.md).

## Preliminary results

From the research prototype. Numbers are **hit@3**: the share of tasks where *at least one* of the
files the real change touched appears in the top 3. They measure a good first pointer, not complete
localization. Protocols and metric definitions are in [benchmarks/](benchmarks/README.md).

| Benchmark | Zero-shot | Qwen3-Emb-0.6B fine-tuned (+ adapter) | **EmbeddingGemma-300M fine-tuned, the default** (+ adapter) |
|---|---|---|---|
| ContextBench, official 500-task subset | .49 | .73 (.80) | **.76 (.81)** |
| ContextBench, 994 tasks in repositories held out from fine-tuning | .52 | .72 (.78) | **.76 (.80)** |
| History replay, one private multi-language repository with 8 architecture rewrites (1,510 matched commits) | .32 | .65 (.80) | **.70 (.83)** |

Zero-shot is the untrained Qwen3-Embedding-0.6B. Both fine-tunes are trained on outcome labels (commit
message to changed files, issue to the files the fix edited); the EmbeddingGemma-300M default (`gemma-xl1`)
was trained on about 1.1M examples with mined hard negatives and a query layout that puts the last tool
output first. On file-level top 5 it also scores .66 on Multi-SWE-bench (7 languages) and .70 on
SWE-PolyBench. On all 1,136 ContextBench tasks, BM25 scores .37 and SweRankEmbed-Small, the closest
published retriever, .62 zero-shot; on the held-out repositories the Qwen fine-tune beats symbol-aware
lexical search by 20 points (.72 vs .53, confidence interval computed over repositories). All numbers
are preliminary.

About the adapter: on ContextBench it adds about 5 points, and a control with shuffled training pairs
shows that gain comes from learning which descriptions map to which files. On the single-repository
history replay, much of its gain can also be had from simple history and file-frequency signals.

## What we have not shown yet

- **That it saves a coding agent cost or time.** In a [controlled pilot](benchmarks/agent-trial.md) on
  50 SWE-bench Pro tasks with a cheap, capable agent, hints got the agent to a correct file about 2.6
  steps sooner, but success was unchanged (32/50 without hints, 31–32/50 with) and cost per resolved
  task was 5–12% *higher*, within noise. The agent called the tool on its own in only 6–11 of 50 tasks,
  and the per-repo adapter showed no benefit there. The pre-declared bar (at least 15% cheaper, at most
  2 points of success harm) was not met. We do not claim agent savings.
- **Whether it helps where search is the bottleneck:** more expensive agents, very large repositories,
  and people navigating by hand. These are the next trials.
- **Conversational follow-ups** ("now do the same for the other handler"): still weak, about .31 hit@3
  with the default model.
- **Well-calibrated abstention for every kind of query.** Thresholds are now per model and per query
  kind, and issue-style task starts never abstain, but error and conversational queries still use the
  thresholds fitted on commit-message queries from one repository.
- **Breadth.** The history replay covers a single project. For exact strings and identifiers, `rg` is
  usually the better tool; see the [FAQ](docs/faq.md).

## Licensing

- **Code:** Apache-2.0 ([LICENSE](LICENSE)).
- **Model weights:** distributed separately, not in this repository. The default model is fine-tuned
  from `google/embeddinggemma-300m` and is subject to the
  [Gemma Terms of Use](https://ai.google.dev/gemma/terms). A fully Apache-licensed alternative model is
  planned. See [NOTICE](NOTICE).
- **Datasets:** published separately, each source under its original license.

## Share stats to help improve wn

`wn report` builds an anonymous usage report (numbers, fixed labels and buckets: no repository
names, paths, file names, commit messages or queries), shows you **all of it**, and only if you
answer `y` posts it as a GitHub issue from your account. `wn report --dry-run` just shows it.
Aggregated results live in [`SUMMARY.md` on the `stats` branch](https://github.com/andreylukin/where-next/blob/stats/SUMMARY.md).
Every field is listed in [privacy and licensing](docs/privacy-and-licensing.md#exactly-what-a-report-contains).

## Documentation

[Quickstart](docs/quickstart.md) · [How it works](docs/how-it-works.md) ·
[Privacy and licensing](docs/privacy-and-licensing.md) · [Adding a source](docs/adding-a-source.md) ·
[Agent skill and daemon](docs/skill.md) · [FAQ](docs/faq.md) · [Building and testing](docs/building.md) · [Launch plan](LAUNCH.md) · [Changelog](CHANGELOG.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md): tests first, and anything with a lifecycle is an explicit
state machine tested over every state and event. Look for issues labelled
[good first issue](https://github.com/andreylukin/where-next/labels/good%20first%20issue). Please follow
the [Code of Conduct](CODE_OF_CONDUCT.md), and report security issues as described in [SECURITY.md](SECURITY.md).
