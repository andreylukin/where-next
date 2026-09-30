# Adding a source

A source turns something in a workspace into indexable resources. Code files are the first source;
configs, logs, docs (Notion, Markdown), and CLI `--help` sections are next. Sources live in the
`wn-sources` crate.

> There is no plugin trait yet. Today everything lives in
> [`crates/wn-sources/src/lib.rs`](../crates/wn-sources/src/lib.rs): `LANGS` / `lang_of` (which
> extensions are source code), `kind_of` and `CONFIG_NAMES` (source vs config vs skipped),
> `symbols` (definitions per language), and `skeleton` / `config_doc` (the text the model reads). A
> new language or file kind is a change there, with tests next to it. This page describes the
> contract a future plugin trait will formalize.

## What a source provides

1. **Discovery:** list the resources in scope, with a stable ID (for example a path, a page ID, or
   `tool subcommand`) and a version key used to detect changes (a git blob hash, a modification time,
   a last-edited timestamp, a tool version).
2. **Skeleton:** a short text representation that the model reads. Keep it small: a title or path,
   the first descriptive line, and names (headings, symbols, flags). Full contents are not stored.
3. **Kind:** one of `file`, `function`, `config`, `log`, `doc`, `cli_help`, so results can be labelled
   and filtered.
4. **Location:** how the caller opens it (a path and optional line range, a URL, or a command).

## Rules

- **Incremental.** Only changed resources are re-embedded, using the version key.
- **Fail open.** If a source cannot read something, report it as unsupported rather than silently
  skipping it.
- **Respect scope and privacy.** Only index what the user can already access. Never send content off
  the machine.
- **Tests first.** Every source needs tests on a small fixture, and if it has a lifecycle (for example
  a sync with a remote API), an explicit state machine with the exhaustive transition test described
  in [CONTRIBUTING.md](../CONTRIBUTING.md).

## Measuring a new source

A source is only useful if it helps rank the right resource. Add labelled examples (a query and the
resource that answered it) to the benchmark kit so the gain can be measured; see
[benchmarks/README.md](../benchmarks/README.md).
