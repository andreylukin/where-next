# Privacy and licensing

## What stays on your machine

Everything. The model runs locally. The index, adapter and usage log are stored under
`~/.cache/where-next/`. There is no account, no login, and no telemetry. The only network access is
the one-time model download, which is checksum-verified.

## The usage log

- Records which hints were shown and which files were later opened and edited, so the adapter can
  learn from real use.
- Query text is not stored unless you opt in.
- `--no-log` skips logging for a single call.
- Entries expire after 30 days.

## Licensing

| Artifact | License | Where |
|---|---|---|
| Code in this repository | Apache-2.0 | [LICENSE](../LICENSE) |
| Default model weights | Gemma Terms of Use | Downloaded separately; never in this repository |
| Alternative model (planned) | Apache-2.0 | Downloaded separately |
| Datasets | Each source under its original license | Published separately with a dataset card |

### The default model

The default model is fine-tuned from `google/embeddinggemma-300m`, so it is a model derivative of
Gemma and is distributed under the [Gemma Terms of Use](https://ai.google.dev/gemma/terms), including
the use restrictions they reference. The model card states that it was modified by where-next. Using
it does not imply any endorsement by Google. `wn init` shows this notice before the first download.

If you need a model without these terms, the planned alternative model is Apache-2.0.

### Datasets

Only public sources are published, each with attribution and its original license in the dataset
card. No private repositories, sessions or usage logs are ever included.

## Verifying a release

Every release will ship SHA-256 checksums, build provenance attestations and an SBOM. Instructions
for verifying them will be added with the first release.

## Reporting a security issue

See [SECURITY.md](../SECURITY.md).
