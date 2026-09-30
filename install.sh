#!/bin/sh
# Install or update `wn` from a checksum-verified release binary by default.
# Usage: sh install.sh [--ref branch|tag|sha] [--yes] [--no-model] [--dry-run] [--uninstall [--keep-models]]
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

fetch_archive() { # url dest; 2 means archive absent
  case "$1" in file://*) [ -f "${1#file://}" ] || return 2 ;; esac
  if command -v curl >/dev/null 2>&1; then
    status="$(curl --proto '=https,file' --tlsv1.2 -sSL -w '%{http_code}' "$1" -o "$2")" || die "release download failed for $1 (HTTP ${status:-unknown})"
    case "$status" in 200 | 000) return 0 ;; 404) return 2 ;; *) die "release download failed for $1 (HTTP $status)" ;; esac
  elif command -v wget >/dev/null 2>&1; then
    if wget -q --server-response "$1" -O "$2" 2>"$tmp/wget.err"; then return 0; fi
    if awk '/^[[:space:]]*HTTP\// { code=$2 } END { exit code == 404 ? 0 : 1 }' "$tmp/wget.err"; then return 2; fi
    die "release download failed for $1 ($(cat "$tmp/wget.err"))"
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
      if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then arch=aarch64; fi
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

require_compatible_glibc() {
  if [ "$(uname -s)" != Linux ] && [ -z "${WN_GLIBC:-}" ]; then return 0; fi
  libc="$(glibc_version)"
  case "$libc" in
    *.*)
      major="${libc%%.*}"; minor="${libc#*.}"
      case "$major:$minor" in :* | *: | *[!0-9:]*) libc="unknown" ;; esac
      ;;
    *) libc="unknown" ;;
  esac
  if [ "$libc" = unknown ] || [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 35 ]; }; then
    say "detected glibc version: $libc"
    say "prebuilt binaries need glibc >= 2.35 (Ubuntu 22.04+, Debian 12+)"
    say "a source build needs a compatible ONNX Runtime shared library"
    die "run in an ubuntu:22.04 container, or set WN_FROM=source to try anyway"
  fi
}

install_release() {
  install_dir="${WN_INSTALL_DIR:-$HOME/.local/bin}"
  manifest="$install_dir/wn.install-files"
  bin_owned=""; local_owned=""; cache_owned=""
  if [ "$install_dir" = "$HOME/.local/bin" ]; then
    if [ ! -d "$install_dir" ] || { [ -f "$manifest" ] && grep -Fxq "dir:$install_dir" "$manifest"; }; then bin_owned=1; fi
    if [ ! -d "$HOME/.local" ] || { [ -f "$manifest" ] && grep -Fxq "dir:$HOME/.local" "$manifest"; }; then local_owned=1; fi
  fi
  if [ ! -d "$HOME/.cache" ] || { [ -f "$manifest" ] && grep -Fxq "dir:$HOME/.cache" "$manifest"; }; then cache_owned=1; fi
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

  tmp="$(mktemp -d)" || die "could not create temporary directory"
  trap 'rm -rf "$tmp"' EXIT
  trap 'exit 1' INT TERM

  say "downloading $archive"
  archive_status=0
  fetch_archive "$base/$archive" "$tmp/$archive" || archive_status=$?
  if [ "$archive_status" -eq 2 ]; then
    rm -rf "$tmp" || die "could not clean temporary directory"
    trap - EXIT INT TERM
    return 2
  fi
  [ "$archive_status" -eq 0 ] || die "release download failed for $archive"
  fetch "$base/$archive.sha256" "$tmp/$archive.sha256" || die "checksum file missing; refusing to install"

  expected="$(cut -d' ' -f1 < "$tmp/$archive.sha256")" || die "could not read checksum file"
  actual="$(sha256_of "$tmp/$archive")" || die "could not calculate checksum"
  [ -n "$expected" ] || die "empty checksum file; refusing to install"
  [ "$expected" = "$actual" ] || die "checksum mismatch for $archive (expected $expected, got $actual)"
  say "checksum ok ($actual)"

  tar -xzf "$tmp/$archive" -C "$tmp" || die "could not extract $archive"
  [ -f "$tmp/wn-$target/wn" ] || die "archive does not contain wn"
  case "$target" in
    *linux*) [ -f "$tmp/wn-$target/libonnxruntime.so" ] || die "archive does not contain libonnxruntime.so" ;;
  esac
  mkdir -p "$install_dir" || die "could not create $install_dir"
  if [ -x "$install_dir/wn" ]; then "$install_dir/wn" daemon stop >/dev/null 2>&1 || true; fi
  if [ -f "$tmp/wn-$target/libonnxruntime.so" ]; then
    install -m 0644 "$tmp/wn-$target/libonnxruntime.so" "$install_dir/libonnxruntime.so" || die "could not install $install_dir/libonnxruntime.so"
  fi
  install -m 0755 "$tmp/wn-$target/wn" "$install_dir/wn" || die "could not install $install_dir/wn"
  printf 'release\n' > "$install_dir/wn.install-method" || die "could not write install marker"
  # What this installer put here, so `wn uninstall` removes exactly these files.
  {
    printf '%s\n' "$install_dir/wn" "$install_dir/wn.install-method" "$install_dir/wn.install-files"
    if [ -f "$install_dir/libonnxruntime.so" ]; then printf '%s\n' "$install_dir/libonnxruntime.so"; fi
    if [ -n "$bin_owned" ]; then printf 'dir:%s\n' "$install_dir"; fi
    if [ -n "$local_owned" ]; then printf 'dir:%s\n' "$HOME/.local"; fi
  } > "$install_dir/wn.install-files" || die "could not write $install_dir/wn.install-files"
  bin_dir="$install_dir"
  say "installed $install_dir/wn"
  case ":$PATH:" in
    *":$install_dir:"*) ;;
    *)
      if [ "$install_dir" = "$HOME/.local/bin" ]; then
        case "${SHELL:-}" in
          */zsh) say "add this line to your shell startup file: echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.zshrc" ;;
          */bash)
            if [ "$(uname -s)" = Darwin ]; then profile=.bash_profile; else profile=.bashrc; fi
            say "add this line to your shell startup file: echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/$profile"
            ;;
          */fish) say 'run: fish_add_path ~/.local/bin' ;;
          *) say "add this line to your shell startup file: export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
        esac
        case "${SHELL:-}" in
          */fish) say 'then open a new terminal or run: fish_add_path ~/.local/bin' ;;
          *) say "then open a new terminal or run: export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
        esac
      else
        say "add $install_dir to your PATH"
      fi
      ;;
  esac
  rm -rf "$tmp" || die "could not clean temporary directory"
  trap - EXIT INT TERM
  return 0
}

record_cache_dir() {
  if [ -n "$cache_owned" ] && [ -d "$HOME/.cache" ] && ! grep -Fxq "dir:$HOME/.cache" "$manifest"; then
    printf 'dir:%s\n' "$HOME/.cache" >> "$manifest" || die "could not update $manifest"
  fi
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
agents_connected=""
keep_models=""

while [ $# -gt 0 ]; do
  case "$1" in
    --ref) [ $# -ge 2 ] || die "--ref needs a value"; git_ref="$2"; from=source; shift ;;
    --ref=*) git_ref="${1#--ref=}"; from=source ;;
    --yes | -y) yes=1 ;;
    --force) force=1 ;;
    --dry-run) dry_run=1 ;;
    --uninstall) uninstall=1 ;;
    --keep-models) keep_models=1 ;;
    --no-model) no_model=1 ;;
    -h | --help) say "usage: install.sh [--ref REF] [--yes] [--no-model] [--dry-run] [--uninstall [--keep-models]]"; return 0 ;;
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

tty="${WN_INSTALL_TTY:-/dev/tty}" # the terminal to ask on (tests point this at a file)
has_tty() { [ -r "$tty" ] && [ -w "$tty" ] && (: < "$tty") 2>/dev/null; }

ask() { # question -> 0 yes / 1 no; reads the terminal even when this script is piped to sh
  [ -n "$yes" ] && return 0
  if has_tty; then
    printf 'wn-install: %s [y/N] ' "$1" >> "$tty"
    read -r answer < "$tty" || answer=""
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

agents_found() { # prints "Claude Code / Codex / Cursor" for the agents configured in $HOME
  found=""
  if [ -d "$HOME/.claude" ]; then found="Claude Code"; fi
  if [ -d "$HOME/.codex" ] || [ -d "$HOME/.agents" ]; then found="${found:+$found / }Codex"; fi
  if [ -d "$HOME/.cursor" ]; then found="${found:+$found / }Cursor"; fi
  printf '%s' "$found"
}

connect_agents() { # after the model step: connect detected agents (skill + hooks); default yes
  found="$(agents_found)"
  [ -n "$found" ] || return 0
  if [ -n "$dry_run" ]; then say "would offer to connect wn to $found (wn setup)"; return 0; fi
  setup_now="${WN_SETUP_AGENTS:-}"
  if [ "$setup_now" = 0 ]; then say "not connecting agents (WN_SETUP_AGENTS=0); later: wn setup"; return 0; fi
  if [ "$setup_now" != 1 ] && [ -z "$yes" ]; then
    # Connecting is the default answer; only a run with no terminal to ask on (and no --yes) skips.
    if ! has_tty; then
      say "no terminal to ask on: agents not connected (--yes or WN_SETUP_AGENTS=1 connects them)"
      return 0
    fi
    "$bin_dir/wn" setup --dry-run >&2 || { say "note: wn setup --dry-run failed; later: wn setup"; return 0; }
    printf 'wn-install: Connect wn to %s (skill + hooks)? [Y/n] ' "$found" >> "$tty"
    # Enter means yes; a failed read (no terminal after all, EOF) means no.
    if ! read -r answer < "$tty"; then say "no answer; agents not connected (later: wn setup)"; return 0; fi
    case "$answer" in n | N | no | No) say "skipped; later: wn setup"; return 0 ;; esac
  fi
  if "$bin_dir/wn" setup --yes >&2; then agents_connected=1; else say "note: wn setup failed; run it again: wn setup"; fi
}

next_steps() {
  say "next steps:"
  if [ -z "$agents_connected" ]; then say "  wn setup                      # connect Claude Code / Codex / Cursor: hints arrive automatically"; fi
  if [ -n "$model_skipped" ]; then say "  wn model pull                 # download the default model for semantic hints"; fi
  say "  cd your-repo && wn init        # index and learn from git history"
  say "  wn ask \"where is X handled?\"   # ranked files to open next"
  say "  wn stats                       # did your agents use the hints?"
}

installed_commit() { # short sha from `wn --version`, if an installed wn reports one
  if [ -x "$bin_dir/wn" ]; then
    "$bin_dir/wn" --version 2>/dev/null | sed -n 's/.*(\([0-9a-f]\{7,40\}\).*/\1/p' | head -n 1
  fi
}

if [ -z "$uninstall" ] && [ "$from" != source ]; then
  if [ "$from" != auto ] && [ "$from" != release ]; then die "WN_FROM must be auto, release, or source"; fi
  require_compatible_glibc
  if [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" != 1 ]; then
    die "Intel Macs aren't supported yet (ONNX Runtime has no macOS x86_64 prebuilt)"
  fi
  if target="${WN_TARGET:-$(detect_target)}"; then
    if [ -n "$dry_run" ]; then
      say "would install checksum-verified release binary for $target"
      return 0
    fi
    if install_release; then
      ensure_model
      connect_agents
      record_cache_dir
      say "update later with: wn update"
      next_steps
      return 0
    fi
    say "release archive absent for $target; falling back to source build"
  else
    die "no release binary for this target"
  fi
fi

safe_root() { # dir -> 0 when it may hold wn files to remove: absolute, not /, top-level, $HOME or above
  d="${1%/}"
  case "$d" in /*) ;; *) return 1 ;; esac
  [ "$(dirname "$d")" != / ] || return 1
  h="${HOME%/}"
  if [ -d "$d" ] && [ -d "$h" ]; then
    d="$(cd "$d" && pwd -P)" || return 1
    h="$(cd "$h" && pwd -P)" || return 1
  fi
  [ "$d" != "$h" ] || return 1
  case "$h/" in "$d"/*) return 1 ;; esac
  return 0
}

hex16='[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]'

remove_cache() { # only what wn writes there, then the directory if that left it empty
  for f in daemon.log daemon.sock skills.json hook-log.jsonl fingerprints.json; do
    if [ -e "$1/$f" ]; then run rm -f "$1/$f" || die "could not remove $1/$f"; fi
  done
  for d in "$1"/*; do
    [ -d "$d" ] || continue
    # shellcheck disable=SC2254 # $hex16 is a pattern
    case "${d##*/}" in
      hook-sessions | *-$hex16) ;;
      models) ls "$d"/*/wn-model.json "$d"/*/model.safetensors >/dev/null 2>&1 || continue ;;
      *) continue ;;
    esac
    run rm -rf "$d" || die "could not remove $d"
  done
  rmdir "$1" 2>/dev/null || true
}

remove_models() {
  for d in "$1"/* "$1"/.*.pulling; do
    if [ -f "$d/wn-model.json" ] || { [ -d "$d" ] && case "$d" in *.pulling) true ;; *) false ;; esac; }; then
      run rm -rf "$d" || die "could not remove $d"
    fi
  done
  rmdir "$1" 2>/dev/null || true
}

if [ -n "$uninstall" ]; then
  release_bin="${WN_INSTALL_DIR:-$HOME/.local/bin}/wn"
  cache_dir="${WHERE_NEXT_HOME:-$HOME/.cache/where-next}"
  models_dir="${WN_MODELS_HOME:-$HOME/.cache/where-next-models}"
  for root in "$cache_dir" "$models_dir" "$wn_home"; do
    safe_root "$root" || die "refusing to remove files under '$root' (it is /, a top-level directory, your home or above it, or not absolute); nothing was removed"
  done
  # `wn uninstall` removes everything: agent skill and hooks, daemon, caches, models, the source
  # checkout and the binary. The steps after it remove files when no wn is left to ask.
  wn_bin=""
  agents_removed=""
  for bin in "$release_bin" "$bin_dir/wn"; do
    if [ -x "$bin" ]; then wn_bin="$bin"; break; fi
  done
  if [ -n "$wn_bin" ]; then
    if [ -n "$keep_models" ]; then
      if run "$wn_bin" uninstall --yes --keep-models >&2; then agents_removed=1; else say "note: wn uninstall failed; removing files directly"; fi
    else
      if run "$wn_bin" uninstall --yes >&2; then agents_removed=1; else say "note: wn uninstall failed; removing files directly"; fi
    fi
  fi
  if [ -z "$agents_removed" ]; then say "remove where-next agent skills and hook entries by hand"; fi
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
  release_dir="${release_bin%/*}"
  if [ -e "$release_dir/wn.install-method" ]; then
    # The installer's own files, only where its marker says it installed a release.
    for f in "$release_bin" "$release_dir/libonnxruntime.so" "$release_dir/wn.install-files" "$release_dir/wn.install-method"; do
      if [ -e "$f" ]; then run rm -f "$f" || die "could not remove $f"; fi
    done
  elif [ -e "$release_bin" ]; then
    run rm -f "$release_bin" || die "could not remove $release_bin"
  fi
  if [ -d "$src/.git" ] && [ -d "$src/crates/wn-cli" ]; then run rm -rf "$src" || die "could not remove $src"; fi
  rmdir "$wn_home" 2>/dev/null || true
  if [ -d "$cache_dir" ]; then remove_cache "$cache_dir"; fi
  if [ -z "$keep_models" ] && [ -d "$models_dir" ]; then remove_models "$models_dir"; fi
  say "uninstalled wn: binary, $src, $cache_dir$([ -n "$keep_models" ] || printf ', %s' "$models_dir")"
  if [ -n "$agents_removed" ]; then say "removed the where-next skill and hooks in your agents"; fi
  if [ -n "$keep_models" ]; then say "kept the models in $models_dir (--keep-models)"; fi
  say "installs made with wn setup --project stay in those repositories"
  exit 0
fi

if [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" != 1 ]; then
  die "Intel Macs aren't supported yet (ONNX Runtime has no macOS x86_64 prebuilt)"
fi

command -v git >/dev/null 2>&1 || die "git is required (install it and re-run)"
if [ -z "$git_ref" ]; then
  git_ref="$(git ls-remote --tags --refs "$repo_url" 'refs/tags/v*' | sed 's@.*refs/tags/@@' | awk '/^v[0-9]+(\.[0-9]+)*$/ { split(substr($0,2), n, "."); printf "%010d.%010d.%010d.%010d %s\n", n[1], n[2], n[3], n[4], $0 }' | sort | tail -n 1 | cut -d' ' -f2)"
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

rm -f "$bin_dir/wn.install-method" "$bin_dir/wn.install-files"
if [ -n "$current" ]; then
  say "updated wn: $current -> $short_target ($git_ref)"
  # Replace a daemon from the old build and re-sync agent skills and hooks installed by `wn setup`.
  "$bin_dir/wn" daemon stop >/dev/null 2>&1 || true
  "$bin_dir/wn" setup --yes --from-state >/dev/null 2>&1 || say "note: run 'wn setup' to update agent skills and hooks"
else
  say "installed wn $short_target ($git_ref) at $bin_dir/wn"
fi
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) say "add $bin_dir to your PATH (e.g. export PATH=\"$bin_dir:\$PATH\")" ;;
esac
ensure_model
connect_agents
say "update later with: wn update   (or re-run this installer)"
next_steps

}
main "$@"
