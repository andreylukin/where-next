# History replay protocol (`wn bench --history`)

History replay answers the question a user actually has: *how well does this work on my
repository?* It replays the repository's past commits as if each one were a new task.

> `wn bench --history` is planned for milestone M1. The protocol below is what the research
> prototype used.

## Setup

For every non-merge commit, in chronological order:

1. **Query:** the commit subject, plus the first line of the body if it is short.
2. **Candidates:** every indexable source file in the commit's **parent** tree, so each commit is
   ranked against the repository as it was just before the change.
3. **Gold:** changed files that already existed in the parent. Tests, generated files and vendored
   code are excluded from the implementation-file variant; a second variant includes tests.
4. **Skip** commits with no eligible gold files or with more than 6. Report how many commits were
   eligible.

## Methods

- Zero-shot model, fine-tuned model, BM25.
- **Recency:** rank files by how recently they changed. Work comes in bursts, so this is the baseline
  the adapter has to beat.
- **Co-change:** files that historically changed together with recently changed files.
- **History retrieval:** find the most similar past commit messages and rank the files they changed.
  This tests whether a learned adapter beats simply remembering examples.
- **Rolling adapter:** for each block of 100 commits, fit the adapter on the previous 200 commits
  only, then rank the block. The model stays frozen, so nothing is re-indexed.
- **Shuffled-query adapter** (control): the same fit with commit messages shuffled against their
  files, preserving file frequencies. If this matches the real adapter, the gain is popularity, not
  learning.

## Leakage controls

- Candidates come from the parent tree; the adapter only sees earlier commits.
- If the model's training data contained this repository's history, results are reported separately
  for commits that were and were not in training, and a model retrained without the repository is the
  clean comparison.

## Report

hit@1/3/10, recall and precision at k, MRR, a time series (rolling 100-commit window) with detected
architecture changes marked, and the eligible-commit share.

Example from the research prototype, on one private multi-language repository with 8 architecture
rewrites (1,510 matched commits, hit@3): zero-shot .32, recency .37, fine-tuned .65, fine-tuned plus a
recency bonus .73, fine-tuned plus rolling adapter .80.
