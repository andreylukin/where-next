# Quickstart

> **Early.** `wn` builds from source today (no release binaries yet). The installer downloads the
> default model, [gemma-xl1](https://huggingface.co/lukandrey/where-next-gemma-xl1) (~1.2 GB,
> Gemma Terms of Use), after asking; `wn model pull` installs it later. Without a model, `wn` uses a
> lexical fallback and says so.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
```

This builds `wn` from the latest `main` (a few minutes the first time) and installs it into
`~/.cargo/bin`. Re-run it, or run `wn update`, to update. Details and options:
[install.md](install.md). Brew, crates.io and prebuilt binaries come with the first release.

## First answer

```sh
cd your-repo
wn init
wn status
wn ask "where are gitignore rules matched against paths"
```

`wn init` does two things, once per repository:

1. Indexes the repository with the best installed model (`gemma-xl1`, else `gemma-g2r`, else `v2b`,
   else the lexical fallback): one vector per file and function skeleton, plus config files.
   Untracked files are included; files ignored by git are not.
2. Fits the per-repo adapter from the repository's recent commit history, in seconds on CPU.

`wn status` shows which model answers. `wn ask` returns at most 3 files with their similarity
scores, or abstains ("no confident hint … use normal search") when nothing scores above the
calibrated threshold. Scores are cosine similarities, not probabilities. On a clone of ripgrep:

```text
$ wn ask "where are gitignore rules matched against paths"
where-next hints (cosine similarity; adapter on):
0.50  crates/ignore/src/gitignore.rs
0.42  crates/ignore/src/dir.rs
0.42  crates/ignore/src/overrides.rs
```

Query tips (self-contained queries, grep for exact strings, `--start`, `--json`) are in the
[README](../README.md#writing-good-queries).

## Try it on your repository in 60 seconds

```sh
wn bench
```

replays your repository's recent commits as if each were a new task (candidates are the files as
they were just before the commit) and shows how often the files that commit changed were in the top
1, 3 and 10 suggestions, for plain lexical search, the model, and the model with your repository's
adapter. It is read-only. On the ripgrep clone (about 45 s):

```text
                      hit@1  hit@3 hit@10    MRR      n
  lexical (BM25)      0.267  0.487  0.743  0.420    300
  model               0.387  0.737  0.923  0.586    300
  model + adapter     0.647  0.810  0.940  0.747    300
```

See [history replay](../benchmarks/history-replay.md) for the protocol and flags.

## Use it from an agent

```sh
wn skill sync            # shows what it will write and asks; --yes to skip the question
```

installs the where-next skill for the agents found in your home directory (Claude Code, Codex,
Cursor), so they call `wn ask --json` when they need to find where to work. A background daemon
keeps the model and index warm between calls. Details, the optional Claude Code hook and the
daemon's settings: [skill.md](skill.md).

Clients without skills can use the MCP adapter instead: `claude mcp add where-next -- wn mcp`.

## Useful commands

| Command | What it does |
|---|---|
| `wn status` | Index state, model and adapter versions, last refresh |
| `wn ask --json` | Machine-readable output, including abstain and fail-open states |
| `wn ask --no-log` | Don't record this query in the local usage log |
| `wn skill sync` | Install or update the agent skill (`--dry-run`, `--uninstall`, `--agent`, `--project`) |
| `wn daemon status` | Whether the background daemon runs, and what it keeps warm (`start`, `stop`) |
| `wn train` | Refit the adapter now (normally automatic) |
| `wn rollback` | Return to the previous adapter |
| `wn bench` | Replay this repository's history to measure quality on your own code (`--contextbench <dir>` runs the public benchmark) |

## If something is wrong

- **"stale_index"**: files changed since the last refresh. The daemon refreshes in the background;
  `wn status` shows progress.
- **"empty_index" or "unsupported_scope"**: nothing indexable was found for the request. Use your
  usual search; `wn` never reports "nothing relevant" when it simply can't see the files.
- **Answers look like keyword matches**: `wn status` probably says `lexical fallback: no model
  installed`; see the note at the top.
- **Model checksum mismatch**: `wn model remove <name>`, then `wn model pull` it again.
- **Daemon trouble**: `wn daemon status`, `wn daemon stop` (it restarts on the next call), or answer
  in-process with `--no-daemon` / `WN_NO_DAEMON=1`. Log: `~/.cache/where-next/daemon.log`.
- **Don't log queries**: `wn ask --no-log` or `WN_NO_LOG=1` (query text is never stored either way).
