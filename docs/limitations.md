# Limitations

What `wn` does badly, and what we have not shown. The measured results are in
[benchmarks/README.md](../benchmarks/README.md).

## Status

- **Early.** v0.1.0; expect rough edges, and please
  [tell us how your first run went](https://github.com/andreylukin/where-next/issues/new?template=first_run.yml).
- **Answers are hints.** At most 3 files; open them and check. When nothing scores above a
  calibrated threshold, `wn` says "no confident hint", and you use your normal search.
- **Use `rg` for exact names.** `wn` ranks by meaning; if you know the identifier or string, grep
  is exact and faster.
- **Platforms.** macOS on Apple silicon and Linux with glibc 2.35+. Older Linux, musl and Intel Macs
  are not supported yet; Windows is untested. See [install.md](install.md#platforms).

## No measured agent savings

**We haven't shown it saves coding agents cost or time.** In three
[controlled trials](../benchmarks/agent-trial.md), automatic start hints got a cheap, capable agent to a
right file sooner but didn't lower cost or raise success, so we don't claim agent savings:

- a 50-task pilot on SWE-bench Pro: success unchanged, cost per resolved task 5–12% *higher* (within
  noise);
- 50 tasks on large repositories (median about 5,400 files): cost ratio 0.89, interval 0.67–1.19;
- a **pre-declared confirmatory trial** on 58 new tasks from 17 repositories with 3,038–13,649 files:
  cost per resolved task 1.05× the no-hint baseline (0.82–1.37; 0.82–1.50 by repository), 27 vs 30
  solved (−5.2 points, −13.8 to +3.4). The hint roughly halved the tokens spent before the agent first
  read a right file (10.2k vs 18.6k median), but that didn't turn into lower cost or more solves, and
  the size trend suggested by the second trial did not replicate.

So `wn` is positioned as fast local navigation for people and an **opt-in** tool for agents. Run
`wn setup` to connect Claude Code, Codex or Cursor with a skill and hooks, or use `wn ask --start`
for a manual task-start hint.

## Not shown, or not good yet

- **Whether it helps people navigating by hand, or more expensive agents.** Not yet measured; these are
  the open questions.
- **Conversational follow-ups** ("now do the same for the other handler") score about .31 hit@3. They
  are not a target: callers are asked to send self-contained queries instead, and `wn` abstains
  rather than guess when a query has nothing to match.
- **Well-calibrated abstention for every kind of query.** Thresholds are per model and per query kind,
  fitted on held-out queries; some strict thresholds miss their precision target on the small test
  split. Expect `wn` to sometimes answer when it should have stayed quiet, and the reverse.
- **Breadth.** The history replay covers a single project. Only source and config files are indexed
  today (docs, logs and CLI help are planned). For exact strings and identifiers, `rg` is usually the
  better tool; see the [FAQ](faq.md).
- **Ranking across repositories.** Workspace search (from `~` or another directory of repositories)
  merges each repository's ranking by rank, not by score, because scores from different adapters
  are not strictly comparable: every matching repository's best file is listed before any
  repository's second, even a strong one. Repositories that abstain drop out. Not benchmarked yet.
- **The adapter's gain on one repository.** On ContextBench a shuffled-pair control shows the adapter
  learns which descriptions map to which files. On the single-repository history replay, much of its
  gain can also be had from simple history and file-frequency signals.
