# FAQ

## Why not just grep?

Use both. Grep (`rg`) is the right tool when you know the exact string, symbol or error message, and
where-next does not replace it. where-next helps when you know *what* you want but not *what it is
called*: "where do we retry uploads?", "which config controls the port?". In our benchmark, plain
lexical ranking (BM25) put a correct file in the top 3 for about 37% of ContextBench tasks; the
fine-tuned model did so for about 74%.

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
decides. A wrong hint costs a detour, which is why `wn` returns at most 3 results and abstains when
it isn't confident.

## Does it help agents in practice?

Not measurably, in the one setting we have tested. In a controlled pilot on 50 SWE-bench Pro tasks with
a cheap, capable agent, hints got the agent to a correct file about 2.6 steps sooner, but success was
unchanged and cost per resolved task was slightly higher (within noise). Finding the file wasn't where
that agent spent its budget. The full numbers are in [benchmarks/agent-trial.md](../benchmarks/agent-trial.md).

Still open: more expensive agents, very large repositories, and people navigating by hand. Until one of
those shows a real benefit, treat where-next as fast local navigation, not a cost saver.

## What does it do badly?

- Short conversational follow-ups ("now do the same for the other one"): about .25 hit@3 so far.
- Exact identifiers and strings, where grep is better.
- Files it cannot see: anything ignored by git, or resource types without a source yet.
- Deciding when to stay quiet: current abstain thresholds withhold too many good hints on issue-style
  questions. Calibration per model and query type is in progress.

## Does it send my code anywhere?

No. See [privacy-and-licensing.md](privacy-and-licensing.md).

## Why Rust?

A single static binary with instant startup is the easiest thing to install and trust. Inference
uses ONNX Runtime; training moves to Rust once the Rust trainer matches the reference trainer's
quality.

## Why is the model under the Gemma Terms?

The default model is fine-tuned from EmbeddingGemma, which performed well at 300M parameters and runs
quickly on CPU. Its derivative has to stay under Google's terms. An Apache-2.0 alternative model is
planned for anyone who needs one.
