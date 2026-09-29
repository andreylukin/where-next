# Benchmarks

Reproducible evaluations of where-next. Planned (see PLAN.md):

- **ContextBench** file localization (hit@k, recall/precision@k, MRR), with repository-cluster confidence intervals.
- **History replay**: rank each commit's changed files against the repository as it was just before that commit.
- **CI gate**: a small fixed subset with cached vectors that runs on every pull request, with budgets for hit@3 and p95 latency.
