---
name: where-next
description: Find which files to open or edit next in a repository with the local `wn` CLI. Use when you need to locate where something is implemented, decide which file to change for a task, orient in a large or unfamiliar codebase, or trace an error or stack trace to the code behind it. Not for exact-string lookups (use grep).
---

# where-next (`wn`)

`wn` ranks the files most likely relevant to a description, from a local model and this
repository's own git history (about 100 ms per call once its background daemon is warm). Treat its
answers as hints: open the files and verify.

## Hints that arrive on their own

With `wn setup`, hooks add a short note to your context that starts with "where-next (local index
of this repository) suggests": after a prompt, and after a search (`rg`, `grep`, `find`, the Grep
or Glob tools) that found nothing or too much. It lists at most 3 files wn is confident about and
never repeats a file in the same session. Open them first when they fit the task; they are hints,
so verify them.

## When to call it

- You need where something lives but don't know what it's called: "where is X implemented /
  configured / tested".
- Orienting in a large or unfamiliar repository: `wn ask --json "<what you need to find>"`.
- After an error: include the error text or stack trace in the query.

Use `rg` instead when you already know an exact identifier, string or error message
(`rg "fn parse_config"` is exact and faster), and skip `wn` in tiny repositories where listing the
tree is enough.

## How

```sh
wn ask --json "<self-contained query>"
```

Write a self-contained query: what you are looking for plus any error text, not a terse follow-up.
Good: `wn ask --json "where are S3 upload retries configured; error: ReadTimeoutError in push.py"`.
Bad: `wn ask --json "the other one"`.

Useful flags: `--context-file -` (pipe recent tool output on stdin), `--functions` (also rank
functions; the first call indexes every definition, which can take minutes in a large repository),
`--start` (opt-in task-start mode; skipped in repositories under 3,000 source files; in trials it did
not lower agent cost, so don't add it by default).

Outside a repository (in `~` or another directory of repositories), `wn ask` searches every
repository indexed below that directory and prints paths relative to it, so they open from there;
in `--json`, `repo` names each file's repository and `repos` lists which repositories matched. Run it
inside a repository to search only that one.

## Reading the answer

JSON fields: `state`, `files` (up to 3 `{path, similarity}`; `"evidence": "exact"` when a name or
identifier from the query literally occurs in that file), `configs`, `functions` (with
`--functions`), `adapter`, and `abstain` or `error` with a reason. File order is the ranking, which
can differ from similarity order.

- `ok`: open the top files first. Similarities rank the files; they are not probabilities.
- `abstain`: nothing scored above the calibrated threshold. Use ordinary search (`rg`, reading
  the tree); rephrasing a vague query as a full sentence can also help.
- `stale_index`: hints from an older index; still useful, verify them.
- `empty_index`, `unsupported_scope`, `error`: nothing usable; search normally.

If `wn` reports the repository is not indexed, run `wn init` once (seconds to a few minutes; it also
learns from git history). `wn status` shows the index, adapter and model; in a directory of
repositories it lists which ones are indexed.
