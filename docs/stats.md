# `wn stats` and `wn report`

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

## Share stats to help improve wn: `wn report`

`wn report` builds an anonymous usage report (numbers, fixed labels and buckets: no repository
names, paths, file names, commit messages or queries), shows you **all of it**, and only if you
answer `y` posts it as a GitHub issue from your account. `wn report --dry-run` just shows it.
Aggregated results will be published as `SUMMARY.md` on a `stats` branch once reports arrive.
Every field is listed in [privacy and licensing](privacy-and-licensing.md#exactly-what-a-report-contains).
