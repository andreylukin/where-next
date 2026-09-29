#!/bin/sh
# Install or update `wn` (where-next). Run it again any time to update to the latest main.
#
#   curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
#
# Default (source): clones https://github.com/andreylukin/where-next into $WN_HOME/src (a private
# clone; your other checkouts are never touched), checks out --ref (default: main) and builds it
# with `cargo install --path crates/wn-cli --locked`. Later runs fetch, and rebuild only when the
# ref moved. `wn update` does the same from inside wn. Then it offers the default model
# (gemma-xl1, ~1.2 GB from Hugging Face, Gemma Terms of Use) if it is missing or its pin moved.
#
# Options:
#   --ref <branch|tag|sha>  what to build (default: main)
#   --yes, -y               no prompts (install Rust with rustup if cargo is missing)
#   --force                 rebuild even if already up to date
#   --dry-run               print what would happen, change nothing
#   --uninstall             remove the wn binary and the private clone (keeps caches and models)
#   --no-model              do not download the default model (wn then uses a lexical fallback)
#
# Environment:
#   WN_HOME       private clone lives in $WN_HOME/src (default: ~/.local/share/where-next)
#   WN_BIN_ROOT   cargo install --root (default: cargo's default, usually ~/.cargo -> ~/.cargo/bin/wn)
#   WN_REPO_URL   source repository (default: https://github.com/andreylukin/where-next)
#   WN_YES=1      same as --yes
#   WN_NO_MODEL=1 same as --no-model
#   WN_MODEL_SOURCE  pull the model from here instead (local dir, https:// URL, hf:owner/repo[@rev])
#   WN_FROM=release  install a prebuilt, checksum-verified release binary instead (no releases yet):
#     WN_VERSION (default: latest), WN_INSTALL_DIR (default: ~/.local/bin), WN_DOWNLOAD_BASE, WN_TARGET
set -eu

repo="andreylukin/where-next"

say() { printf 'wn-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

fetch() { # url dest
  if command -v curl >/dev/null 2>&1; then
    curl --proto '=https,file' --tlsv1.2 -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$1" -O "$2"
  else
    die "need curl or wget"
  fi
}

# ---------------------------------------------------------------- release mode (WN_FROM=release)

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$arch" in
    arm64 | aarch64) arch="aarch64" ;;
    x86_64 | amd64) arch="x86_64" ;;
    *) die "unsupported architecture: $arch" ;;
  esac
  case "$os" in
    Darwin)
      [ "$arch" = "aarch64" ] || die "Intel Macs are not supported yet (no prebuilt ONNX Runtime); build from source"
      echo "$arch-apple-darwin"
      ;;
    Linux) echo "$arch-unknown-linux-gnu" ;;
    *) die "unsupported OS: $os (Windows: download the .zip from the releases page)" ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else die "need sha256sum or shasum to verify the download"; fi
}

install_release() {
  install_dir="${WN_INSTALL_DIR:-$HOME/.local/bin}"
  version="${WN_VERSION:-latest}"
  target="${WN_TARGET:-$(detect_target)}"
  archive="wn-$target.tar.gz"
  if [ -n "${WN_DOWNLOAD_BASE:-}" ]; then
    base="$WN_DOWNLOAD_BASE"
  elif [ "$version" = "latest" ]; then
    base="https://github.com/$repo/releases/latest/download"
  else
    base="https://github.com/$repo/releases/download/$version"
  fi

  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  say "downloading $archive"
  fetch "$base/$archive" "$tmp/$archive" || die "download failed (no release for $target yet?)"
  fetch "$base/$archive.sha256" "$tmp/$archive.sha256" || die "checksum file missing; refusing to install"

  expected="$(cut -d' ' -f1 < "$tmp/$archive.sha256")"
  actual="$(sha256_of "$tmp/$archive")"
  [ -n "$expected" ] || die "empty checksum file; refusing to install"
  [ "$expected" = "$actual" ] || die "checksum mismatch for $archive (expected $expected, got $actual)"
  say "checksum ok ($actual)"

  tar -xzf "$tmp/$archive" -C "$tmp"
  [ -f "$tmp/wn-$target/wn" ] || die "archive does not contain wn"
  mkdir -p "$install_dir"
  install -m 0755 "$tmp/wn-$target/wn" "$install_dir/wn"
  say "installed $install_dir/wn"
  case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) say "add $install_dir to your PATH" ;;
  esac
  say "next: cd your-repo && wn init"
}

# ---------------------------------------------------------------- source mode (default)

git_ref="main"
yes="${WN_YES:-}"
force=""
dry_run=""
uninstall=""
no_model="${WN_NO_MODEL:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --ref) [ $# -ge 2 ] || die "--ref needs a value"; git_ref="$2"; shift ;;
    --ref=*) git_ref="${1#--ref=}" ;;
    --yes | -y) yes=1 ;;
    --force) force=1 ;;
    --dry-run) dry_run=1 ;;
    --uninstall) uninstall=1 ;;
    --no-model) no_model=1 ;;
    -h | --help) sed -n '2,28p' "$0" 2>/dev/null || say "see https://github.com/$repo/blob/main/install.sh"; exit 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
  shift
done

if [ "${WN_FROM:-source}" = "release" ]; then
  install_release
  exit 0
fi

repo_url="${WN_REPO_URL:-https://github.com/$repo}"
wn_home="${WN_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/where-next}"
src="$wn_home/src"
bin_root="${WN_BIN_ROOT:-}"
bin_dir="${bin_root:-${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}}/bin"

run() { # print and run, or only print with --dry-run
  if [ -n "$dry_run" ]; then say "would run: $*"; else "$@"; fi
}

find_cargo() {
  if command -v cargo >/dev/null 2>&1; then command -v cargo
  elif [ -x "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" ]; then echo "${CARGO_HOME:-$HOME/.cargo}/bin/cargo"
  fi
}

ask() { # question -> 0 yes / 1 no; reads the terminal even when this script is piped to sh
  [ -n "$yes" ] && return 0
  if [ -r /dev/tty ] && [ -w /dev/tty ] && (: < /dev/tty) 2>/dev/null; then
    printf 'wn-install: %s [y/N] ' "$1" > /dev/tty
    read -r answer < /dev/tty || answer=""
    case "$answer" in y | Y | yes | Yes) return 0 ;; esac
  fi
  return 1
}

model_pull() { # extra args... ; honours WN_MODEL_SOURCE
  if [ -n "${WN_MODEL_SOURCE:-}" ]; then
    "$bin_dir/wn" model pull "$@" --source "$WN_MODEL_SOURCE"
  else
    "$bin_dir/wn" model pull "$@"
  fi
}

ensure_model() { # offer the default model when it is missing or its pinned source moved
  if [ -n "$no_model" ]; then
    say "skipping the model (--no-model); install it later with: wn model pull"
    return 0
  fi
  if [ -n "$dry_run" ]; then
    say "would check the default model (wn model pull --check) and offer to download it"
    return 0
  fi
  status=0
  model_pull --check >/dev/null 2>&1 || status=$?
  case "$status" in
    0) say "model: gemma-xl1 is installed and current"; return 0 ;;
    10) ;;
    *) say "note: could not check the model (exit $status); install it with: wn model pull"; return 0 ;;
  esac
  say "wn needs a model for good hints: gemma-xl1 (~1.2 GB) from ${WN_MODEL_SOURCE:-huggingface.co/lukandrey/where-next-gemma-xl1},"
  say "  fine-tuned from Google's EmbeddingGemma and provided under the Gemma Terms of Use"
  say "  (https://ai.google.dev/gemma/terms). Without it, wn uses a lexical fallback."
  if ask "download the model now?"; then
    model_pull || say "model download failed; retry with: wn model pull"
  else
    say "skipped the model; install it later with: wn model pull   (or re-run with --yes)"
  fi
}

installed_commit() { # short sha from `wn --version`, if an installed wn reports one
  if [ -x "$bin_dir/wn" ]; then
    "$bin_dir/wn" --version 2>/dev/null | sed -n 's/.*(\([0-9a-f]\{7,40\}\).*/\1/p' | head -n 1
  fi
}

if [ -n "$uninstall" ]; then
  cargo_bin="$(find_cargo)"
  if [ -n "$cargo_bin" ] && [ -x "$bin_dir/wn" ]; then
    if [ -n "$bin_root" ]; then
      run "$cargo_bin" uninstall where-next --root "$bin_root" || run rm -f "$bin_dir/wn"
    else
      run "$cargo_bin" uninstall where-next || run rm -f "$bin_dir/wn"
    fi
  elif [ -e "$bin_dir/wn" ]; then
    run rm -f "$bin_dir/wn"
  fi
  [ -d "$src" ] && run rm -rf "$src"
  say "uninstalled wn (binary and $src)"
  say "caches and models are kept: ~/.cache/where-next and ~/.cache/where-next-models (remove them yourself if you want)"
  exit 0
fi

command -v git >/dev/null 2>&1 || die "git is required (install it and re-run)"

cargo_bin="$(find_cargo)"
if [ -z "$cargo_bin" ]; then
  rustup_cmd="curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal"
  say "cargo (Rust) is needed to build wn from source. Install it with:"
  say "  $rustup_cmd"
  if [ -n "$dry_run" ]; then
    say "would install Rust with rustup"
    cargo_bin="cargo"
  elif ask "install Rust now with rustup?"; then
    sh -c "$rustup_cmd" || die "rustup failed"
    cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin/cargo"
    [ -x "$cargo_bin" ] || die "rustup finished but cargo was not found at $cargo_bin"
  else
    die "cargo not found; install Rust with the command above (or re-run with --yes)"
  fi
fi

# Clone once, then fetch the requested ref.
if [ -d "$src/.git" ]; then
  old="$(git -C "$src" rev-parse HEAD 2>/dev/null || echo "")"
  run git -C "$src" remote set-url origin "$repo_url"
elif [ -e "$src" ] && [ -n "$(ls -A "$src" 2>/dev/null)" ]; then
  die "$src exists but is not a git clone; remove it or set WN_HOME"
else
  old=""
  say "cloning $repo_url into $src"
  run mkdir -p "$wn_home"
  run git clone --quiet "$repo_url" "$src"
fi

if [ -n "$dry_run" ] && [ ! -d "$src/.git" ]; then
  target="$(git ls-remote "$repo_url" "$git_ref" 2>/dev/null | head -n 1 | cut -f1)"
  target="${target:-$git_ref}"
else
  if ! git -C "$src" fetch --quiet --force origin "$git_ref" 2>/dev/null; then
    git -C "$src" fetch --quiet --force --tags origin || die "git fetch failed"
    target="$(git -C "$src" rev-parse --verify "$git_ref^{commit}" 2>/dev/null)" || die "ref '$git_ref' not found in $repo_url"
  else
    target="$(git -C "$src" rev-parse "FETCH_HEAD^{commit}")"
  fi
fi

short_target="$(printf '%s' "$target" | cut -c1-7)"
current="$(installed_commit)"
[ -n "$current" ] || current="$(printf '%s' "$old" | cut -c1-7)"
if [ -z "$force" ] && [ -n "$current" ] && [ "$current" = "$short_target" ] && [ -x "$bin_dir/wn" ]; then
  say "wn is up to date ($short_target on $git_ref)"
  ensure_model
  exit 0
fi

run git -C "$src" checkout --quiet --force --detach "$target"
say "building wn $short_target ($git_ref) with cargo; the first build takes a few minutes"
if [ -n "$bin_root" ]; then
  run "$cargo_bin" install --path "$src/crates/wn-cli" --locked --force --root "$bin_root"
else
  run "$cargo_bin" install --path "$src/crates/wn-cli" --locked --force
fi

if [ -n "$dry_run" ]; then
  ensure_model
  say "dry run: nothing was changed"
  exit 0
fi

if [ -n "$current" ]; then
  say "updated wn: $current -> $short_target ($git_ref)"
  # Replace a daemon from the old build and re-sync agent skills installed by `wn skill sync`.
  "$bin_dir/wn" daemon stop >/dev/null 2>&1 || true
  "$bin_dir/wn" skill sync --yes --from-state >/dev/null 2>&1 || say "note: run 'wn skill sync' to update agent skills"
else
  say "installed wn $short_target ($git_ref) at $bin_dir/wn"
fi
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) say "add $bin_dir to your PATH (e.g. export PATH=\"$bin_dir:\$PATH\")" ;;
esac
ensure_model
say "update later with: wn update   (or re-run this installer)"
say "next steps:"
say "  cd your-repo && wn init        # index + learn from this repo's git history"
say "  wn ask \"where is X handled?\"   # ranked files to open next"
say "  wn skill sync                  # teach Claude Code / Codex / Cursor when to call wn"
