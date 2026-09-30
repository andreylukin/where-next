# Install

## Platforms

| Platform | Status |
|---|---|
| macOS, Apple silicon (`aarch64-apple-darwin`) | Prebuilt binary |
| Linux x86_64 and arm64 with glibc 2.39+ (Ubuntu 24.04+, Debian 13+, Fedora 40+) | Prebuilt binary |
| Older Linux (Ubuntu 22.04, Debian 12, RHEL/Alma 9, Amazon Linux 2023) | **Not supported yet.** The prebuilt ONNX Runtime library `wn` links needs glibc 2.38+ and GCC 13's libstdc++, so a source build fails to link there too. |
| musl Linux (Alpine) | Not supported yet (no prebuilt ONNX Runtime). |
| Intel Mac (`x86_64-apple-darwin`) | Not supported yet (no prebuilt ONNX Runtime). |
| Windows (`x86_64-pc-windows-msvc`) | Untested. `install.sh` doesn't handle Windows; a zip is built with each release. No background daemon, so every call loads the model (seconds). |

The unsupported platforms need ONNX Runtime built from source; that is on the roadmap.

**Resources:** the default model is a ~1.2 GB download, stored once under
`~/.cache/where-next-models`. Indexes are small (a few MB per repository, under
`~/.cache/where-next`). The background daemon that keeps the model warm uses about 1.2–1.6 GB of RAM
and exits after 15 idle minutes. The first `wn init` on a large repository takes minutes (about
6.5 minutes and 1.6 GB peak memory for kubernetes on an Apple M-series laptop; see
[building.md](building.md#measured-apple-m-series-laptop-cpu-fp32)).

## One-line install

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
```

To read it first:

```sh
curl -fsSLO https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh
less install.sh
sh install.sh
```

What it does:

1. Detects your platform. Where a prebuilt binary exists (see above), it downloads the latest
   release, verifies its SHA-256 checksum, and installs `wn` into `~/.local/bin`.
2. Otherwise, or if no release is available, it builds from source: it checks for `git` and a Rust
   toolchain (and asks before installing Rust with rustup), clones this repository into a private
   directory (`~/.local/share/where-next/src`; your other checkouts are never touched), and runs
   `cargo install --path crates/wn-cli --locked`, installing `wn` into `~/.cargo/bin`. The first
   build takes a few minutes. On Linux it refuses early, with a message, when glibc is too old.
3. Offers the default model (~1.2 GB) and verifies it against its SHA-256 manifest.
4. Prints the next steps.

If your shell then says `wn: command not found`, open a new terminal (after a fresh Rust install,
`source ~/.cargo/env`), and check that `~/.local/bin` or `~/.cargo/bin` is on your `PATH`.

| option / variable | meaning |
|---|---|
| `--ref <branch\|tag\|sha>` | build this ref from source (default `main`) |
| `--yes`, `WN_YES=1` | no prompts; installs Rust with rustup if a source build needs it, and downloads the model |
| `--no-model`, `WN_NO_MODEL=1` | skip the model download |
| `--force` | rebuild even if already up to date |
| `--dry-run` | print what would happen; no download, clone or build |
| `--uninstall` | remove the `wn` binary and the private clone (caches and models are kept) |
| `WN_MODEL_SOURCE` | pull the model from a local dir, an `https://` URL or `hf:owner/repo[@rev]` |
| `WN_VERSION` | release tag to install (default: latest) |
| `WN_INSTALL_DIR` | where a prebuilt binary goes (default `~/.local/bin`) |
| `WN_HOME` | private clone location for source builds (default `~/.local/share/where-next`) |
| `WN_BIN_ROOT` | `cargo install --root` for source builds (default: cargo's own, usually `~/.cargo`) |
| `WN_REPO_URL` | source repository (default this repo; a local path works too) |

Uninstall: `curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh -s -- --uninstall`.
Caches (`~/.cache/where-next`) and models (`~/.cache/where-next-models`) are left in place; delete
those directories to remove everything.

## Updating

```sh
wn update                 # source installs: fetch main and rebuild if it moved (asks first; --yes to skip)
wn update --check         # exit 0: up to date, 10: update available, 1: error
wn update --ref <tag-or-sha>
wn --version              # wn 0.0.1 (abc1234 2026-09-29): the commit this binary was built from
```

Re-running the installer always updates, for prebuilt and source installs alike.

## Build it yourself

```sh
git clone https://github.com/andreylukin/where-next
cd where-next
cargo install --path crates/wn-cli --locked
```

You need Rust, `git` and a C/C++ toolchain (`xcode-select --install` on macOS, `build-essential` on
Debian/Ubuntu). The build downloads a prebuilt ONNX Runtime, which is why the platform limits above
apply to source builds too. See [building.md](building.md).

## Verify a release

Every release archive ships with a `.sha256` file and is listed in `SHA256SUMS`. Build provenance
is attested with Sigstore through GitHub artifact attestations:

```sh
sha256sum -c wn-x86_64-unknown-linux-gnu.tar.gz.sha256
gh attestation verify wn-x86_64-unknown-linux-gnu.tar.gz --repo andreylukin/where-next
```

Each archive also has an SPDX SBOM (`wn-<target>.spdx.json`). Homebrew and crates.io packages are
prepared but not published yet.

## Models

The binary does not include model weights. Models are downloaded separately, verified against a
SHA-256 manifest, and stored under `~/.cache/where-next-models` (`$WN_MODELS_HOME`).

The default model is **gemma-xl1**, published at
[huggingface.co/lukandrey/where-next-gemma-xl1](https://huggingface.co/lukandrey/where-next-gemma-xl1)
(~1.2 GB; `wn` pins a revision). The installer offers it and offers it again on a later run if it is
missing or the pin moved. `wn update` says when the model has an update but never downloads it
unasked.

```sh
wn model pull                  # install or upgrade the default model; a no-op when current
wn model pull --check          # exit 0 if installed and current, 10 if a download is needed
wn model pull gemma-xl1 --source <path | https://… | hf:owner/repo[@revision]>   # another source
wn model list                  # installed models, and known models you can pull
wn model remove gemma-xl1 --yes
```

After pulling a model, run `wn init` again in repositories you indexed without one.

Models fine-tuned from EmbeddingGemma are distributed under the Gemma Terms of Use; the installer
states this when it asks, and `wn model pull` shows the notice when such a model is first installed. See
[privacy-and-licensing.md](privacy-and-licensing.md).

Without a model, `wn` falls back to keyword matching, which is much weaker, and says so.

## npm

An `npx where-next` launcher is designed but not published; see
[packaging/npm/DESIGN.md](../packaging/npm/DESIGN.md).
