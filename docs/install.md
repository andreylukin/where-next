# Install

## Platforms

| Platform | Status |
|---|---|
| macOS, Apple silicon (`aarch64-apple-darwin`) | Prebuilt binary |
| Linux x86_64 and arm64 with glibc 2.35+ (Ubuntu 22.04+, Debian 12+) | Prebuilt binary; ONNX Runtime (`libonnxruntime.so`) is bundled and installed beside `wn` |
| Older Linux (Debian 11, RHEL/Alma 9, Amazon Linux 2023, …) | **Not supported yet.** The installer checks glibc and stops before downloading anything. |
| musl Linux (Alpine) | Not supported yet. |
| Intel Mac (`x86_64-apple-darwin`) | **Not supported yet.** The installer stops with a message: ONNX Runtime has no prebuilt build for it. Apple silicon Macs running a shell under Rosetta get the arm64 binary. |
| Windows (`x86_64-pc-windows-msvc`) | Untested. `install.sh` doesn't support Windows. There's no background daemon, so every call loads the model (seconds). |

**Resources:** the default model is a ~1.2 GB download, stored once under
`~/.cache/where-next-models`. Indexes live under `~/.cache/where-next` and grow with the
repository: a few MB for a small one, tens of MB for a large one (the vectors alone for
kubernetes' ~20k indexed files are about 62 MB). The background daemon that keeps the model warm
uses about 1.2–1.6 GB of RAM and exits after 15 idle minutes. The first `wn init` on a large
repository takes minutes (about 6.5 minutes and 1.6 GB peak memory for kubernetes on an Apple
M-series laptop; see [building.md](building.md#measured-apple-m-series-laptop-cpu-fp32)) and prints
progress on stderr while it runs.

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

1. Checks your platform. On Linux it checks the glibc version, and on an Intel Mac it stops, both
   before downloading anything.
2. Downloads the latest release (v0.1.0) for your platform, verifies its SHA-256 checksum (a
   mismatch or missing checksum stops the install), and installs `wn` into `~/.local/bin`, plus
   `libonnxruntime.so` beside it on Linux. If `wn` is already running as a daemon, it's stopped
   first. The installer tells you if `~/.local/bin` isn't on your `PATH`.
3. Only if no release binary is published for your platform (HTTP 404), it builds the latest
   release tag from source instead. It checks for `git` and a Rust toolchain and asks before
   installing Rust with rustup. Then it clones this repository into a private directory
   (`~/.local/share/where-next/src`; your other checkouts are never touched) and runs
   `cargo install --path crates/wn-cli --locked`, installing `wn` into `~/.cargo/bin`.
4. Offers the default model (~1.2 GB), shows download progress, and verifies the model against
   its SHA-256 manifest. An interrupted download resumes on the next try.
5. If Claude Code, Codex or Cursor is installed (`~/.claude`, `~/.codex` or `~/.agents`,
   `~/.cursor`), shows what `wn setup` would write and asks "Connect wn to … (skill + hooks)?
   [Y/n]" (Enter means yes; it reads the answer from your terminal, so it works under
   `curl | sh`). With `--yes` it connects without asking. With no terminal and no `--yes` it skips
   this and `wn setup` is the first next step. See [skill.md](skill.md).
6. Prints the next steps.

| option / variable | meaning |
|---|---|
| `--yes`, `WN_YES=1` | no prompts: downloads the model, and installs Rust with rustup if a source build needs it |
| `--no-model`, `WN_NO_MODEL=1` | skip the model download |
| `--dry-run` | print what would happen; no download, clone or build |
| `--uninstall` | remove everything (runs `wn uninstall --yes`, see [Uninstall](#uninstall)); add `--keep-models` to keep the models |
| `WN_SETUP_AGENTS=1` / `=0` | connect agents without asking / never connect them |
| `WN_VERSION` | release to install (default: latest) |
| `WN_INSTALL_DIR` | where the release binary goes (default `~/.local/bin`) |
| `WN_MODEL_SOURCE` | pull the model from a local dir, an `https://` URL or `hf:owner/repo[@rev]` |
| `--ref <branch\|tag\|sha>`, `WN_FROM=source` | build from source instead (`--ref main` for the development branch) |
| `--force` | source builds: rebuild even if already up to date |
| `WN_HOME` | source builds: private clone location (default `~/.local/share/where-next`) |
| `WN_BIN_ROOT` | source builds: `cargo install --root` (default: cargo's own, usually `~/.cargo`) |
| `WN_REPO_URL` | source builds: source repository (default this repo; a local path works too) |

## Uninstall

```sh
wn uninstall                 # shows the full list, asks once
wn uninstall --yes           # no question
wn uninstall --keep-models   # keep the ~1.2 GB model
wn uninstall --dry-run       # only show the list
```

It removes, in this order:

1. The running daemon (stopped).
2. From Claude Code, Codex and Cursor: the where-next skill (`~/.claude/skills/where-next/`,
   `~/.agents/skills/where-next/`, `~/.cursor/skills/where-next/`, plus `wn setup --project`
   installs it recorded) and the where-next hook entries in `~/.claude/settings.json`,
   `~/.codex/hooks.json` and `~/.cursor/hooks.json`. Only entries `wn setup` owns are removed;
   your other settings and hooks stay, and a file restored to what it was before `wn setup` gets
   its original bytes back. A hooks file that `wn setup` created and that holds nothing else is
   deleted.
3. In `~/.cache/where-next` (`$WHERE_NEXT_HOME`): the per-repository index directories
   (`<name>-<16 hex digits>`), `daemon.log`, `daemon.sock`, `fingerprints.json`, `skills.json`,
   `hook-log.jsonl`, `hook-sessions/`; then the directory if that left it empty.
4. In `~/.cache/where-next-models` (`$WN_MODELS_HOME`), unless `--keep-models`: each model
   directory with a `wn-model.json` and interrupted `.<name>.pulling` downloads; then the directory
   if empty.
5. In `~/.local/share/where-next` (`$WN_HOME`): `src/`, the source checkout used by `wn update`.
6. The `wn` binary it runs as, and the files the release installer recorded beside it in
   `wn.install-files` (`libonnxruntime.so` on Linux, `wn.install-method`); for installs from before
   that file, `libonnxruntime.so` only when `wn.install-method` says `release`.
   Directories recorded as created by the release installer are removed only if empty.

Anything else in those directories stays. It refuses, before removing anything, when one of
them is empty, relative, `/`, a top-level directory, your home directory or a directory above
it, or holds files but nothing wn wrote.

It says what it removed and what it could not remove. It never edits your shell profile: if you
added the install directory to `PATH`, remove that line yourself. A `wn` binary inside a Homebrew
or npm installation is left to that package manager. For a `cargo install`, `cargo uninstall
where-next` also clears cargo's record.

You can also run:
`curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh -s -- --uninstall`
It runs `wn uninstall --yes` when `wn` works. Without a working `wn`, the installer removes the
binary, source checkout, caches and models. Remove where-next agent skills and hook entries by hand.

## Updating

```sh
wn update                 # release installs: re-run the installer for the latest release
                          # source installs: fetch main (or --ref) and rebuild if it moved
wn update --check         # exit 0: up to date, 10: update available, 1: error
wn --version              # wn 0.1.0 (abc1234 2026-09-30): the commit this binary was built from
```

Re-running the installer also updates.

## Build it yourself

```sh
git clone https://github.com/andreylukin/where-next
cd where-next
cargo install --path crates/wn-cli --locked
```

You need Rust, `git` and a C/C++ toolchain (`xcode-select --install` on macOS, `build-essential` on
Debian/Ubuntu). On macOS the build links a prebuilt ONNX Runtime. On Linux, `wn` loads ONNX
Runtime as a shared library at run time: put a compatible `libonnxruntime.so` (1.28) beside the
binary or point `ORT_DYLIB_PATH` at it; release archives include one. See [building.md](building.md).

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
