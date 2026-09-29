# Install

## From source (works today)

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
```

Run it again at any time to update: it only rebuilds when the requested ref moved. What it does:

1. Checks for `git` and a Rust toolchain. If `cargo` is missing it prints the rustup command and
   asks before running it (`--yes` to skip the prompt, e.g. in CI).
2. Clones `https://github.com/andreylukin/where-next` into `$WN_HOME/src` (default
   `~/.local/share/where-next/src`), a private clone; your other checkouts are never touched.
   Later runs `git fetch` it.
3. Checks out `--ref` (default `main`) and builds with
   `cargo install --path crates/wn-cli --locked`, installing `wn` into `~/.cargo/bin`
   (or `$WN_BIN_ROOT/bin`).
4. Prints the old and new commit and the next steps.

From inside `wn`, the same flow is `wn update`:

```sh
wn update                 # fetch main, rebuild if it moved (asks first on a terminal; --yes to skip)
wn update --check         # exit 0: up to date, 10: update available, 1: error
wn update --ref v0.1.0    # any branch, tag or commit
wn update --force         # rebuild even when up to date
wn --version              # wn 0.0.1 (abc1234 2026-09-29): the commit this binary was built from
```

| option / variable | meaning |
|---|---|
| `--ref <branch\|tag\|sha>` | what to build (default `main`) |
| `--yes`, `WN_YES=1` | no prompts; installs Rust with rustup if `cargo` is missing |
| `--force` | rebuild even if already up to date |
| `--dry-run` | print what would happen; no clone, checkout or build |
| `--uninstall` | remove the `wn` binary and the private clone (caches and models are kept) |
| `WN_HOME` | private clone location (default `~/.local/share/where-next`) |
| `WN_BIN_ROOT` | `cargo install --root` (default: cargo's own, usually `~/.cargo`) |
| `WN_REPO_URL` | source repository (default this repo; a local path works too) |

Uninstall: `curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh -s -- --uninstall`.
Caches (`~/.cache/where-next`) and models (`~/.cache/where-next-models`) are left in place.

## Prebuilt binaries (coming soon)

> There are no releases yet. When the first release is published, the same script installs a
> prebuilt, checksum-verified binary with `WN_FROM=release`.

```sh
# macOS (Apple silicon) and Linux (x86_64, arm64). Verifies the SHA-256 before installing to ~/.local/bin.
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | WN_FROM=release sh

# Homebrew (tap, once published)
brew install andreylukin/tap/where-next

# From crates.io (builds locally; downloads ONNX Runtime at build time)
cargo install where-next
```

Windows: download `wn-x86_64-pc-windows-msvc.zip` from the releases page.

Environment variables for release mode: `WN_VERSION` (a tag, default latest), `WN_INSTALL_DIR`
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
