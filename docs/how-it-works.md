# How it works

where-next keeps three kinds of knowledge in three different places.

| Layer | What it knows | Where it lives | How it changes |
|---|---|---|---|
| **Model weights** (skill) | How to judge whether a resource is relevant to a task: issue words to file and symbol names, error text to where errors come from | The downloaded model file | Only when a new model is released |
| **Index** (memory) | What is in this repository right now: one vector per file skeleton (and per function, once `--functions` is used) | `~/.cache/where-next/<repo>/` | Incrementally, for changed files only |
| **Adapter and usage log** (learning) | This repository's habits: which kinds of change touch which files, its abbreviations and conventions | Next to the index | Refit from git history, and later from local usage |

## One query

1. The query (your request, plus recent context such as the last error) is turned into one vector by
   the model.
2. The adapter, a small linear map, adjusts that query vector for this repository. It only ever
   transforms the query, so refitting it never invalidates the index.
3. The query vector is compared with every stored vector (cosine similarity), and the closest
   candidates are kept.
4. Abstain rules check the best candidate against a threshold calibrated for this model and kind
   of query. If nothing scores above it, `wn` returns no hint and says so.
5. At most 3 results go back, each with its score.

## What the index stores

Not your code. For each file it stores a short skeleton (path, first doc or comment line, top-level
symbol names) and the vector made from it. For each function it stores the name and signature and
its vector. File contents are read while indexing and not kept.

## Why the adapter works

Each commit in a repository is a free, labelled example: the commit message describes a change, and
the files it touched are the answer. Fitting the adapter on the last few hundred commits teaches it
the repository's vocabulary and structure. It is fitted in seconds on CPU because the model stays
frozen; only a small matrix is learned.

In the research prototype, on one private repository with eight architecture rewrites, the adapter
raised hit@3 from .70 to .83 over the default fine-tuned model alone, and re-adapted within 100 to 200
commits after each rewrite. Ranking by recency alone scored .37, so the adapter is learning more than
"what changed recently".

## How the model was trained

The model is a small embedding model fine-tuned with a contrastive loss on outcome labels:

- commit message to the files and functions it changed,
- issue text to the files the accepted fix edited,
- an error or stack trace to where the fix landed,

with mined hard negatives.

Evaluation repositories are excluded from training. Details live in
[training/README.md](../training/README.md) and [datasets/README.md](../datasets/README.md).

## Lifecycles are state machines

The index, model download, adapter, daemon and each query are explicit state machines with complete
transition tables, tested over every state and event. See [CONTRIBUTING.md](../CONTRIBUTING.md).
