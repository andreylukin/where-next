# where-next

Ask your repo "where is X?" in plain English. Get the 3 files to open.

Runs locally · learns from your git history · ~80–100 ms per warm query · CLI + MCP

[![CI](https://github.com/andreylukin/where-next/actions/workflows/ci.yml/badge.svg)](https://github.com/andreylukin/where-next/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/andreylukin/where-next)](https://github.com/andreylukin/where-next/releases/latest)
[![License](https://img.shields.io/github/license/andreylukin/where-next)](LICENSE)

![Terminal recording: in a kubernetes clone, wn ask is given the titles of three real bug reports and lists the file each fix changed first; then rg finds an exact symbol name.](docs/demo.gif)

*Real bug-report titles on a kubernetes clone; the file each fix changed ranks #1. [About the demo](docs/quickstart.md#the-demo).*

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
```

macOS on Apple silicon and Linux with glibc 2.35+. The ~1.2 GB model downloads after the installer
asks. Read-first install, uninstall, Intel Mac and Windows status: [docs/install.md](docs/install.md).
Uninstall everything: `wn uninstall`.

## Quick start

```sh
cd your-repo
wn init                                   # index the repo, learn from its git history
wn setup                                  # connect Claude Code, Codex, Cursor: hints arrive automatically
wn ask "where are gitignore rules matched against paths"
wn bench                                  # optional: replay past commits, see how it does here
```

On a clone of [ripgrep](https://github.com/BurntSushi/ripgrep):

```text
$ wn ask "where are gitignore rules matched against paths"
crates/ignore/src/gitignore.rs  0.50
crates/ignore/src/dir.rs        0.42
crates/ignore/src/overrides.rs  0.42
```

Scores rank the files; they are not probabilities. When nothing clears a calibrated threshold, `wn`
says "no confident hint". Query tips: [docs/quickstart.md](docs/quickstart.md#writing-good-queries).

For agents, run `wn setup`: Claude Code, Codex and Cursor get the skill and hooks that add hints
to their context. See [docs/skill.md](docs/skill.md); MCP (`wn mcp`) is there for other clients.

## Why not grep or plain embeddings?

Grep needs the string you already know. Plain embedding search matches text that looks similar.
`wn`'s model is fine-tuned on ~1.1M (task → files that actually changed) pairs, and a per-repo
adapter fitted on your commits in seconds learns your repository.

| Ranker | hit@3 |
|---|---|
| BM25¹ | .37 |
| EmbeddingGemma-300M, untrained (same base as `wn`) | .46 |
| SweRankEmbed-Small | .61 |
| `wn` model | .76 |
| `wn` model + per-repo adapter | **.81** |

hit@3 = a file the real fix changed is in the top 3. ContextBench, official 500-task subset.
¹ BM25 is on all 1,136 tasks. On the 994 tasks from repositories held out from training: untrained
.46, `wn` .76, with adapter .80. Protocols and more models: [benchmarks](benchmarks/README.md) ·
[FAQ](docs/faq.md#how-is-this-different-from-embedding-search).

## Limits

- **No measured agent savings.** Three controlled trials found no lower cost or higher success for
  agents ([agent trial](benchmarks/agent-trial.md)). Use it as navigation, not a cost-saver.
- **Exact names and strings: use `rg`.** `wn` ranks by meaning.
- **Vague follow-ups** like "now the other one" get "no confident hint" rather than a guess. Ask full
  questions.
- **Early.** v0.1.0. Windows and Intel Macs are not supported yet.

Full list: [docs/limitations.md](docs/limitations.md).

---

[Docs](docs/README.md) · [FAQ](docs/faq.md) · [Troubleshooting](docs/troubleshooting.md) ·
[Benchmarks](benchmarks/README.md) · [Changelog](CHANGELOG.md) · [Contributing](CONTRIBUTING.md)

Code: Apache-2.0 ([LICENSE](LICENSE)). The default model,
[lukandrey/where-next-gemma-xl1](https://huggingface.co/lukandrey/where-next-gemma-xl1), is
distributed separately; it is fine-tuned from `google/embeddinggemma-300m` and is subject to the
[Gemma Terms of Use](https://ai.google.dev/gemma/terms). See [NOTICE](NOTICE) and
[privacy and licensing](docs/privacy-and-licensing.md).

No telemetry. Your code never leaves your machine. `wn report` shares anonymous usage stats only
after you read and confirm them ([docs/stats.md](docs/stats.md)).
