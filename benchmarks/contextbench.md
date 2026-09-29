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

## Running it with `wn`

```sh
wn bench --contextbench path/to/cb
```

`path/to/cb/tasks.jsonl` has one task per line:

```json
{"instance_id": "…", "repo_dir": "repos/owner__name", "base_commit": "…",
 "problem_statement": "…", "gold_files": ["src/…"]}
```

`repo_dir` is a local clone (relative to the tasks file, or absolute) that contains `base_commit`;
gold paths written as `/workspace/<name>/…` are made repository-relative. Tasks whose repository is
missing are skipped and counted in the report. To build the file from the ContextBench release (a
parquet file with `instance_id`, `repo_url`, `base_commit`, `problem_statement` and `gold_context`),
for example with DuckDB (`duckdb < prepare.sql`, run next to `contextbench.parquet` with a `cb/`
directory present):

```sql
COPY (
  SELECT instance_id,
         'repos/' || replace(regexp_extract(repo_url, 'github\.com/([^/]+/[^/]+?)(\.git)?/?$', 1), '/', '__') AS repo_dir,
         base_commit,
         problem_statement,
         list_distinct(list_transform(from_json(gold_context, '[{"file": "VARCHAR"}]'), g -> g.file)) AS gold_files
  FROM 'contextbench.parquet'
) TO 'cb/tasks.jsonl' (FORMAT json);
```

This yields 1,136 tasks across 67 repositories. Then clone each repository into
`cb/repos/<owner>__<name>` with full history (the adapter is fitted
on each repository's commits before the task's base commit). The runner scores lexical BM25, the
model, and the model with that adapter, with the same metrics as the history replay.
