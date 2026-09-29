# History replay protocol (`wn bench --history`)

History replay answers the question a user actually has: *how well does this work on my
repository?* It replays the repository's past commits as if each one were a new task.

## Try it on your repository in 60 seconds

```sh
cd your-repo
wn bench            # same as: wn bench --history
```

It reads history with git plumbing only (`log`, `ls-tree`, `cat-file`), never touches the working
tree, and prints hit@1/3/10 and MRR for three rankers on the same commits: lexical BM25 over the file
skeletons, the model, and the model with the personal adapter. On a repository of a few thousand files
the first run takes one to two minutes on CPU, almost all of it embedding old file versions; vectors
are cached (and reused from `wn init`'s index), so a rerun takes seconds.

| Flag | Default | Meaning |
|---|---|---|
| `--commits N` | 300 | Score the newest N eligible commits |
| `--train N` | 200 | Fit each adapter on the N previous eligible commits that are git ancestors of every commit it scores |
| `--step N` | 100 | Refit the adapter every N scored commits (smaller in short histories, so each block is fitted on most of what came before it) |
| `--with-tests` | off | Count changed test files as gold too |
| `--no-adapter` | off | Lexical and model only |
| `--json` | off | Machine-readable report |

The report also breaks results down by candidate-set size and, when the repository's layout or
dominant language changed, by era.

Example, on the public [ripgrep](https://github.com/BurntSushi/ripgrep) repository (gemma-xl1, CPU,
newest 300 eligible commits, median 101 candidate files, 59 s cold):

| | hit@1 | hit@3 | hit@10 | MRR |
|---|---|---|---|---|
| lexical (BM25) | .27 | .49 | .74 | .42 |
| model | .39 | .74 | .92 | .59 |
| model + adapter | .65 | .81 | .94 | .75 |

**Parity with the research prototype.** On ripgrep's 928 newest eligible commits (adapter fitted on
the previous 200, refitted every 100), `wn bench` and an independent Python recomputation of the
prototype protocol on the same vectors agree exactly for lexical (hit@3 .539) and model (.762), and
within 0.3 points for the adapter (.852 vs .850; negative sampling uses a different random generator,
and `wn` additionally requires git ancestry).

## Protocol

For every non-merge commit, in chronological order:

1. **Query:** the commit subject, plus the first line of the body if it is short.
2. **Candidates:** every indexable source file in the commit's **parent** tree, so each commit is
   ranked against the repository as it was just before the change.
3. **Gold:** changed files that already existed in the parent. Tests, generated files and vendored
   code are excluded from the implementation-file variant; a second variant includes tests.
4. **Skip** commits with no eligible gold files, or that change more than 6 existing files
   (counting tests). Report how many commits were eligible.

## Methods

`wn bench` implements lexical BM25, the model and the rolling adapter. The research prototype also
compared these (see the controls results):

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
