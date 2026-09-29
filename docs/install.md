# Install

> **Coming soon.** There are no releases yet, so the commands below do not work today. Build from
> source instead: see [building.md](building.md). This page documents what the first release will
> ship and how to verify it.

## Binary

```sh
# macOS (Apple silicon) and Linux (x86_64, arm64). Verifies the SHA-256 before installing to ~/.local/bin.
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh

# Homebrew (tap, once published)
brew install andreylukin/tap/where-next

# From crates.io (builds locally; downloads ONNX Runtime at build time)
cargo install where-next
```

Windows: download `wn-x86_64-pc-windows-msvc.zip` from the releases page.

Environment variables for `install.sh`: `WN_VERSION` (a tag, default latest), `WN_INSTALL_DIR`
(default `~/.local/bin`).

## Verify a release

Every release archive ships with a `.sha256` file and is listed in `SHA256SUMS`. Build provenance
is attested with Sigstore through GitHub artifact attestations:

```sh
sha256sum -c wn-x86_64-unknown-linux-gnu.tar.gz.sha256
gh attestation verify wn-x86_64-unknown-linux-gnu.tar.gz --repo andreylukin/where-next
```

Each archive also has an SPDX SBOM (`wn-<target>.spdx.json`).

Supported targets: `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
(glibc 2.39+, e.g. Ubuntu 24.04), `x86_64-pc-windows-msvc`. Not offered yet, because ONNX Runtime
has no prebuilt binaries for them: Intel Macs (`x86_64-apple-darwin`) and static musl Linux. Those
need ONNX Runtime built from source.

## Models

The binary does not include model weights. Models are downloaded separately, verified against a
SHA-256 manifest, and stored under `~/.cache/where-next-models`:

```sh
wn model pull gemma-xl1 --source <path | https://… | hf:owner/repo[@revision]>
wn model list
wn model remove gemma-xl1 --yes
```

There is no default model source until hosting is decided, so `--source` is required. Models
fine-tuned from EmbeddingGemma are distributed under the Gemma Terms of Use; `wn` shows that
notice the first time such a model is pulled. See
[privacy-and-licensing.md](privacy-and-licensing.md).

Without a model, `wn` still works with a lexical fallback and says so.

## npm

An `npx where-next` launcher is designed but not published; see
[packaging/npm/DESIGN.md](../packaging/npm/DESIGN.md).
