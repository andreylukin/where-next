#!/bin/sh
# Install `wn` from a GitHub release, verifying its SHA-256 checksum before anything is installed.
#
#   curl -fsSL https://raw.githubusercontent.com/andreylukin/where-next/main/install.sh | sh
#
# Not live yet: there are no releases. Environment variables:
#   WN_VERSION         release tag to install (default: latest)
#   WN_INSTALL_DIR     where to put the binary (default: ~/.local/bin)
#   WN_DOWNLOAD_BASE   override the download base URL (used by tests; must contain the archive,
#                      its .sha256 file, and nothing else is fetched)
set -eu

repo="andreylukin/where-next"
install_dir="${WN_INSTALL_DIR:-$HOME/.local/bin}"
version="${WN_VERSION:-latest}"

say() { printf 'wn-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$arch" in
    arm64 | aarch64) arch="aarch64" ;;
    x86_64 | amd64) arch="x86_64" ;;
    *) die "unsupported architecture: $arch" ;;
  esac
  case "$os" in
    Darwin) echo "$arch-apple-darwin" ;;
    Linux) echo "$arch-unknown-linux-gnu" ;;
    *) die "unsupported OS: $os (Windows: download the .zip from the releases page)" ;;
  esac
}

fetch() { # url dest
  if command -v curl >/dev/null 2>&1; then
    curl --proto '=https,file' --tlsv1.2 -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$1" -O "$2"
  else
    die "need curl or wget"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else die "need sha256sum or shasum to verify the download"; fi
}

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
