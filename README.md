# where-next

A fast, local "where next" model for coding agents and developers. Given what you are working on,
`wn` ranks the files, functions and configs you are most likely to need next, and it learns
your repositories from their git history, on your machine.

> **Status: early.** This repository is the new home of a research prototype. `wn init`, `wn ask`
> and `wn mcp` work end to end when built from source; the one-line installer below does that and
> downloads the default model. There are no release binaries yet. See [PLAN.md](PLAN.md) and
> [docs/building.md](docs/building.md).

## What it does

- **Hints, doesn't drive.** You, or an agent such as Claude Code or Codex, ask `wn` where to look; it
  answers with at most 3 paths (under 250 tokens) or abstains when it isn't confident. The agent stays
  in control.
- **Local-first.** A small embedding model (about 300M parameters) runs on your laptop. Code, logs and
  usage stay on your machine. No telemetry.
- **Learns your repos.** A tiny per-repository adapter is fitted from your commit history in about a
  second on CPU, without re-indexing, and keeps up as the codebase changes.
- **Code and configs today.** Source files (and, with `--functions`, functions) and config files are
  indexed; docs, logs and CLI help sections are planned resource types.

## Quick start

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
cd your-repo
wn init                                  # index the repo, fit the adapter from git history
wn status                                # which model answers (see "About models" below)
wn ask "where are gitignore rules matched against paths"
wn bench                                 # try it on your repo: replay its history, measure hit@k
wn skill sync                            # teach Claude Code / Codex / Cursor when to call `wn`
```

**About models.** The installer downloads the default model,
[gemma-xl1](https://huggingface.co/lukandrey/where-next-gemma-xl1) (~1.2 GB, Gemma Terms of Use),
after asking. Without a model, `wn` answers with a **lexical fallback** (keyword matching), which is
much weaker; `wn status` and `wn init` say which one is in use:

```text
model: gemma-xl1 (gemma-xl1-30ae960f08a8d9e8-model-wn-sources-v1)      # a model is installed
model: hash-bow2-1024 (no model installed → run `wn model pull` …)     # no model
```

`wn model pull` installs (or upgrades) the default model; `wn` then uses it automatically. See
[Install](#install) and [docs/install.md](docs/install.md).

A real answer, on a clone of [ripgrep](https://github.com/BurntSushi/ripgrep) with the default model:

```text
$ wn ask "where are gitignore rules matched against paths"
where-next hints (cosine similarity; adapter on):
0.50  crates/ignore/src/gitignore.rs
0.42  crates/ignore/src/dir.rs
0.42  crates/ignore/src/overrides.rs
```

**Try it on your own repository.** `wn bench` replays your recent commits as if each were a new task
(candidates are the files as they were just before the commit; the adapter only learns from earlier
commits) and reports how often a file the commit changed was in the top 1, 3 and 10. It is read-only.
On the same ripgrep clone (about 45 s):

```text
with the personal adapter (same 300 tasks)
                      hit@1  hit@3 hit@10    MRR      n
  lexical (BM25)      0.267  0.487  0.743  0.420    300
  model               0.387  0.737  0.923  0.586    300
  model + adapter     0.647  0.810  0.940  0.747    300
```

Agents use `wn` through its CLI and a short [skill](skills/where-next/SKILL.md) (see
[docs/skill.md](docs/skill.md)). A background daemon starts on first use and keeps the model and
index warm, so repeated calls are fast; it exits after 15 idle minutes.

### Writing good queries

- **Say what you are looking for, in one self-contained sentence**, plus any error text you have:
  `wn ask "where is the retry logic for S3 upload timeouts"`, or pass the error with
  `wn ask "why does the upload fail" --context-file error.txt`. Terse follow-ups such as
  "now the other one" have nothing to match; agents should rephrase them.
- **Use `rg`/grep for exact strings and identifiers.** `wn` ranks by meaning; if you already know the
  symbol name, search for it.
- **Results are hints.** Open the files and check; scores are cosine similarities, not probabilities.
- **"No confident hint" means `wn` abstained**: nothing scored above the calibrated threshold for
  that kind of query, so use your usual search. `--strict` abstains more (fewer, more precise
  answers); `--no-abstain` always answers.
- **`--start` is an opt-in task-start hint for large repositories.** It is skipped below 3,000 source
  files. In agent trials it didn't lower cost at any size, so it is off unless you ask for it.
- **`--json`** gives machine-readable output (`state`, `files`, `adapter`), including abstain and
  fail-open states.

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
(`curl -fsSL …/install.sh | sh -s -- --uninstall`). The installer then offers the default model,
[gemma-xl1](https://huggingface.co/lukandrey/where-next-gemma-xl1) (~1.2 GB, fine-tuned from
EmbeddingGemma, Gemma Terms of Use), and verifies it against its SHA-256 manifest. `--yes` accepts,
`--no-model` skips; install or upgrade it later with `wn model pull`. Without a model `wn` uses a
lexical fallback and says so. Prebuilt, checksum-verified release
binaries, Homebrew and crates.io are set up but not published yet. Details:
[docs/install.md](docs/install.md).

## Troubleshooting

| Symptom | What to do |
|---|---|
| Answers look like keyword matches | `wn status`: `no model installed` means no model is in use; run `wn model pull` (see "About models" above). |
| Files you just added or changed are missing | Normally picked up in the background; `wn status` shows the index state, and `wn init` re-indexes now. |
| Something about the daemon seems off | `wn daemon status`; `wn daemon stop` (it restarts on the next call); `--no-daemon` or `WN_NO_DAEMON=1` answers in-process. Log: `~/.cache/where-next/daemon.log`. |
| You don't want queries logged locally | `wn ask --no-log`, or set `WN_NO_LOG=1` (the log feeds `wn stats` and `wn report`; query text is never stored). |
| Update, reinstall or remove | `wn update` (or re-run the installer); uninstall with `curl -fsSL …/install.sh \| sh -s -- --uninstall`. |
| Model checksum mismatch | `wn model remove <name>`, then `wn model pull` it again. |

More in [docs/quickstart.md](docs/quickstart.md#if-something-is-wrong) and the [FAQ](docs/faq.md).

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
lexical search by 20 points (.72 vs .53, confidence interval computed over repositories). Adding training data past
about 1.1M examples (mostly more commits) did not help issue-style queries; see
[training-data size](benchmarks/README.md#training-data-size). All numbers are preliminary.

About the adapter: on ContextBench it adds about 5 points, and a control with shuffled training pairs
shows that gain comes from learning which descriptions map to which files. On the single-repository
history replay, much of its gain can also be had from simple history and file-frequency signals.

## What we have not shown yet

- **That it saves a coding agent cost or time.** Three [controlled trials](benchmarks/agent-trial.md)
  with a cheap, capable agent found no benefit from automatic start hints, at any repository size tested:
  - a 50-task pilot on SWE-bench Pro: success unchanged, cost per resolved task 5–12% *higher* (within
    noise);
  - 50 tasks on large repositories (median about 5,400 files): cost ratio 0.89, interval 0.67–1.19;
  - a **pre-declared confirmatory trial** on 58 new tasks from 17 repositories with 3,038–13,649 files:
    cost per resolved task 1.05× the no-hint baseline (0.82–1.37; 0.82–1.50 by repository), 27 vs 30
    solved (−5.2 points, −13.8 to +3.4). The hint roughly halved the tokens spent before the agent first
    read a right file (10.2k vs 18.6k median), but that didn't turn into lower cost or more solves, and
    the size trend suggested by the second trial did not replicate.

  So we don't claim agent savings. `wn` is positioned as fast local navigation for people and an
  **opt-in** tool for agents: the start hint (`wn ask --start`, or the optional Claude Code hook) is off
  by default.
- **Whether it helps people navigating by hand, or more expensive agents.** Not yet measured; these are
  the open questions.
- **Conversational follow-ups** ("now do the same for the other handler") score about .31 hit@3. They
  are not a target: callers are asked to send self-contained queries instead.
- **Well-calibrated abstention for every kind of query.** Thresholds are per model and per query kind:
  issue-style task starts never abstain, error queries have their own thresholds (fitted on about 1.2k
  held-out error queries; some strict thresholds miss their precision target on the small test split),
  and conversational queries use the defaults.
- **Breadth.** The history replay covers a single project. For exact strings and identifiers, `rg` is
  usually the better tool; see the [FAQ](docs/faq.md).

## Licensing

- **Code:** Apache-2.0 ([LICENSE](LICENSE)).
- **Model weights:** distributed separately, not in this repository. The default model,
  [lukandrey/where-next-gemma-xl1](https://huggingface.co/lukandrey/where-next-gemma-xl1), is
  fine-tuned from `google/embeddinggemma-300m` and is subject to the
  [Gemma Terms of Use](https://ai.google.dev/gemma/terms). A fully Apache-licensed alternative model is
  planned. See [NOTICE](NOTICE).
- **Datasets:** published separately, each source under its original license.

## See how it's doing: `wn stats`

`wn stats` answers "is this helping?" from data already on your machine:

- **Agents used the hint.** For each `wn ask` an agent made (Claude Code and Codex transcripts, read
  locally and never sent), it follows the agent's next 10 tool calls. *Exact* = the agent read, ran
  or edited a hinted file; *near* = a file in the same directory, or the hint's test ↔ source pair;
  *elsewhere* = other files; *no files* = it moved on.
- **You edited a hint:** hinted files that changed in git within a day of the answer.
- **Replay:** the latest `wn bench`, wn against grep-style search on your own past commits.
- Speed, query volume and the model in use.

```text
wn stats · my-service · last 30 days

Agents used the hint  ████████████░░░░░░░░  60% exact · 20% near  (10 answers)
                      on the hinted file: read 3 · ran 1 · edited 2 · took #1 first 4 of 6
                      elsewhere 1 · no files 1 · from 3 Claude Code / 1 Codex sessions
You edited a hint     7 of 15 answers within 24 h (git)
Replay (top 3)        wn + adapter 87% · wn 74% · grep-style 51%  (300 commits, 2 days ago)
Speed                 p50 14 ms · p95 41 ms · 294 files
Queries               17  ····················▂····▅·▁·█  request 12 · error 5 · abstained 2
Model                 gemma-xl1 + adapter (200 commits)
```

(Example output. Shares only become percentages at 5 or more answers; below that you see counts.)
`wn stats --all` shows every repository as a table. `wn stats --share` prints a redacted card with
no repository names, paths or queries (it describes the repository as, say, "a ~300-file Python
repo"), and `wn stats --share --svg card.svg` writes the same card as an image to post.
`--no-agents` (or `WN_STATS_NO_AGENTS=1`) skips reading transcripts.

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
