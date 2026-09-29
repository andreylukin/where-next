# ContextBench protocol

[ContextBench](https://arxiv.org/abs/2602.05892) contains 1,136 issue-resolution tasks from 66
repositories in 8 languages, with annotated gold context. We use it to measure file localization in
repositories where-next was not trained on.

## Setup

1. For each task, check out the repository at the task's base commit (the state before the fix).
2. **Candidates:** every indexable source file at that commit. Files outside source coverage (for
   example `go.mod` or reproduction scripts) are counted as misses, and their share is reported.
3. **Query:** the issue text only.
4. **Gold:** the files in the task's gold context, with paths normalized to the repository root.

## Methods

| Method | Notes |
|---|---|
| BM25 | Over the same file skeletons the model sees |
| Symbol-aware lexical | Path segments, `snake_case` and `camelCase` split, exact-match boosts |
| Zero-shot embedding | The base model before fine-tuning |
| SweRankEmbed-Small | The closest published issue-localization retriever |
| where-next | The fine-tuned model |
| where-next + adapter | The adapter fitted on that repository's commits before the task's base commit, using git ancestry, not timestamps |

## Leakage controls

- Training excludes every ContextBench repository. Rows from those repositories found in raw
  training data are audited and reported; results are shown both for all tasks and for the subset of
  repositories with no training overlap.
- The adapter uses only commits that are ancestors of the base commit.

## Report

hit@1/3/10, recall@5 and precision@5 (the benchmark's official file-level metrics, using the top 5 as
the prediction), MRR, per language and per repository size, with repository-cluster confidence
intervals. Include a list of tasks where a baseline beats where-next.
