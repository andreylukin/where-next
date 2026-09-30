# where-next (`wn`)

**Ask your repository "where is X?" in plain English and get the 2–3 files to open.**
`wn` is a small command-line tool that runs a local model on your machine. It learns from your
repository's git history which kinds of change touch which files, so its guesses get specific to
*your* code. Your code never leaves your machine.

<!-- DEMO SLOT: a ~15 s recording goes here as docs/demo.gif (cd ripgrep; wn ask "where are gitignore
rules matched against paths"). Until it exists, the real output below does the job. -->

```text
$ wn ask "where are gitignore rules matched against paths"
where-next hints (cosine similarity; adapter on):
0.50  crates/ignore/src/gitignore.rs
0.42  crates/ignore/src/dir.rs
0.42  crates/ignore/src/overrides.rs
```

That is real output on a clone of [ripgrep](https://github.com/BurntSushi/ripgrep). The numbers are
similarity scores used for ranking (higher = closer match), not probabilities.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
```

Prefer to read it first: `curl -fsSLO https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh && less install.sh && sh install.sh`.

| Platform | Status |
|---|---|
| macOS, Apple silicon | Prebuilt binary |
| Linux x86_64 / arm64 with glibc 2.39+ (Ubuntu 24.04+, Debian 13+, Fedora 40+) | Prebuilt binary |
| Older Linux (Ubuntu 22.04, Debian 12, RHEL 9, Amazon Linux 2023), musl | **Not supported yet.** The ONNX Runtime library `wn` links needs glibc 2.38+ and GCC 13, so building from source fails there too. |
| Intel Mac | **Not supported yet** (no prebuilt ONNX Runtime for `x86_64-apple-darwin`). |
| Windows | Untested. No installer and no background daemon; see [docs/install.md](docs/install.md). |

The installer downloads a checksum-verified binary when one exists for your platform, and otherwise
builds from source (it asks before installing Rust; the first build takes a few minutes). Then it
asks before downloading the model.

**What it needs:** the model is a separate **~1.2 GB download** ([gemma-xl1](https://huggingface.co/lukandrey/where-next-gemma-xl1),
under the [Gemma Terms of Use](#licensing)). While running, `wn` keeps it in a background process
that uses **about 1.2–1.6 GB of RAM** and exits after 15 idle minutes. Without the model, `wn` falls
back to plain keyword matching, which is much weaker, and says so. Install it any time with
`wn model pull`.

More options (pinning a version, uninstalling, verifying releases): [docs/install.md](docs/install.md).

## Quick start

```sh
cd your-repo
wn init        # one-time: index this repo and learn from its git history
wn ask "where is the retry logic for upload timeouts"
```

`wn` only works inside a git repository (it refuses other directories, and your home directory,
rather than indexing everything under them). `wn init` takes seconds on a small repository and a few
minutes on a big one (about 6.5 minutes for kubernetes' ~20k indexed files on a laptop CPU); it
prints progress while it works and is incremental after that. Once the background daemon is warm, a
`wn ask` takes about 80–100 ms end to end (p50; 180–200 ms p95, measured on a 3,000-file repository
under load; see [docs/skill.md](docs/skill.md#the-daemon)).

## Honest status

- **Early.** Version 0.0.1; expect rough edges, and please [tell us how your first run went](https://github.com/andreylukin/where-next/issues/new?template=first_run.yml).
- **Answers are hints.** At most 3 files; open them and check. When nothing scores above a
  calibrated threshold, `wn` says it has no confident match, and you use your normal search.
- **We have not shown it saves coding agents money or time.** In three controlled trials,
  automatic start hints got a cheap agent to a right file sooner but didn't lower cost or raise
  success. It's a navigation tool you (or your agent) can call, not an agent cost-saver. Details in
  [Limitations](#limitations-and-what-we-havent-shown).
- **Use `rg` for exact names.** `wn` ranks by meaning; if you know the identifier or string, grep
  is exact and faster. `wn` is also weak on vague follow-ups like "now the other one": ask full
  questions.

## Why you might want it

- **You know *what* you're looking for but not *what it's called*.** "Where do we retry uploads?",
  "which config sets the port?". `grep` needs the right word; `wn` needs a description.
- **You're new to a codebase** and want a starting point instead of reading the tree.
- **You use a coding agent** (Claude Code, Codex, Cursor) and want it to have a quick, local
  "where should I look?" tool. `wn skill sync` teaches it when to call `wn`.

## How good is it? Check on your own repo

`wn bench` replays your repository's recent commits: for each one, it asks "given this commit
message, which files would you open?" and checks whether a file the commit actually changed was in
the top 1, 3 or 10. It only reads history and changes nothing. The first run embeds old file
versions, so it can take several minutes on a laptop CPU; reruns reuse the cached vectors. On the
ripgrep clone (300 commits):

| How files were ranked | right file in top 1 | in top 3 | in top 10 |
|---|---|---|---|
| Keyword search (BM25) | 27% | 49% | 74% |
| `wn` model | 39% | 74% | 92% |
| `wn` model + what it learned from ripgrep's history | **65%** | **81%** | **94%** |

Your numbers will differ; that's the point of running it. Public benchmark results are
[further down](#preliminary-results).

## Using it with an agent

```sh
wn skill sync                                  # Claude Code, Codex, Cursor: installs a short skill; shows the diff and asks first
claude mcp add -s user where-next -- wn mcp    # or, for MCP-only clients
```

Agents call `wn ask --json "<question>"` and get up to 3 files plus a `state` field
(`ok`, `abstain`, …). The skill tells them to use `rg` instead for exact names. See
[docs/skill.md](docs/skill.md).

### Writing good queries

- **One self-contained sentence** about what you're looking for, plus any error text:
  `wn ask "why does the upload fail" --context-file error.txt`. Terse follow-ups such as
  "now the other one" have nothing to match.
- **Exact names and strings → `rg`.** `wn` ranks by meaning.
- **Scores rank the files; they aren't probabilities.** Open the files and check.
- **"No confident match"** means `wn` chose not to guess; use your usual search. `--strict` makes it
  stay quiet more often (fewer, more precise answers); `--no-abstain` always shows its best 3.
- **`--json`** for scripts and agents (`state`, `files`, `adapter`), including when `wn` has no
  answer or no index.

## Troubleshooting

| Symptom | What to do |
|---|---|
| `wn: command not found` right after installing | Open a new terminal. The binary is in `~/.local/bin` (prebuilt) or `~/.cargo/bin` (built from source); make sure that directory is on your `PATH` (`source ~/.cargo/env` after a fresh Rust install). |
| Build fails with linker errors mentioning `GLIBC_2.38`, `__isoc23_strtol` or `__cxa_call_terminate` | Your Linux is older than `wn` supports (glibc 2.39+ / Ubuntu 24.04+ for binaries). See the platform table above. |
| `wn` refuses to run: not a git repository, or your home directory | `cd` into a git repository, or pass `--path <repo>`. `wn` won't index arbitrary directories. |
| Answers look like keyword matches | `wn status`: `lexical fallback` means no model is installed; run `wn model pull`, then `wn init` again in repositories you already indexed. |
| `wn init` seems slow | The first index of a big repository takes minutes (it prints progress). Later runs only re-embed changed files. |
| Files you just added or changed are missing | Normally picked up in the background; `wn status` shows the index state, and `wn init` re-indexes now. |
| Something about the daemon seems off | `wn daemon status`; `wn daemon stop` (it restarts on the next call); `--no-daemon` or `WN_NO_DAEMON=1` answers in-process. Log: `~/.cache/where-next/daemon.log`. |
| You don't want queries logged locally | `wn ask --no-log`, or set `WN_NO_LOG=1` (the log feeds `wn stats` and `wn report`; query text is never stored). |
| Update, reinstall or remove | `wn update` (or re-run the installer); uninstall with `curl -fsSL …/install.sh \| sh -s -- --uninstall`. |
| Model checksum mismatch | `wn model remove <name>`, then `wn model pull` it again. |
| "not inside a git repository" or "refusing to index" your home directory | `wn` only indexes git repositories, and never `~` or `/` by default: run it in the project (or `--path <repo>`); `--any-dir` indexes the directory anyway. |

More in [docs/quickstart.md](docs/quickstart.md#if-something-is-wrong) and the [FAQ](docs/faq.md).

## Preliminary results

How often a file the real fix touched was in `wn`'s top 3 (**hit@3**). It measures a good first
pointer, not complete localization. Protocols and metric definitions are in
[benchmarks/](benchmarks/README.md).

| Benchmark | Untrained base model | **The default model, gemma-xl1** (+ repo adapter) |
|---|---|---|
| ContextBench, official 500-task subset | .49 | **.76 (.81)** |
| ContextBench, 994 tasks in repositories held out from fine-tuning | .52 | **.76 (.80)** |
| History replay, one private repository (1,510 commits)¹ | .32 | **.70 (.83)** |

¹ A multi-language repository with 8 architecture rewrites.

The untrained baseline is Qwen3-Embedding-0.6B zero-shot. The default model (`gemma-xl1`, fine-tuned
from EmbeddingGemma-300M) is trained on outcome labels (commit message to changed files, issue to the
files the fix edited): about 1.1M examples with mined hard negatives. On file-level top 5 it also
scores .66 on Multi-SWE-bench (7 languages) and .70 on SWE-PolyBench. On all 1,136 ContextBench tasks,
BM25 scores .37 and SweRankEmbed-Small, the closest published retriever, .62 zero-shot. Adding
training data past about 1.1M examples (mostly more commits) did not help issue-style queries; see
[training-data size](benchmarks/README.md#training-data-size). Results for other models we trained
are in [benchmarks/](benchmarks/README.md). All numbers are preliminary.

About the adapter (what `wn` learns from your history): on ContextBench it adds about 5 points, and a
control with shuffled training pairs shows that gain comes from learning which descriptions map to
which files. On the single-repository history replay, much of its gain can also be had from simple
history and file-frequency signals.

## Limitations and what we haven't shown

**We haven't shown it saves coding agents cost or time.** In three
[controlled trials](benchmarks/agent-trial.md), automatic start hints got a cheap, capable agent to a
right file sooner but didn't lower cost or raise success, so we don't claim agent savings:

- a 50-task pilot on SWE-bench Pro: success unchanged, cost per resolved task 5–12% *higher* (within
  noise);
- 50 tasks on large repositories (median about 5,400 files): cost ratio 0.89, interval 0.67–1.19;
- a **pre-declared confirmatory trial** on 58 new tasks from 17 repositories with 3,038–13,649 files:
  cost per resolved task 1.05× the no-hint baseline (0.82–1.37; 0.82–1.50 by repository), 27 vs 30
  solved (−5.2 points, −13.8 to +3.4). The hint roughly halved the tokens spent before the agent first
  read a right file (10.2k vs 18.6k median), but that didn't turn into lower cost or more solves, and
  the size trend suggested by the second trial did not replicate.

So `wn` is positioned as fast local navigation for people and an **opt-in** tool for agents: the
start hint (`wn ask --start`, or the optional Claude Code hook) is off by default.

Also not shown or not good yet:

- **Whether it helps people navigating by hand, or more expensive agents.** Not yet measured; these are
  the open questions.
- **Conversational follow-ups** ("now do the same for the other handler") score about .31 hit@3. They
  are not a target: callers are asked to send self-contained queries instead.
- **Well-calibrated abstention for every kind of query.** Thresholds are per model and per query kind,
  fitted on held-out queries; some strict thresholds miss their precision target on the small test
  split. Expect `wn` to sometimes answer when it should have stayed quiet, and the reverse.
- **Breadth.** The history replay covers a single project. Only source and config files are indexed
  today (docs, logs and CLI help are planned). For exact strings and identifiers, `rg` is usually the
  better tool; see the [FAQ](docs/faq.md).

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
Aggregated results will be published as `SUMMARY.md` on a `stats` branch once reports arrive.
Every field is listed in [privacy and licensing](docs/privacy-and-licensing.md#exactly-what-a-report-contains).

## Documentation

[Quickstart](docs/quickstart.md) · [How it works](docs/how-it-works.md) ·
[Privacy and licensing](docs/privacy-and-licensing.md) · [Adding a source](docs/adding-a-source.md) ·
[Install](docs/install.md) · [Agent skill and daemon](docs/skill.md) · [FAQ](docs/faq.md) ·
[Building and testing](docs/building.md) · [Changelog](CHANGELOG.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md): tests first, and anything with a lifecycle is an explicit
state machine tested over every state and event. Look for issues labelled
[good first issue](https://github.com/andreylukin/where-next/labels/good%20first%20issue). Please follow
the [Code of Conduct](CODE_OF_CONDUCT.md), and report security issues as described in [SECURITY.md](SECURITY.md).
