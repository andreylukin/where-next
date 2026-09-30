#!/bin/sh
# Install or update `wn` from a checksum-verified release binary by default.
# Usage: sh install.sh [--ref branch|tag|sha] [--yes] [--no-model] [--dry-run] [--uninstall]
# --ref and WN_FROM=source build from source; source defaults to the latest release tag.
# WN_RELEASE_BASE overrides the release download URL (useful for local mirrors/tests).
# WN_GLIBC overrides detected glibc for installer tests.
set -eu

main() {
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
    *) return 1 ;;
  esac
  case "$os" in
    Darwin)
      [ "$arch" = "aarch64" ] || return 1
      echo "$arch-apple-darwin"
      ;;
    Linux) echo "$arch-unknown-linux-gnu" ;;
    *) return 1 ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else die "need sha256sum or shasum to verify the download"; fi
}

glibc_version() {
  if [ -n "${WN_GLIBC:-}" ]; then printf '%s\n' "$WN_GLIBC"; return; fi
  if command -v getconf >/dev/null 2>&1; then
    detected="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
    case "$detected" in 'glibc '*) printf '%s\n' "${detected#glibc }"; return ;; esac
  fi
  ldd --version 2>&1 | head -n 1 | sed -n 's/.* \([0-9][0-9]*\.[0-9][0-9]*\).*$/\1/p'
}

release_compatible() {
  if [ "$(uname -s)" != Linux ] && [ -z "${WN_GLIBC:-}" ]; then return 0; fi
  libc="$(glibc_version)"
  if [ -z "$libc" ]; then say "Linux libc could not be identified; falling back to source build"; return 1; fi
  major="${libc%%.*}"; minor="${libc#*.}"; minor="${minor%%.*}"
  if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 39 ]; }; then
    say "glibc $libc is below the release binary requirement (2.39); falling back to source build (ONNX Runtime source linking may also require newer glibc/GCC)"
    return 1
  fi
}

install_release() {
  install_dir="${WN_INSTALL_DIR:-$HOME/.local/bin}"
  version="${WN_VERSION:-latest}"
  target="${WN_TARGET:-$(detect_target)}"
  archive="wn-$target.tar.gz"
  if [ -n "${WN_RELEASE_BASE:-${WN_DOWNLOAD_BASE:-}}" ]; then
    base="${WN_RELEASE_BASE:-$WN_DOWNLOAD_BASE}"
  elif [ "$version" = "latest" ]; then
    base="https://github.com/$repo/releases/latest/download"
  else
    base="https://github.com/$repo/releases/download/$version"
  fi

  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  say "downloading $archive"
  if ! fetch "$base/$archive" "$tmp/$archive"; then
    rm -rf "$tmp"; trap - EXIT
    return 2
  fi
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
  printf 'release\n' > "$install_dir/wn.install-method"
  bin_dir="$install_dir"
  say "installed $install_dir/wn"
  case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) say "add $install_dir to your PATH" ;;
  esac
  rm -rf "$tmp"; trap - EXIT
  return 0
}

# ---------------------------------------------------------------- source mode (default)

git_ref=""
from="${WN_FROM:-auto}"
yes="${WN_YES:-}"
force=""
dry_run=""
uninstall=""
no_model="${WN_NO_MODEL:-}"
model_skipped=""

while [ $# -gt 0 ]; do
  case "$1" in
    --ref) [ $# -ge 2 ] || die "--ref needs a value"; git_ref="$2"; from=source; shift ;;
    --ref=*) git_ref="${1#--ref=}"; from=source ;;
    --yes | -y) yes=1 ;;
    --force) force=1 ;;
    --dry-run) dry_run=1 ;;
    --uninstall) uninstall=1 ;;
    --no-model) no_model=1 ;;
    -h | --help) say "usage: install.sh [--ref REF] [--yes] [--no-model] [--dry-run] [--uninstall]"; return 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
  shift
done

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
    model_skipped=1
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
    model_pull || { model_skipped=1; say "model download failed; retry with: wn model pull"; }
  else
    model_skipped=1
    say "skipped the model; install it later with: wn model pull   (or re-run with --yes)"
  fi
}

next_steps() {
  say "next steps:"
  if [ -n "$model_skipped" ]; then say "  wn model pull                 # download the default model for semantic hints"; fi
  say "  cd your-repo && wn init        # index and learn from git history"
  say "  wn ask \"where is X handled?\"   # ranked files to open next"
  say "  wn skill sync                  # install agent skills"
}

installed_commit() { # short sha from `wn --version`, if an installed wn reports one
  if [ -x "$bin_dir/wn" ]; then
    "$bin_dir/wn" --version 2>/dev/null | sed -n 's/.*(\([0-9a-f]\{7,40\}\).*/\1/p' | head -n 1
  fi
}

if [ -z "$uninstall" ] && [ "$from" != source ]; then
  if [ "$from" != auto ] && [ "$from" != release ]; then die "WN_FROM must be auto, release, or source"; fi
  if target="${WN_TARGET:-$(detect_target)}"; then
    if release_compatible; then
      if [ -n "$dry_run" ]; then
        say "would install checksum-verified release binary for $target"
        return 0
      fi
      if install_release; then
        ensure_model
        say "update later with: wn update"
        next_steps
        return 0
      fi
      say "release archive unavailable for $target; falling back to source build"
    fi
  else
    say "no release binary for this target; falling back to source build"
  fi
fi

if [ -n "${WN_DECISION_ONLY:-}" ]; then say "source fallback selected"; return 0; fi

if [ -n "$uninstall" ]; then
  release_bin="${WN_INSTALL_DIR:-$HOME/.local/bin}/wn"
  if [ -x "$release_bin" ]; then run "$release_bin" daemon stop || true; fi
  if [ -x "$bin_dir/wn" ] && [ "$bin_dir/wn" != "$release_bin" ]; then run "$bin_dir/wn" daemon stop || true; fi
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
  [ -e "$release_bin" ] && run rm -f "$release_bin"
  [ -e "${release_bin}.install-method" ] && run rm -f "${release_bin}.install-method"
  [ -d "$src" ] && run rm -rf "$src"
  say "uninstalled wn (binary and $src)"
  say "agent skill files remain in ~/.claude/skills/where-next, ~/.agents/skills/where-next, ~/.cursor/skills/where-next (and project equivalents); reinstall wn to run: wn skill sync --uninstall"
  say "caches and models are kept: ~/.cache/where-next and ~/.cache/where-next-models (remove them yourself if you want)"
  exit 0
fi

command -v git >/dev/null 2>&1 || die "git is required (install it and re-run)"
if [ -z "$git_ref" ]; then
  git_ref="$(git ls-remote --tags --refs "$repo_url" 'refs/tags/v*' | sed 's@.*refs/tags/@@' | sort -V | tail -n 1)"
  [ -n "$git_ref" ] || die "could not find a release tag in $repo_url (use --ref main for the development branch)"
fi

cargo_bin="$(find_cargo)"
if [ -z "$cargo_bin" ]; then
  rustup_cmd="curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal"
  say "cargo (Rust) is needed to build wn from source. Install it with:"
  say "  $rustup_cmd"
  say "rustup will edit your shell startup files to add cargo to PATH"
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

rm -f "$bin_dir/wn.install-method"
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
next_steps

}
main "$@"
