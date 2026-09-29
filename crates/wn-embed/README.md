# wn-embed

Embedding backend for where-next: loads an exported model, verifies it, and turns documents and
queries into unit vectors with ONNX Runtime.

## Model directory

A model is a directory. Weights are not in this repository; they are distributed separately
under their own license (see `NOTICE`).

| File | Contents |
|---|---|
| `wn-model.json` | Model spec: `name`, `family` (`qwen` / `gemma`), `pooling`, `dim`, `max_seq`, `query_prefix`, `document_prefix`, `matryoshka` |
| `tokenizer.json` | Hugging Face tokenizer |
| `model.onnx` (+ `model.onnx.data`) | fp32 graph: inputs `input_ids`, `attention_mask` (int64, `[batch, seq]`, right padding); output `embeddings` (float32, `[batch, dim]`, L2-normalised). Pooling, projection layers and normalisation are inside the graph. |
| `model.q8.onnx` (optional) | weight-only int8 graph (`MatMulNBits`, 8 bits) with the same interface: half the download, ~3x slower per query on Apple CPUs; used when no fp32 graph is present. Dynamic activation int8 and 4-bit graphs fail parity for these models. |
| `wn-manifest.json` | SHA-256 of every file above. The model is only used after all checksums match. |
| `fixture.json` (optional) | texts with reference embeddings from the Python export, for parity tests |

Write the manifest with `cargo run -p wn-embed --example manifest -- <model-dir>`.

## Text layout

Documents are `wn-sources` doc texts (`file: <skeleton>` or `function: <path>::<name>`) with the
family's `document_prefix` in front (empty for Qwen, `title: none | text: ` for Gemma). Queries are
built by `wn_core::text::query_text` (instruction, task, tail of recent context); Gemma swaps the
instruction for its `query_prefix`. This must match training byte for byte, which the parity test
checks.

## Lifecycle

`Missing → Downloading → Verifying → Loaded / Corrupt`, an explicit state machine
(`src/lifecycle.rs`). Only `Loaded` embeds. A checksum mismatch lands in `Corrupt`; the next
`ensure` re-fetches from the source.

## Tests

- `cargo test -p wn-embed`: state machine (every state × event pair, model-based property test),
  manifests, store, spec, text builders.
- `WN_TEST_MODEL_DIR=<model-dir> cargo test --release -p wn-embed --test onnx_parity`: Rust
  embeddings and text builders against the Python reference (skipped without the variable).

## Benchmarks

- `cargo run --release -p wn-embed --example encode_bench -- <model-dir> [graph]`: single-query
  latency and document throughput on CPU.
