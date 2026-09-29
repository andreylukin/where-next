#!/usr/bin/env bash
# Tests install.sh against a local fixture release (file:// URLs): a good archive installs, a
# tampered archive or a missing checksum file is refused and installs nothing.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
target="x86_64-unknown-linux-gnu"

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi; }

make_release() { # dir
  local dir="$1"
  mkdir -p "$dir/stage/wn-$target"
  printf '#!/bin/sh\necho wn-fixture\n' > "$dir/stage/wn-$target/wn"
  chmod +x "$dir/stage/wn-$target/wn"
  tar -czf "$dir/wn-$target.tar.gz" -C "$dir/stage" "wn-$target"
  (cd "$dir" && sha256 "wn-$target.tar.gz" > "wn-$target.tar.gz.sha256")
}

run_install() { # base bindir
  WN_DOWNLOAD_BASE="$1" WN_INSTALL_DIR="$2" WN_TARGET="$target" sh "$root/install.sh"
}

fail() { echo "FAIL: $*" >&2; exit 1; }

# 1. Good release installs and runs.
make_release "$work/good"
run_install "file://$work/good" "$work/bin1" 2>/dev/null
[ "$("$work/bin1/wn")" = "wn-fixture" ] || fail "installed binary does not run"

# 2. Tampered archive is refused.
make_release "$work/bad"
echo "tamper" >> "$work/bad/wn-$target.tar.gz"
if run_install "file://$work/bad" "$work/bin2" 2>/dev/null; then fail "tampered archive installed"; fi
[ ! -e "$work/bin2/wn" ] || fail "tampered archive left a binary"

# 3. Missing checksum file is refused.
make_release "$work/nosum"
rm "$work/nosum/wn-$target.tar.gz.sha256"
if run_install "file://$work/nosum" "$work/bin3" 2>/dev/null; then fail "installed without checksum"; fi
[ ! -e "$work/bin3/wn" ] || fail "missing checksum left a binary"

echo "install.sh: 3 tests passed"
