# Metrics

All ranking metrics are computed per task over the ranked candidate list, then averaged.

| Metric | Definition | Why |
|---|---|---|
| **hit@k** | 1 if at least one gold resource is in the top k, else 0 | "Did the first pointer land?" It is not complete localization. |
| **recall@k** | Share of gold resources that appear in the top k | How much of the needed context was found |
| **precision@k** | Share of the top k that are gold | The cost of the hint: wrong suggestions are detours |
| **MRR** | 1 / rank of the first gold resource (0 if none in the list) | Rewards putting the right answer first |
| **coverage** | Share of queries where `wn` answered instead of abstaining | Read together with precision of the answers given |

"Gold" means the files (or functions) that the real change touched. That label is imperfect: a
useful read-only file (a test, a config, a log) may never be edited. Reports say which label was used.

## Timings

| Timing | Includes |
|---|---|
| Ranking | Similarity search over precomputed vectors only |
| Warm query | Encoding the query, applying the adapter, ranking, and returning the result |
| Cold start | Process start, model load, index load, then a query |
| Initial index | Embedding every file in a repository |
| Refresh | Re-embedding changed files |
| Adapter fit | Mining history and fitting the adapter |

Report p50 and p95, peak memory, disk footprint, and the hardware.

## Uncertainty

Differences between two methods are reported as paired bootstrap confidence intervals over tasks.
Claims about new repositories use a repository-cluster bootstrap, because tasks from the same
repository are not independent.
