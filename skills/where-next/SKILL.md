---
name: where-next
description: Find which files to open or edit next in a repository with the local `wn` CLI. Use when you need to locate where something is implemented, decide which file to change for a task, orient in a large or unfamiliar codebase, or trace an error or stack trace to the code behind it. Not for exact-string lookups (use grep).
---

# where-next (`wn`)

`wn` ranks the files most likely relevant to a task, in milliseconds, from a local model and this
repository's own git history. Treat its answers as hints: open the files and verify.

## When to call it

- Starting a task in a large repository (3,000+ files): `wn ask --start --json "<task>"`.
- You need where something lives: "where is X implemented / configured / tested".
- After an error: include the error text or stack trace in the query.

Don't call it for exact names or strings you already know (`rg "fn parse_config"` is faster and
exact), or in tiny repositories where listing the tree is enough.

## How

```sh
wn ask --json "<self-contained query>"
```

Write a self-contained query: what you are looking for plus any error text, not a terse follow-up.
Good: `wn ask --json "where are S3 upload retries configured; error: ReadTimeoutError in push.py"`.
Bad: `wn ask --json "the other one"`.

Useful flags: `--start` (task-start hint; skipped in small repositories), `--functions` (also rank
functions), `-k 5` (more hints), `--context-file -` (pipe recent tool output on stdin).

## Reading the answer

JSON fields: `state`, `files` (up to 3 `{path, similarity}`), `configs`, `functions` (with
`--functions`), `adapter`, and `abstain` or `error` with a reason.

- `ok`: open the top files first. Similarities rank the files; they are not probabilities.
- `abstain`: not confident enough. Use ordinary search (`rg`, reading the tree).
- `stale_index`: hints from an older index; still useful, verify them.
- `empty_index`, `unsupported_scope`, `error`: nothing usable; search normally.

If `wn` reports the repository is not indexed, run `wn init` once (seconds to a few minutes; it also
learns from git history). `wn status` shows the index, adapter and model.
