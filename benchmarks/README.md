# Benchmarks

How where-next is measured, and how anyone can reproduce the numbers. Every claim in the README
traces to one of these protocols.

| Kit | Question it answers | Status |
|---|---|---|
| [ContextBench](contextbench.md) | Does it find the right files in unfamiliar repositories? | Protocol final; `wn bench --contextbench` |
| [History replay](history-replay.md) | How well does it work on *my* repository, and does the adapter help? | `wn bench --history` |
| [Agent trial](agent-trial.md) | Does it save an agent cost and time without hurting success? | Three trials done, no cost benefit shown ([details](agent-trial.md)) |
| CI gate | Did this pull request make quality or latency worse? | Planned |

Metrics are defined once, in [metrics.md](metrics.md).

## Principles

1. **Pin everything:** repository commits, task lists, model weights and their checksums, tool
   versions, and hardware.
2. **Report baselines next to every result:** ripgrep-style lexical search, BM25, a zero-shot
   embedding model, and SweRankEmbed (the closest published retriever).
3. **Show where we lose.** Every report includes the task types and examples where a baseline beats
   where-next.
4. **No leakage.** Candidates come from the repository as it was *before* the change; the adapter only
   sees history from before the task; evaluation repositories are excluded from training.
5. **Separate timings:** ranking only, warm query, cold start, initial indexing, incremental refresh,
   and adapter fit. Report p50 and p95.
6. **Uncertainty:** paired bootstrap intervals, clustered by repository when claiming generalization to
   new repositories.

## Current results (preliminary, research prototype)

hit@3 = at least one file the real change touched is in the top 3.

| Benchmark | Zero-shot | Qwen3-Emb-0.6B fine-tuned (+ adapter) | **EmbeddingGemma-300M fine-tuned, the default** (+ adapter) |
|---|---|---|---|
| ContextBench, official 500-task subset | .49 | .73 (.80) | **.76 (.81)** |
| ContextBench, 994 tasks in repositories held out from fine-tuning | .52 | .72 (.78) | **.76 (.80)** |
| History replay, one private multi-language repository with 8 architecture rewrites (1,510 matched commits) | .32 | .65 (.80) | **.70 (.83)** |

The default model is the EmbeddingGemma-300M fine-tune (`gemma-xl1`, about 1.1M training examples,
mined hard negatives, v2 query layout); file-level top 5: Multi-SWE-bench .66, SWE-PolyBench .70. On
all 1,136 ContextBench tasks: BM25 .37, SweRankEmbed-Small .62 zero-shot, Qwen fine-tune .74 (.79 with
the adapter). The Rust `wn` answers warm MCP queries in about 21 ms at p95 on kubernetes (31,000 files);
see [docs/building.md](../docs/building.md).

### Training-data size

More rows did not help past about 1.1M. The extra rows are mostly commit history, which moves the model
toward commit-style queries (history replay) and away from issue-style ones (ContextBench,
SWE-PolyBench). All EmbeddingGemma-300M, same protocol:

| Model | Training rows | ContextBench held-out hit@3 (+ adapter) | History replay hit@3 (+ adapter) | SWE-PolyBench top 5 |
|---|---|---|---|---|
| gemma-g1 | 100k | .69 (.76) | .69 (.82) | – |
| gemma-g2r | 342k | .72 (.78) | .70 (.82) | – |
| **gemma-xl1 (default)** | 1.10M | **.76 (.80)** | .70 (.83) | **.70** |
| gemma-xl2 | 2.14M | .73 (.79) | .74 (.83) | .66 |
| gemma-xl2m (xl1's source mix, mined negatives) | 2.39M | .73 (.80) | .75 (.83) | .70 |
| gemma-xl3 | 2.87M | .71 (.78) | .75 (.84) | .64 |

gemma-xl1 stays the default because agents send issue-style, self-contained queries. With the per-repo
adapter the differences on ContextBench mostly disappear. The next gains likely need more issue-style
training data or a larger model, not more commits.

