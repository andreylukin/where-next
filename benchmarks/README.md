# Benchmarks

How where-next is measured, and how anyone can reproduce the numbers. Every claim in the README
traces to one of these protocols.

| Kit | Question it answers | Status |
|---|---|---|
| [ContextBench](contextbench.md) | Does it find the right files in unfamiliar repositories? | Protocol final; Rust runner planned |
| [History replay](history-replay.md) | How well does it work on *my* repository, and does the adapter help? | Protocol final; `wn bench --history` planned |
| [Agent trial](agent-trial.md) | Does it save an agent cost and time without hurting success? | In progress in the research prototype |
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
