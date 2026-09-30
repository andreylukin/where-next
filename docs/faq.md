# FAQ

## Why not just grep?

Use both. Grep (`rg`) is the right tool when you know the exact string, symbol or error message, and
where-next does not replace it. where-next helps when you know *what* you want but not *what it is
called*: "where do we retry uploads?", "which config controls the port?". On ContextBench, the default
model put a correct file in the top 3 for about 76% of the tasks in repositories held out from its
training; on all 1,136 tasks, plain lexical ranking (BM25) managed about 37%. (Different subsets; see
[benchmarks](../benchmarks/README.md).)

## How is this different from embedding search?

Plain embedding search ranks files by how similar their text is to your query. `wn` differs in three
layers:

1. **The training target is the outcome.** The default model (`gemma-xl1`, fine-tuned from
   EmbeddingGemma-300M) learned from about 1.1M (task, files that actually changed) pairs: commit
   message to changed files, issue to the files the fix edited, error to where the fix landed, with
   mined hard negatives. Queries use a fixed layout: the request, then the tail of the last tool
   output (errors, stack traces), then earlier context.
2. **A per-repository adapter.** `wn init` fits a small linear map on the query side from your
   repository's commit history, in seconds on CPU. It only transforms the query, so refitting never
   re-indexes. A control fitted on commits shuffled against their files did no better than no
   adapter, and the real adapter beat it by +.071 hit@3 on ContextBench's held-out repositories
   (measured with an earlier fine-tune; see [benchmarks](../benchmarks/README.md#current-results-preliminary-research-prototype)).
3. **A product layer around the ranking.** At most 3 hints; a calibrated threshold below which `wn`
   says "no confident hint" instead of guessing; `(exact)` evidence when a distinctive name from the
   query literally appears in a file; path priors that down-weight test and example files unless you
   ask for them; a git-aware index (untracked files in, git-ignored files out, incremental refresh);
   fail-open states when the index or model is missing; and a warm background daemon.

ContextBench hit@3, same evaluator (the research harness):

| Ranker | hit@3 | Tasks |
|---|---|---|
| BM25 | .37 | all 1,136 |
| EmbeddingGemma-300M, untrained (the same base model) | .46 | official 500 |
| SweRankEmbed-Small, zero-shot | .61 | official 500 |
| `wn` default model | .76 | official 500 |
| `wn` default model + per-repo adapter | .81 | official 500 |

On the 994 tasks from repositories held out from fine-tuning: untrained .46, `wn` .76, with the
adapter .80. The untrained run likely used a 512-token input limit against 1,024 for the fine-tune.

## Why not my editor's codebase index?

Editor indexes are search over your code. where-next differs in three ways:

1. **It is trained on outcomes.** The model learned from real changes: which files a fix actually
   touched, not just which text looks similar.
2. **It learns your repository.** A small adapter is fitted from your repository's own history, on
   your machine.
3. **It is local and agent-agnostic.** It runs as a CLI and an MCP server, so any agent can use it,
   and nothing is uploaded. Some editor indexes send code chunks to a server to be embedded.

## Does it replace my coding agent's exploration?

No. It gives the agent a good first pointer, or says nothing. The agent still reads, searches and
decides. A wrong hint costs a detour, which is why `wn` returns at most 3 results and says it has
no confident hint when nothing scores above a calibrated threshold.

## Does it help agents in practice?

Not measurably, for a cheap, capable agent. Three controlled trials (a pilot, a large-repository trial,
and a pre-declared confirmatory trial on repositories with 3,000–13,600 files) found that automatic
start hints didn't lower cost per resolved task or raise success. In the confirmatory trial the hint
roughly halved the tokens spent before the agent first read a right file, but finding the file wasn't
where that agent spent its budget. A fourth trial gave the agent the files the real fix touched
(an upper bound for any hint): 34 of 58 solved vs 32 without hints, so finding files isn't the
bottleneck for a capable agent. The full numbers are in
[benchmarks/agent-trial.md](../benchmarks/agent-trial.md).

Still open: people navigating by hand, and more expensive agents. Until one of those shows a real
benefit, treat where-next as fast local navigation. Agent integration is optional: `wn setup`
connects Claude Code, Codex, Cursor or bough with a skill and hooks; `wn ask --start` gives a
manual task-start hint.

## What does it do badly?

- Short conversational follow-ups ("now do the same for the other one"): about .31 hit@3 so far.
- Exact identifiers and strings, where grep is better.
- Files it cannot see: anything ignored by git, or resource types without a source yet.
- Deciding when to stay quiet: thresholds are calibrated per model and query kind on held-out
  queries, but not perfectly; expect it to sometimes answer when it should have stayed quiet, and the
  reverse.

## Does it send my code anywhere?

No. See [privacy-and-licensing.md](privacy-and-licensing.md).

## Which platforms does it run on?

macOS on Apple silicon, and Linux (x86_64, arm64) with glibc 2.35+ (Ubuntu 22.04+, Debian 12+). Older
Linux, Intel Macs and musl aren't supported yet because of the ONNX Runtime builds `wn` uses (the
installer says so before downloading anything); Windows is untested. See [install.md](install.md#platforms).

## Why Rust?

A Rust CLI with quick startup is easy to install and inspect. It links system libraries on macOS;
on Linux, the release includes `libonnxruntime.so` alongside `wn`. Inference uses ONNX Runtime;
training moves to Rust once the Rust trainer matches the reference trainer's quality.

## Why is the model under the Gemma Terms?

The default model is fine-tuned from EmbeddingGemma, which performed well at 300M parameters and runs
quickly on CPU. Its derivative has to stay under Google's terms. An Apache-2.0 alternative model is
planned for anyone who needs one.
