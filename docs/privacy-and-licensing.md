# Privacy and licensing

## What stays on your machine

Everything. The model runs locally. The index, adapter and usage log are stored under
`~/.cache/where-next/`. There is no account, no login, and no telemetry. The only network access is
the one-time model download, which is checksum-verified. `wn stats` also reads your coding agents'
transcripts on this machine (see below); it stores nothing from them and sends nothing.

## The usage log

- Stored per repository under `~/.cache/where-next/<repo>/`: `usage.jsonl` (one line per answered
  query: time, query kind, answer state, latency, model, whether the adapter applied, the number of
  indexed files and the hinted paths), `usage-index.json` (latest index size, build time, extension
  counts, peak memory), `usage-bench.json` (latest `wn bench --history` hit rates) and
  `usage-repo.json` (the repository root, used only to check locally whether hinted files were
  edited later).
- Query text is never stored.
- `--no-log` skips logging for a single `wn ask`; setting `WN_NO_LOG` (to anything) turns logging
  off everywhere, including `wn mcp`.
- Query events expire after 30 days.
- Nothing in the log is sent anywhere by wn. It only feeds `wn stats` and `wn report`, below.

## `wn stats` and agent transcripts

To show whether agents acted on its hints, `wn stats` reads Claude Code transcripts
(`$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`) and Codex sessions
(`$CODEX_HOME/sessions`, else `~/.codex/sessions`) **read-only**, and only files modified in the
window you ask for. It looks for `wn ask` calls (and where-next MCP calls and start hints), matches
each to an answer in the usage log by time, and checks which files the agent's next 10 tool calls
read, ran or edited. It keeps only counts: nothing from a transcript is written to disk, cached or
sent, and `wn report` does not read transcripts at all.

- `wn stats --no-agents`, or `WN_STATS_NO_AGENTS=1`, skips transcripts entirely.
- `wn stats` (without `--share`) prints repository names because it is for you.
- `wn stats --share` (and `--svg`) builds a separate card type that holds only numbers and fixed
  labels: counts and shares, the most common language, the file count rounded to one significant
  figure, the model name from the published list, and latency. It cannot contain repository names,
  paths, remotes, file names or queries, and a canary test checks that none leak.

## Sharing a usage report (`wn report`)

`wn report` turns the log into an anonymous summary and, only if you confirm, posts it as a GitHub
issue from your own account. It is never automatic and never runs in the background.

1. It builds the report locally and prints **all of it** (a readable summary plus the exact JSON).
2. It asks `Post it? [y/N]`. Anything but `y` posts nothing; so does running without a terminal.
3. On `y` it uses `gh issue create` if the GitHub CLI is installed and logged in, otherwise it opens a
   pre-filled issue in your browser for you to submit.

`wn report --json` prints the JSON only, and `wn report --dry-run` shows the summary and the link;
neither posts anything. Posting from your own account means your GitHub user name is visible on the
issue; that is the one thing the report cannot hide.

### Exactly what a report contains

The report's types have no field that can hold text from a repository. Every value is a number, a
fixed label or a bucket, and a bot rejects posted reports with any extra field or unknown value.

| Section | Field | Values | Why |
|---|---|---|---|
| wn | `version`, `commit` | crate version; 7-hex build commit | Tie results to a build |
| setup | `os`, `arch` | macos / linux / windows / other; aarch64 / x86_64 / other | Platform-specific speed |
| setup | `threads`, `ram` | 1-4 / 5-8 / 9-16 / 17+; <8GB … 64GB+ / unknown | Hardware context for latency |
| setup | `model`, `precision` | gemma-xl1 / gemma-g2r / gemma-g1 / v2b / custom / lexical; fp32 / q8 / none | Which model the numbers describe (custom models are never named) |
| repos | `count` | 0 / 1-9 / 10-49 / 50-199 / 200+ | How many repositories wn was used in |
| repos | `sizes`, `history` | repositories per size bucket (<1k … 10k+ files) and per adapter-history bucket, each as a count bucket | Quality and speed depend on size and history |
| repos | `languages` | % of indexed files per language (python, rust, go, …, other), rounded to 10, below 10 omitted | Which languages need work |
| performance | `index_time`, `peak_memory` | <10s … 5min+; <512MB … 4GB+ | Indexing cost |
| performance | `query_ms_p50`, `query_ms_p95` | milliseconds, rounded | Answer latency |
| quality | `bench` | per repository, up to 10: size bucket, model, hit@1/3/10 for lexical, model and model + adapter, rounded to 0.01 | Real-world accuracy |
| quality | `abstain_rate`, `hint_usefulness`, `hints_checked` | shares rounded to 0.05; count bucket | How often wn answers, and how often hinted files were edited within a day |
| reliability | `states`, `fallback_rate` | share of queries per non-ok state; lexical-fallback share | Failures to fix |
| usage | `kinds`, `queries_per_week`, `adapter_rate` | share per query kind (request / issue / error / conversational); count bucket; share | How wn is used |
| – | `truncated` | true / false | Set when detail was dropped to fit the issue link |

**Never included:** repository or organisation names, remote URLs, file or directory names, paths,
commit messages or authors, query text, code, user or host names, absolute times, or exact counts.

These promises are tested: a canary repository with unique strings in its directory and file names,
commit messages, author, remote URL and queries is indexed, queried and benched, and the report must
contain none of them; a property test does the same with random content; a snapshot pins the exact
format.

Posted reports are validated and aggregated by a GitHub Action into `stats/reports.jsonl` and
`stats/SUMMARY.md` on the repository's `stats` branch (hit@3 by repository size and language,
latency by hardware, abstain rate, hint usefulness).

## Licensing

| Artifact | License | Where |
|---|---|---|
| Code in this repository | Apache-2.0 | [LICENSE](../LICENSE) |
| Default model weights | Gemma Terms of Use | Downloaded separately; never in this repository |
| Alternative model (planned) | Apache-2.0 | Downloaded separately |
| Datasets | Each source under its original license | Published separately with a dataset card |

### The default model

The default model is fine-tuned from `google/embeddinggemma-300m`, so it is a model derivative of
Gemma and is distributed under the [Gemma Terms of Use](https://ai.google.dev/gemma/terms), including
the use restrictions they reference. The model card states that it was modified by where-next. Using
it does not imply any endorsement by Google. `wn init` shows this notice before the first download.

If you need a model without these terms, the planned alternative model is Apache-2.0.

### Datasets

Only public sources are published, each with attribution and its original license in the dataset
card. No private repositories, sessions or usage logs are ever included.

## Verifying a release

Every release will ship SHA-256 checksums, build provenance attestations and an SBOM. Instructions
for verifying them will be added with the first release.

## Reporting a security issue

See [SECURITY.md](../SECURITY.md).
