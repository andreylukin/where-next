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

Run it inside a git repository: `wn` never indexes your home directory or another plain directory
as one big repository (to search many repositories at once, see
[below](#many-repositories-from-your-home-directory)). `wn init` does two things, once per
repository:

1. Indexes the repository with the installed model (or keyword matching if none is installed): one
   vector per file, plus config files. Function vectors are built the first time you use
   `wn ask --functions`. Untracked files are included; files
   ignored by git are not. This takes seconds on a small repository and a few minutes on a big one
   (about 6.5 minutes for kubernetes), with progress printed as it goes.
2. Learns from the repository's recent commit history (fits a small per-repo adapter), in seconds
   on CPU.

`wn status` shows which model answers. `wn ask` returns at most 3 files, one per line with its
similarity score at the end, or says "no confident hint" when nothing scores above a calibrated
threshold; then use your normal search. Scores rank the files; they are not probabilities. On a
clone of ripgrep:

```text
$ wn ask "where are gitignore rules matched against paths"
crates/ignore/src/gitignore.rs  0.50
crates/ignore/src/dir.rs        0.42
crates/ignore/src/overrides.rs  0.42

$ wn ask "fix the bug"
no confident hint (request: top similarity 0.07 < 0.20); try rg for exact names, or add detail
```

On a terminal the file names are bold and the scores dimmed; piped output is plain text. Use
`--color always|never` to override (`NO_COLOR` and `CLICOLOR_FORCE` are honored), and `--json`
for scripts and agents.

## Many repositories from your home directory

If you (or your agents) start in `~` and work down into different repositories, index them all
from there once:

```sh
cd ~
wn init        # finds the git repositories up to 4 levels down and indexes each; asks first above 10
wn status      # which repositories are indexed, how many files, how fresh
wn ask "where are hook sessions deduplicated"
```

`wn init` skips hidden directories, `node_modules`, `target`, `vendor`, `build`, `dist` and
`~/Library`, never follows symlinks, and does not look inside a repository for more repositories.
Re-running it refreshes every index (only changed files are re-embedded).

`wn ask` there searches every repository indexed below the directory, each with its own index,
adapter and abstain thresholds, so repositories with nothing confident drop out. The query is
embedded once. Each remaining repository's best file comes first (ordered by score), then the
second-best ones, so the top 3 span repositories instead of one large repository filling the list.
Paths are relative to where you asked, so they open from there, and a last line says which
repositories the hints came from:

```text
~$ wn ask "retry uploads when the request times out"
src/github.com/me/uploader/internal/storage/retry.go              0.48
repos/where-next/crates/wn-sources/tests/fixtures/src/Retry.java  0.37
src/github.com/me/uploader/cmd/main.go                            0.36
3 repositories searched; hints from src/github.com/me/uploader, repos/where-next; no confident hint in 1
```

`--json` adds `repo`, `root`, `repo_path`, per-repository `rank` and the `fused` score to each file,
and a `repos` list with each repository's state and best similarity. Inside a repository nothing
changes: `wn` answers for that repository alone. Nothing is indexed at query time: a repository you
clone later is searched after the next `wn init`. `wn mcp` and the agent hooks started in such a
directory search the same way.

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
- **`--json`** for scripts and agents (`state`, `fallback`, `files` with `similarity` and, for literal
  matches, `"evidence": "exact"`), including when `wn` has no answer or no index. `fallback: true`
  means the lexical encoder answered because no usable model was available; it appears on `ok`
  and `abstain` answers too.

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
(BM25), the model, and the model with your repository's adapter. It leaves the working tree alone;
partial clones may fetch missing historical blobs. The first run embeds old file versions, so it can
take several minutes on a laptop CPU;
reruns reuse the cached vectors. For this sample, use ripgrep commit
`3fce3b5bb0236da2df6d99672afb8a719642eca7` (300 commits):

```sh
git clone --filter=blob:none https://github.com/BurntSushi/ripgrep.git
git -C ripgrep checkout 3fce3b5bb0236da2df6d99672afb8a719642eca7
wn bench --path ripgrep
```

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
wn setup                 # shows what it will write and asks; --yes to skip the question
```

connects the agents found in your home directory (Claude Code, Codex, Cursor, bough): the where-next
skill, and hooks that add wn's hints to the agent's context on each prompt and after a search that
found nothing or too much. A background daemon keeps the model and index warm. `wn stats` shows
what the hooks did; `WN_HOOKS=0` or `wn setup --uninstall` turns them off. Details:
[skill.md](skill.md#how-wn-plugs-into-your-agent).

Clients without skills can use the MCP adapter instead: `claude mcp add -s user where-next -- wn mcp`.

## Useful commands

| Command | What it does |
|---|---|
| `wn status` | Index state, model and adapter versions, last refresh |
| `wn ask --json` | Machine-readable output, including when `wn` has no answer or no index |
| `wn ask --no-log` | Don't record this query in the local usage log |
| `wn setup` | Connect agents: skill + hooks (`--dry-run`, `--uninstall`, `--agent`, `--project`, `--no-hooks`) |
| `wn uninstall` | Remove wn and everything it added (asks first; `--keep-models`) |
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
  never your home directory or `/` as one repository (everything under them would be indexed). Run
  it in the project, pass `--path <repo>`, or run `wn init` there to index the repositories below
  it ([workspace search](#many-repositories-from-your-home-directory)); `--any-dir` indexes the
  directory itself. `wn mcp` started where nothing is indexed still runs and answers every call
  with this error.
- **Daemon trouble**: `wn daemon status`, `wn daemon stop` (it restarts on the next call), or answer
  in-process with `--no-daemon` / `WN_NO_DAEMON=1`. Log: `~/.cache/where-next/daemon.log`.
- **Don't log queries**: `wn ask --no-log` or `WN_NO_LOG=1` (query text is never stored either way).
