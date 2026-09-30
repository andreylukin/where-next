#!/usr/bin/env bash
# Tests install.sh.
# Release mode (the default), against a local fixture release (file:// URLs): a good archive
# installs, a tampered archive or a missing checksum file is refused and installs nothing.
# Source mode (WN_FROM=source), against a local upstream repository and a fake cargo: first run
# installs, a rerun is a no-op, a new upstream commit updates, --ref pins, --dry-run changes
# nothing, a missing cargo is refused without --yes, --uninstall removes the binary and clone.
# Model step: --yes pulls the default model once, a rerun keeps a current model, a moved source is
# pulled again, --no-model never downloads.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
target="x86_64-unknown-linux-gnu"

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi; }

make_release() { # dir
  local dir="$1"
  mkdir -p "$dir/stage/wn-$target"
  # shellcheck disable=SC2016
  printf '#!/bin/sh\n[ "$1 $2 $3" = "model pull --check" ] && exit 10\necho wn-fixture\n' > "$dir/stage/wn-$target/wn"
  chmod +x "$dir/stage/wn-$target/wn"
  printf 'fixture\n' > "$dir/stage/wn-$target/libonnxruntime.so"
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
[ -f "$work/bin1/libonnxruntime.so" ] || fail "installed runtime missing"
[ "$(cat "$work/bin1/libonnxruntime.so")" = fixture ] || fail "installed runtime differs from archive"
cat > "$work/bin1/wn" <<'OLD_WN'
#!/bin/sh
[ "$1 $2" = 'daemon stop' ] && printf 'stopped\n' > "$WN_DAEMON_LOG"
OLD_WN
chmod +x "$work/bin1/wn"
WN_DAEMON_LOG="$work/daemon.log" run_install "file://$work/good" "$work/bin1" 2>/dev/null
[ "$(cat "$work/daemon.log")" = stopped ] || fail "release reinstall did not stop daemon"

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

# A Linux archive without its shared library must be refused.
make_release "$work/nort"
rm "$work/nort/stage/wn-$target/libonnxruntime.so"
tar -czf "$work/nort/wn-$target.tar.gz" -C "$work/nort/stage" "wn-$target"
(cd "$work/nort" && sha256 "wn-$target.tar.gz" > "wn-$target.tar.gz.sha256")
if run_install "file://$work/nort" "$work/bin4" 2>"$work/nort.err"; then fail "installed without runtime"; fi
grep -q 'archive does not contain libonnxruntime.so' "$work/nort.err" || fail "missing runtime error not reported"
[ ! -e "$work/bin4/wn" ] || fail "missing runtime left a binary"

# A destination that cannot contain files must never report success.
mkdir "$work/blocked-bin"
chmod 0500 "$work/blocked-bin"
if [ -w "$work/blocked-bin" ]; then
  # root bypasses directory permissions; a regular file is still unusable as a directory.
  blocked_dir="$work/blocked-file"
  printf 'occupied\n' > "$blocked_dir"
else
  blocked_dir="$work/blocked-bin"
fi
if run_install "file://$work/good" "$blocked_dir" 2>"$work/blocked.err"; then
  fail "unwritable install destination succeeded"
fi
! grep -q 'installed ' "$work/blocked.err" || fail "failed install reported success"
mkdir -p "$work/marker-bin/wn.install-method"
if run_install "file://$work/good" "$work/marker-bin" 2>"$work/marker.err"; then
  fail "unwritable install marker succeeded"
fi
! grep -q 'installed ' "$work/marker.err" || fail "failed marker write reported success"

echo "install.sh: 7 release-mode tests passed"

# A server error must fail rather than entering source mode.
mkdir -p "$work/http-fake"
cat > "$work/http-fake/curl" <<'CURL'
#!/bin/sh
printf '503'
CURL
chmod +x "$work/http-fake/curl"
if PATH="$work/http-fake:$PATH" WN_FROM=auto WN_GLIBC=2.39 WN_NO_MODEL=1 \
  WN_RELEASE_BASE=https://example.invalid WN_TARGET="$target" \
  sh "$root/install.sh" 2>"$work/http.err"; then fail "HTTP 503 succeeded"; fi
grep -q 'HTTP 503' "$work/http.err" || fail "HTTP 503 was not reported"
! grep -q 'falling back\|cloning\|cargo' "$work/http.err" || fail "HTTP 503 tried source mode"
cat > "$work/http-fake/curl" <<'CURL'
#!/bin/sh
printf '000'
exit 6
CURL
if PATH="$work/http-fake:$PATH" WN_FROM=auto WN_GLIBC=2.39 WN_NO_MODEL=1 \
  WN_RELEASE_BASE=https://example.invalid WN_TARGET="$target" \
  sh "$root/install.sh" 2>"$work/dns.err"; then fail "network failure succeeded"; fi
grep -q 'release download failed' "$work/dns.err" || fail "network failure was not reported"
! grep -q 'falling back' "$work/dns.err" || fail "network failure tried source mode"
echo "install.sh: 2 release transport tests passed"

# ---- source mode ----
g() { git -c user.name=t -c user.email=t@example.com -c commit.gpgsign=false -c tag.gpgsign=false "$@"; }
up="$work/upstream"
mkdir -p "$up/crates/wn-cli"
g -C "$up" init -q -b main
printf '[package]\nname = "where-next"\n' > "$up/crates/wn-cli/Cargo.toml"
g -C "$up" add -A && g -C "$up" commit -q -m first
g -C "$up" tag v0.0.1
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
printf '#!/bin/sh\nWN_SHA=%s exec "$FAKE_WN" "$@"\n' "$sha" > "$root/bin/wn"
chmod +x "$root/bin/wn"
CARGO
chmod +x "$fake/cargo"

# Fake wn: prints its version; `model pull [--check] [--source S]` records/compares the source.
cat > "$fake/wn-impl" <<'WN'
#!/bin/sh
if [ "${1:-} ${2:-}" = "model pull" ]; then
  shift 2; check=""; src="pinned"
  while [ $# -gt 0 ]; do case "$1" in --check) check=1 ;; --source) src="$2"; shift ;; esac; shift; done
  m="$FAKE_ROOT/model-source"
  if [ -n "$check" ]; then
    if [ -f "$m" ] && [ "$(cat "$m")" = "$src" ]; then exit 0; fi
    exit 10
  fi
  echo "$src" > "$m"; echo "$src" >> "$FAKE_MODEL_LOG"; echo "installed gemma-xl1"; exit 0
fi
case "${1:-}" in daemon | skill) exit 0 ;; esac
echo "wn 0.0.1 ($WN_SHA 2026-01-01)"
WN
chmod +x "$fake/wn-impl"

export FAKE_CARGO_LOG="$work/cargo.log" FAKE_ROOT="$work/wnroot" FAKE_WN="$fake/wn-impl" \
  FAKE_MODEL_LOG="$work/model.log"
src_install() { # extra args... (no model unless a test asks: the prompt would read /dev/tty)
  HOME="$work/home" PATH="$fake:$PATH" WN_HOME="$work/wnhome" WN_BIN_ROOT="$FAKE_ROOT" \
    WN_REPO_URL="$up" WN_FROM=source WN_NO_MODEL="${WN_NO_MODEL-1}" sh "$root/install.sh" --ref main "$@"
}
pulls() { if [ -f "$FAKE_MODEL_LOG" ]; then wc -l < "$FAKE_MODEL_LOG" | tr -d ' '; else echo 0; fi; }
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
  WN_REPO_URL="$up" WN_FROM=source CARGO_HOME="" sh "$root/install.sh" 2>"$work/nocargo.err" </dev/null; then
  fail "installed without cargo"
fi
grep -q "rustup" "$work/nocargo.err" || fail "missing cargo did not print the rustup command"
[ "$(calls)" = "$before" ] || fail "built without cargo"

# 10. With --yes the default model is pulled (from WN_MODEL_SOURCE here: no network in tests).
models="$work/models-fixture"; mkdir -p "$models"
WN_NO_MODEL="" WN_MODEL_SOURCE="$models" src_install --yes 2>"$work/model.err"
[ "$(pulls)" = 1 ] || fail "model was not pulled ($(pulls) pulls)"
grep -q "Gemma Terms of Use" "$work/model.err" || fail "model prompt did not mention the Gemma Terms"

# 11. Rerun with the model current: no second download.
WN_NO_MODEL="" WN_MODEL_SOURCE="$models" src_install --yes 2>"$work/model2.err"
[ "$(pulls)" = 1 ] || fail "current model was downloaded again"
grep -q "installed and current" "$work/model2.err" || fail "rerun did not report the model current"

# 12. The model source moved (a new pin): the rerun pulls it, even with wn itself up to date.
WN_NO_MODEL="" WN_MODEL_SOURCE="$models-v2" src_install --yes 2>/dev/null
[ "$(pulls)" = 2 ] || fail "moved model source was not pulled"

# 13. --no-model never downloads.
rm -f "$FAKE_ROOT/model-source"
WN_NO_MODEL="" WN_MODEL_SOURCE="$models" src_install --yes --no-model 2>"$work/nomodel.err"
[ "$(pulls)" = 2 ] || fail "--no-model downloaded"
grep -q "wn model pull" "$work/nomodel.err" || fail "--no-model did not say how to install later"

# 14. --uninstall removes the binary and the clone.
src_install --uninstall 2>/dev/null
[ ! -e "$FAKE_ROOT/bin/wn" ] || fail "uninstall left the binary"
[ ! -e "$work/wnhome/src" ] || fail "uninstall left the clone"

echo "install.sh: 11 source-mode tests passed"

# Default path selects release; a missing archive selects source without changing the checksum policy.
HOME="$work/auto-home" WN_RELEASE_BASE="file://$work/good" WN_INSTALL_DIR="$work/auto-bin" WN_TARGET="$target" WN_NO_MODEL=1 sh "$root/install.sh" 2>"$work/auto.err"
[ "$("$work/auto-bin/wn")" = "wn-fixture" ] || fail "default did not install the release"
[ -f "$work/auto-bin/wn.install-method" ] || fail "release install marker missing"
grep -q 'wn model pull' "$work/auto.err" || fail "model next step missing"
HOME="$work/fallback-home" PATH="$fake:$PATH" WN_GLIBC=2.35 WN_RELEASE_BASE="file://$work/missing" WN_INSTALL_DIR="$work/fallback-bin" WN_TARGET="$target" WN_HOME="$work/fallback-wnhome" WN_BIN_ROOT="$work/fallback-root" WN_REPO_URL="$up" WN_NO_MODEL=1 sh "$root/install.sh" 2>"$work/fallback.err"
[ -x "$work/fallback-root/bin/wn" ] || fail "source fallback did not build"

# Old or unknown glibc is refused before any release download or source clone.
for libc in 2.34 unknown; do
  if HOME="$work/$libc-home" WN_GLIBC="$libc" WN_RELEASE_BASE="file://$work/good" WN_TARGET="$target" sh "$root/install.sh" 2>"$work/$libc.err"; then
    fail "glibc $libc was accepted"
  fi
  grep -q "detected glibc version: $libc" "$work/$libc.err" || fail "glibc $libc detection missing"
  grep -q 'prebuilt binaries need glibc >= 2.35 (Ubuntu 22.04+, Debian 12+)' "$work/$libc.err" || fail "glibc $libc requirement missing"
  grep -q 'source build needs a compatible ONNX Runtime shared library' "$work/$libc.err" || fail "glibc $libc source warning missing"
  grep -q 'ubuntu:22.04' "$work/$libc.err" || fail "glibc $libc container option missing"
  grep -q 'WN_FROM=source' "$work/$libc.err" || fail "glibc $libc override missing"
  ! grep -q 'downloading\|cloning' "$work/$libc.err" || fail "glibc $libc attempted a download"
done

# Explicit source mode remains available on old glibc.
HOME="$work/old-source-home" PATH="$fake:$PATH" WN_GLIBC=2.34 WN_FROM=source WN_HOME="$work/old-source-wnhome" WN_BIN_ROOT="$work/old-source-root" WN_REPO_URL="$up" WN_NO_MODEL=1 sh "$root/install.sh" 2>"$work/old-source.err"
[ -x "$work/old-source-root/bin/wn" ] || fail "explicit source override did not build"

# Uninstall removes a release install and explains retained skill files.
HOME="$work/auto-home" WN_INSTALL_DIR="$work/auto-bin" sh "$root/install.sh" --uninstall 2>"$work/release-uninstall.err"
[ ! -e "$work/auto-bin/wn" ] || fail "release binary remained after uninstall"
[ ! -e "$work/auto-bin/libonnxruntime.so" ] || fail "release runtime remained after uninstall"
[ ! -e "$work/auto-bin/wn.install-method" ] || fail "release marker remained after uninstall"
grep -q 'wn skill sync --uninstall' "$work/release-uninstall.err" || fail "uninstall omitted agent skills"
echo "install.sh: 6 additional auto/release tests passed"

# A non-TTY release install that declines the model includes a pull command in next steps.
HOME="$work/non-tty-home" WN_RELEASE_BASE="file://$work/good" WN_INSTALL_DIR="$work/non-tty-bin" WN_TARGET="$target" WN_NO_MODEL="" sh "$root/install.sh" </dev/null 2>"$work/non-tty.err"
grep -A5 'next steps:' "$work/non-tty.err" | grep -q 'wn model pull' || fail "non-TTY next steps omitted model pull"
echo "install.sh: non-TTY model next step passed"

# Explicit source mode without --ref uses the latest release tag, not the moving main branch.
g -C "$up" tag v99.0.0-rc1 "$second"
g -C "$up" tag v0.0.2 "$first"
g -C "$up" tag v0.0.10 "$first"
HOME="$work/tag-home" PATH="$fake:$PATH" WN_FROM=source WN_HOME="$work/tag-wnhome" WN_BIN_ROOT="$work/tag-root" WN_REPO_URL="$up" WN_NO_MODEL=1 sh "$root/install.sh" 2>"$work/tag.err"
"$work/tag-root/bin/wn" | grep -q "$(short "$first")" || fail "source default was not pinned to release tag"
grep -q 'v0.0.10' "$work/tag.err" || fail "source did not select the highest stable tag"
echo "install.sh: source tag pin passed"

# Rosetta selects the Apple Silicon archive; native Intel Macs fail before source setup.
mkdir -p "$work/mac-fake"
cat > "$work/mac-fake/uname" <<'UNAME'
#!/bin/sh
case "$1" in -s) echo Darwin ;; -m) echo x86_64 ;; esac
UNAME
cat > "$work/mac-fake/sysctl" <<'SYSCTL'
#!/bin/sh
echo "${FAKE_TRANSLATED:-0}"
SYSCTL
chmod +x "$work/mac-fake/uname" "$work/mac-fake/sysctl"
FAKE_TRANSLATED=1 PATH="$work/mac-fake:$PATH" WN_FROM=release sh "$root/install.sh" --dry-run 2>"$work/rosetta.err"
grep -q 'aarch64-apple-darwin' "$work/rosetta.err" || fail "Rosetta target was not aarch64"
if FAKE_TRANSLATED=0 PATH="$work/mac-fake:$PATH" WN_FROM=source sh "$root/install.sh" --dry-run 2>"$work/intel.err"; then fail "Intel Mac source build offered"; fi
grep -q "Intel Macs aren't supported yet" "$work/intel.err" || fail "Intel Mac error missing"
! grep -q 'rustup\|cloning' "$work/intel.err" || fail "Intel Mac attempted source setup"
