#!/usr/bin/env bash
# Tests install.sh.
# Release mode (WN_FROM=release), against a local fixture release (file:// URLs): a good archive
# installs, a tampered archive or a missing checksum file is refused and installs nothing.
# Source mode (the default), against a local upstream repository and a fake cargo: first run
# installs, a rerun is a no-op, a new upstream commit updates, --ref pins, --dry-run changes
# nothing, a missing cargo is refused without --yes, --uninstall removes the binary and clone.
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
  WN_FROM=release WN_DOWNLOAD_BASE="$1" WN_INSTALL_DIR="$2" WN_TARGET="$target" sh "$root/install.sh"
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

echo "install.sh: 3 release-mode tests passed"

# ---- source mode ----
g() { git -c user.name=t -c user.email=t@example.com -c commit.gpgsign=false "$@"; }
up="$work/upstream"
mkdir -p "$up/crates/wn-cli"
g -C "$up" init -q -b main
printf '[package]\nname = "where-next"\n' > "$up/crates/wn-cli/Cargo.toml"
g -C "$up" add -A && g -C "$up" commit -q -m first
first="$(git -C "$up" rev-parse HEAD)"

fake="$work/fakebin"
mkdir -p "$fake"
cat > "$fake/cargo" <<'CARGO'
#!/bin/sh
echo "$@" >> "$FAKE_CARGO_LOG"
case "$1" in
  uninstall) rm -f "$FAKE_ROOT/bin/wn"; exit 0 ;;
esac
path=""; root=""
while [ $# -gt 0 ]; do case "$1" in --path) path="$2"; shift ;; --root) root="$2"; shift ;; esac; shift; done
sha="$(git -C "$path" rev-parse --short=7 HEAD)"
mkdir -p "$root/bin"
printf '#!/bin/sh\necho "wn 0.0.1 (%s 2026-01-01)"\n' "$sha" > "$root/bin/wn"
chmod +x "$root/bin/wn"
CARGO
chmod +x "$fake/cargo"

export FAKE_CARGO_LOG="$work/cargo.log" FAKE_ROOT="$work/wnroot"
src_install() { # extra args...
  HOME="$work/home" PATH="$fake:$PATH" WN_HOME="$work/wnhome" WN_BIN_ROOT="$FAKE_ROOT" \
    WN_REPO_URL="$up" sh "$root/install.sh" "$@"
}
calls() { if [ -f "$FAKE_CARGO_LOG" ]; then wc -l < "$FAKE_CARGO_LOG" | tr -d ' '; else echo 0; fi; }
short() { printf '%s' "$1" | cut -c1-7; }

# 4. --dry-run changes nothing.
src_install --dry-run 2>/dev/null
[ ! -e "$work/wnhome/src" ] || fail "dry run cloned"
[ "$(calls)" = 0 ] || fail "dry run built"

# 5. First run clones and installs.
src_install 2>/dev/null
"$FAKE_ROOT/bin/wn" | grep -q "$(short "$first")" || fail "first install did not build $first"
[ -d "$work/wnhome/src/.git" ] || fail "no private clone"

# 6. Rerun without upstream changes does not rebuild.
src_install 2>"$work/rerun.err"
[ "$(calls)" = 1 ] || fail "rerun rebuilt ($(calls) cargo calls)"
grep -q "up to date" "$work/rerun.err" || fail "rerun did not say up to date"

# 7. A new upstream commit updates, reporting old -> new.
echo change > "$up/change.txt"
g -C "$up" add -A && g -C "$up" commit -q -m second
second="$(git -C "$up" rev-parse HEAD)"
src_install 2>"$work/update.err"
"$FAKE_ROOT/bin/wn" | grep -q "$(short "$second")" || fail "update did not build $second"
grep -q "$(short "$first") -> $(short "$second")" "$work/update.err" || fail "update did not report old -> new"

# 8. --ref pins a commit.
src_install --ref "$first" 2>/dev/null
"$FAKE_ROOT/bin/wn" | grep -q "$(short "$first")" || fail "--ref did not pin $first"

# 9. Missing cargo is refused without --yes (and nothing is built).
before="$(calls)"
if HOME="$work/home2" PATH="/usr/bin:/bin" WN_HOME="$work/wnhome2" WN_BIN_ROOT="$work/root2" \
  WN_REPO_URL="$up" CARGO_HOME="" sh "$root/install.sh" 2>"$work/nocargo.err" </dev/null; then
  fail "installed without cargo"
fi
grep -q "rustup" "$work/nocargo.err" || fail "missing cargo did not print the rustup command"
[ "$(calls)" = "$before" ] || fail "built without cargo"

# 10. --uninstall removes the binary and the clone.
src_install --uninstall 2>/dev/null
[ ! -e "$FAKE_ROOT/bin/wn" ] || fail "uninstall left the binary"
[ ! -e "$work/wnhome/src" ] || fail "uninstall left the clone"

echo "install.sh: 7 source-mode tests passed"
