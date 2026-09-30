# Quickstart

> **Early (v0.1.0).** Supported on macOS (Apple silicon) and Linux with glibc 2.35+ (Ubuntu 22.04+,
> Debian 12+); see [install.md](install.md#platforms). The installer downloads the default model,
> [gemma-xl1](https://huggingface.co/lukandrey/where-next-gemma-xl1) (~1.2 GB, Gemma Terms of Use),
> after asking; `wn model pull` installs it later. Without a model, `wn` falls back to keyword
> matching, which is much weaker, and says so.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
```

This installs the latest release as a checksum-verified binary in `~/.local/bin` (falling back to
a source build only if no binary is published for your platform). Re-run it, or run `wn update`, to
update. If the installer says `~/.local/bin` isn't on your `PATH`, add it. Details and options:
[install.md](install.md).

## First answer

```sh
cd your-repo
wn init
wn status
wn ask "where are gitignore rules matched against paths"
```

Run it inside a git repository: `wn` refuses other directories and your home directory rather
than indexing everything under them. `wn init` does two things, once per repository:

1. Indexes the repository with the installed model (or keyword matching if none is installed): one
   vector per file, plus config files. Function vectors are built the first time you use
   `wn ask --functions`. Untracked files are included; files
   ignored by git are not. This takes seconds on a small repository and a few minutes on a big one
   (about 6.5 minutes for kubernetes), with progress printed as it goes.
2. Learns from the repository's recent commit history (fits a small per-repo adapter), in seconds
   on CPU.

`wn status` shows which model answers. `wn ask` returns at most 3 files with their similarity
scores, or says "no confident hint" when nothing scores above a calibrated threshold; then use
your normal search. Scores rank the files; they are not probabilities. On a clone of ripgrep:

```text
$ wn ask "where are gitignore rules matched against paths"
crates/ignore/src/gitignore.rs  0.50
crates/ignore/src/dir.rs        0.42
crates/ignore/src/overrides.rs  0.42
```

## Writing good queries

- **One self-contained sentence** about what you're looking for, plus any error text:
  `wn ask "why does the upload fail" --context-file error.txt`. Terse follow-ups such as
  "now the other one" have nothing to match.
- **Exact names and strings → `rg`.** `wn` ranks by meaning.
- **Scores rank the files; they aren't probabilities.** Open the files and check.
- **"No confident hint"** means `wn` chose not to guess; use your usual search. `--strict` makes it
  stay quiet more often (fewer, more precise answers); `--no-abstain` always shows its best guesses.
- **`(exact)`** after a file means a distinctive word from your query (an identifier or a file name)
  literally appears in that file or is its name. Such files can outrank higher-similarity ones, and
  when `wn` otherwise isn't confident they are the only hints it shows.
- **`-k 1`–`-k 3`** sets how many hints you get (3 at most; higher values are rejected).
- **`--functions`** also ranks functions and reserves one hint for a definition. The first call in a
  repository indexes every definition, which can take minutes in a large one.
- **`--json`** for scripts and agents (`state`, `files` with `similarity` and, for literal matches,
  `"evidence": "exact"`), including when `wn` has no answer or no index.

## The demo

The GIF in the README was recorded on a kubernetes clone (~20k indexed files) with a warm daemon. The
queries are the titles (two lightly shortened) of real bug reports
[#141298](https://github.com/kubernetes/kubernetes/issues/141298),
[#142526](https://github.com/kubernetes/kubernetes/issues/142526) and
[#141488](https://github.com/kubernetes/kubernetes/issues/141488); their fixes changed
`replica_calculator.go`, `yaml/decoder.go` and `winkernel/hns.go`, each ranked #1. The last step
shows `rg` is still the right tool for an exact name. Script: [demo.tape](demo.tape).

## Try it on your repository

```sh
wn bench
```

replays your repository's recent commits: for each one, it asks "given this commit message, which
files would you open?" (candidates are the files as they were just before the commit) and shows how
often a file that commit changed was in the top 1, 3 and 10 suggestions, for plain lexical search
(BM25), the model, and the model with your repository's adapter. It only reads history and changes
nothing. The first run embeds old file versions, so it can take several minutes on a laptop CPU;
reruns reuse the cached vectors. On the ripgrep clone (300 commits):

```text
                      hit@1  hit@3 hit@10    MRR      n
  lexical (BM25)      0.267  0.487  0.743  0.420    300
  model               0.387  0.737  0.923  0.586    300
  model + adapter     0.647  0.810  0.940  0.747    300
```

That is 27% → 39% → 65% top 1 and 49% → 74% → 81% top 3. Your numbers will differ; that's the point
of running it. See [history replay](../benchmarks/history-replay.md) for the protocol and flags.

## Use it from an agent

```sh
wn skill sync            # shows what it will write and asks; --yes to skip the question
```

installs the where-next skill for the agents found in your home directory (Claude Code, Codex,
Cursor), so they call `wn ask --json` when they need to find where to work. A background daemon
keeps the model and index warm between calls. Details, the optional Claude Code hook and the
daemon's settings: [skill.md](skill.md).

Clients without skills can use the MCP adapter instead: `claude mcp add -s user where-next -- wn mcp`.

## Useful commands

| Command | What it does |
|---|---|
| `wn status` | Index state, model and adapter versions, last refresh |
| `wn ask --json` | Machine-readable output, including when `wn` has no answer or no index |
| `wn ask --no-log` | Don't record this query in the local usage log |
| `wn skill sync` | Install or update the agent skill (`--dry-run`, `--uninstall`, `--agent`, `--project`) |
| `wn daemon status` | Whether the background daemon runs, and what it keeps warm (`start`, `stop`) |
| `wn train` | Refit the adapter now (normally automatic) |
| `wn rollback` | Return to the previous adapter |
| `wn bench` | Replay this repository's history to measure quality on your own code (`--contextbench <dir>` runs the public benchmark) |

In `--json` answers, each file keeps its raw cosine `similarity`. A file promoted by a rare literal in the query also has `"evidence": "exact"`; other hints omit `evidence`. File order reflects ranking evidence and may differ from similarity order.

## If something is wrong

- **"stale_index"**: files changed since the last refresh. The daemon refreshes in the background;
  `wn status` shows progress.
- **"empty_index" or "unsupported_scope"**: nothing indexable was found for the request. Use your
  usual search; `wn` never reports "nothing relevant" when it simply can't see the files.
- **Answers look like keyword matches**: `wn status` probably says
  ``model: lexical fallback (no model installed; run `wn model pull`)``. Run `wn model pull`, then
  `wn init` again in repositories you already indexed.
- **Model checksum mismatch**: `wn model remove <name>`, then `wn model pull` it again.
- **"not inside a git repository" / "refusing to index"**: `wn` indexes git repositories only, and
  never your home directory or `/` by default (everything under them would be indexed). Run it in
  the project or pass `--path <repo>`; `--any-dir` overrides. `wn mcp` started outside a repository
  still runs and answers every call with this error; register it as `wn --path <repo> mcp`.
- **Daemon trouble**: `wn daemon status`, `wn daemon stop` (it restarts on the next call), or answer
  in-process with `--no-daemon` / `WN_NO_DAEMON=1`. Log: `~/.cache/where-next/daemon.log`.
- **Don't log queries**: `wn ask --no-log` or `WN_NO_LOG=1` (query text is never stored either way).
