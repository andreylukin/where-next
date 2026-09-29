# Documentation

| Page | For |
|---|---|
| [Quickstart](quickstart.md) | Installing `wn` and getting a first answer |
| [How it works](how-it-works.md) | The model, the index, the adapter, and what each one knows |
| [Privacy and licensing](privacy-and-licensing.md) | What stays on your machine; code, model and data licenses |
| [Adding a source](adding-a-source.md) | Writing a new resource plugin (docs, logs, CLI help, a language) |
| [Building and testing](building.md) | What exists in each crate today, and how to build, test and benchmark it |
| [Benchmarks](../benchmarks/README.md) | How results are measured and how to reproduce them |
| [FAQ](faq.md) | Why not grep, why not an editor index, and other common questions |

Status: the Rust tool is being built (milestone M1 in [PLAN.md](../PLAN.md)). The core, git mining,
source extraction, ONNX inference, the MCP server and the `wn` command exist (build from source; no releases yet). Pages describe
the intended behaviour and are marked where a feature does not exist yet.
- [install.md](install.md): installing `wn`, verifying releases, and pulling models.
