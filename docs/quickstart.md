# Quickstart

> **Not installable yet.** This page describes the M1 release. Until then, `wn` prints a
> placeholder. The MCP server can already be built and run from source with a local model; see
> [building.md](building.md#run-the-mcp-server). Follow progress in [PLAN.md](../PLAN.md).

## Install

```sh
brew install where-next        # macOS and Linux
# or
cargo install where-next
# or, for Node-based agent setups
npx where-next --version
```

Release binaries for macOS, Linux and Windows are signed and checksummed. See
[privacy-and-licensing.md](privacy-and-licensing.md#verifying-a-release) for how to verify one.

## First answer

```sh
cd your-repo
wn init
wn ask "retry the upload when S3 times out"
```

`wn init` does three things, once per repository:

1. Downloads the model on first use (and shows its license notice), then verifies its checksum.
2. Indexes the repository: one vector per file and function skeleton. Untracked files are included;
   files ignored by git are not.
3. Fits the per-repo adapter from the repository's recent commit history, in seconds on CPU.

`wn ask` returns at most 3 results, each with a similarity score and a short reason, or abstains
when nothing is confidently relevant. Scores are similarities, not probabilities.

## Try it on your repository in 60 seconds

```sh
wn bench
```

replays your repository's recent commits as if each were a new task (candidates are the files as
they were just before the commit) and shows how often the files that commit changed were in the top
1, 3 and 10 suggestions, for plain lexical search, the model, and the model with your repository's
adapter. It is read-only. See [history replay](../benchmarks/history-replay.md) for the protocol and
flags.

## Use it from an agent

Claude Code:

```sh
claude mcp add where-next -- wn mcp
```

The MCP server keeps the model and index warm, so repeated calls are fast. Configuration for Cursor
and VS Code will be committed to `docs/integrations/` with the M1 release.

## Useful commands

| Command | What it does |
|---|---|
| `wn status` | Index state, model and adapter versions, last refresh |
| `wn ask --json` | Machine-readable output, including abstain and fail-open states |
| `wn ask --no-log` | Don't record this query in the local usage log |
| `wn train` | Refit the adapter now (normally automatic) |
| `wn rollback` | Return to the previous adapter |
| `wn bench` | Replay this repository's history to measure quality on your own code (`--contextbench <dir>` runs the public benchmark) |

## If something is wrong

- **"stale_index"**: files changed since the last refresh. The daemon refreshes in the background;
  `wn status` shows progress.
- **"empty_index" or "unsupported_scope"**: nothing indexable was found for the request. Use your
  usual search; `wn` never reports "nothing relevant" when it simply can't see the files.
- **Model checksum mismatch**: delete the cached model and run `wn init` again.
