#!/usr/bin/env bash
# Launch acceptance checks for `wn`. Every check prints PASS/FAIL/SKIP; exit 1 if any FAIL.
#
#   scripts/acceptance.sh              # all local checks (needs `wn` on PATH and the default model)
#   scripts/acceptance.sh abstain lock # only these groups
#
# Groups: abstain, guard, lock, progress, flags, bench, docs, install
# Fixture repositories are cloned once into $WN_ACCEPT_CACHE (default ~/.cache/wn-acceptance).
# Checks may be tightened, never loosened, without the integrator's agreement.
# JSON is parsed from stdout only; progress and diagnostics go to stderr.
set -uo pipefail

WN=${WN:-wn}
CACHE=${WN_ACCEPT_CACHE:-$HOME/.cache/wn-acceptance}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
FAILS=0
mkdir -p "$CACHE"

pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n      %s\n' "$1" "${2:-}"; FAILS=$((FAILS + 1)); }
skip() { printf 'SKIP  %s (%s)\n' "$1" "$2"; }
want() { [ $# -eq 0 ] && return 0; for g in "${SELECTED[@]}"; do [ "$g" = "$1" ] && return 0; done; return 1; }
SELECTED=("$@")
sel() { [ ${#SELECTED[@]} -eq 0 ] || want "$1"; }

# Pinned fixtures: gin (Go, ~1k commits used), axum (Rust).
fixture() { # name url sha
  local dir="$CACHE/$1"
  if [ ! -d "$dir/.git" ]; then
    git clone -q --filter=blob:none "$2" "$dir" >/dev/null 2>&1 || return 1
  fi
  git -C "$dir" -c advice.detachedHead=false checkout -q "$3" 2>/dev/null || return 1
  echo "$dir"
}
GIN=$(fixture gin https://github.com/gin-gonic/gin.git v1.10.0) || { echo "cannot clone gin"; exit 2; }

state_of() { python3 -c 'import json,sys; print(json.load(sys.stdin).get("state",""))' 2>/dev/null; }
nfiles_of() { python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("files",[])))' 2>/dev/null; }

"$WN" init --path "$GIN" >/dev/null 2>&1

# --- abstain: garbage and empty queries must not produce hints (README promise) -----------------
if sel abstain; then
  for q in "asdkjh qwezzz" "fix the bug" "the other one"; do
    s=$("$WN" ask "$q" --json --path "$GIN" 2>/dev/null | state_of)
    [ "$s" = "abstain" ] && pass "abstain: \"$q\"" || fail "abstain: \"$q\"" "state=$s (want abstain)"
  done
  out=$("$WN" ask "" --json --path "$GIN" 2>/dev/null); rc=$?
  n=$(echo "$out" | nfiles_of)
  if [ $rc -ne 0 ] || [ "${n:-0}" = "0" ]; then pass "abstain: empty query gives no hints"; else fail "abstain: empty query gives no hints" "rc=$rc files=$n"; fi
  s=$("$WN" ask "where is the request logger that prints status and latency" --json --path "$GIN" 2>/dev/null | state_of)
  [ "$s" = "ok" ] && pass "abstain: good query still answers" || fail "abstain: good query still answers" "state=$s"
fi

# --- guard: never silently index a non-repository or $HOME ----------------------------------------
if sel guard; then
  d=$(mktemp -d); mkdir -p "$d/a/b"; for i in $(seq 1 50); do echo "fn f$i() {}" > "$d/a/b/f$i.rs"; done
  start=$(date +%s)
  out=$(cd "$d" && WN_NO_DAEMON=1 timeout 20 "$WN" status 2>&1); rc=$?
  el=$(( $(date +%s) - start ))
  if [ $rc -ne 0 ] && [ $el -lt 10 ] && echo "$out" | grep -qi "git"; then pass "guard: non-git dir refused with a message"
  else fail "guard: non-git dir refused with a message" "rc=$rc ${el}s: $(echo "$out" | head -2)"; fi
  start=$(date +%s)
  out=$(WN_NO_DAEMON=1 timeout 20 "$WN" ask "where is the config" --path "$HOME" 2>&1); rc=$?
  el=$(( $(date +%s) - start ))
  if [ $rc -ne 0 ] && [ $el -lt 10 ]; then pass "guard: \$HOME refused quickly"; else fail "guard: \$HOME refused quickly" "rc=$rc ${el}s"; fi
  out=$(WN_NO_DAEMON=1 "$WN" ask "x" --path /nonexistent/wn 2>&1); rc=$?
  [ $rc -ne 0 ] && pass "guard: missing --path exits non-zero" || fail "guard: missing --path exits non-zero" "$out"
  rm -rf "$d"
fi

# --- lock + progress: indexing a big repo must not block other repos, and must show progress ------
big_repo() {
  local b="$CACHE/big"
  if [ ! -d "$b/.git" ]; then
    mkdir -p "$b" && git -C "$b" init -q
    for i in $(seq 1 6000); do
      mkdir -p "$b/pkg$((i % 60))"
      printf 'package pkg\n// handler %d parses request %d and writes response\nfunc Handle%d(x int) int { return x + %d }\n' "$i" "$i" "$i" "$i" > "$b/pkg$((i % 60))/h$i.go"
    done
    git -C "$b" add -A && git -C "$b" -c user.email=a@b -c user.name=a commit -qm init
  fi
  echo "$b"
}
if sel lock || sel progress; then
  BIG=$(big_repo)
  rm -rf "$BIG/.wn" 2>/dev/null
  "$WN" daemon stop >/dev/null 2>&1
  "$WN" ask "warm up" --path "$GIN" >/dev/null 2>&1
  errlog=$(mktemp)
  "$WN" init --path "$BIG" >/dev/null 2>"$errlog" &
  initpid=$!
  sleep 5
  if sel progress; then
    [ -s "$errlog" ] && pass "progress: init prints progress within 5 s" || fail "progress: init prints progress within 5 s" "stderr empty after 5 s"
  fi
  if sel lock; then
    # The init above may run in-process; also index a second big copy through the daemon,
    # which is where a global lock would block other repositories.
    BIG2="$CACHE/big2"
    [ -d "$BIG2/.git" ] || cp -R "$BIG" "$BIG2"
    rm -rf "$BIG2/.wn" 2>/dev/null
    "$WN" ask "where are handlers" --path "$BIG2" >/dev/null 2>&1 &
    daskpid=$!
    sleep 3
    start=$(python3 -c 'import time;print(time.time())')
    timeout 30 "$WN" ask "where is the request logger" --path "$GIN" >/dev/null 2>&1; rc=$?
    el=$(python3 -c "import time;print(round(time.time()-$start,2))")
    if [ $rc -eq 0 ] && python3 -c "import sys;sys.exit(0 if $el < 3 else 1)"; then pass "lock: other repo answers in ${el}s during a big init"
    else fail "lock: other repo answers in <3 s during a big init" "rc=$rc ${el}s"; fi
    ds=$("$WN" daemon status 2>&1)
    echo "$ds" | grep -qi "not running" && fail "lock: daemon status correct while busy" "$ds" || pass "lock: daemon status correct while busy"
  fi
  wait $initpid 2>/dev/null
  [ -n "${daskpid:-}" ] && wait "$daskpid" 2>/dev/null
  rm -f "$errlog"
fi

# --- flags: -k and --functions do what they say, or are rejected ---------------------------------
if sel flags; then
  out=$("$WN" ask "where are routes registered" -k 5 --json --no-abstain --path "$GIN" 2>/dev/null); rc=$?
  n=$(echo "$out" | nfiles_of)
  if [ $rc -ne 0 ] || [ "${n:-0}" -gt 3 ]; then pass "flags: -k 5 honoured or rejected"; else fail "flags: -k 5 honoured or rejected" "rc=0, got $n files silently"; fi
  out=$("$WN" ask "where are routes registered" --functions --json --no-abstain --path "$GIN" 2>/dev/null); rc=$?
  nf=$(echo "$out" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("functions",[])))' 2>/dev/null)
  if [ $rc -ne 0 ] || [ "${nf:-0}" -gt 0 ]; then pass "flags: --functions returns functions or is rejected"; else fail "flags: --functions returns functions or is rejected" "functions=[]"; fi
fi

# --- bench: quality must not regress (gin v1.10.0 baseline, hit@3 model+adapter) ----------------
if sel bench; then
  BASE=${WN_ACCEPT_BENCH_BASE:-$(cat "$ROOT/scripts/acceptance-baseline.txt" 2>/dev/null)}
  if [ -z "$BASE" ]; then skip "bench: gin hit@3" "no baseline"; else
    h=$("$WN" bench --json --commits 150 --path "$GIN" 2>/dev/null | python3 -c '
import json,sys
d=json.load(sys.stdin)
print(next((m["hit3"] for m in d.get("matched",[]) if m.get("method")=="model + adapter"),""))')
    if [ -n "$h" ] && python3 -c "import sys;sys.exit(0 if $h >= $BASE - 0.01 else 1)"; then pass "bench: gin hit@3 $h (baseline $BASE)"
    else fail "bench: gin hit@3 within 1 pt of $BASE" "got '$h'"; fi
  fi
fi

# --- docs: README matches reality -----------------------------------------------------------------
if sel docs; then
  R="$ROOT/README.md"
  grep -q "hash-bow2" "$R" && fail "docs: no stale model names in README" "hash-bow2 present" || pass "docs: no stale model names in README"
  grep -qE "\(LAUNCH\.md\)" "$R" && fail "docs: LAUNCH.md not linked from README" || pass "docs: LAUNCH.md not linked from README"
  bad=0
  for l in $(grep -oE '\]\([^)#]+' "$ROOT"/README.md "$ROOT"/docs/*.md | sed -E 's/.*\]\(//' | grep -vE '^(https?|mailto):' | sort -u); do
    [ -e "$ROOT/$l" ] || [ -e "$ROOT/docs/$l" ] || { echo "      missing: $l"; bad=1; }
  done
  [ $bad -eq 0 ] && pass "docs: relative links resolve" || fail "docs: relative links resolve"
  for u in $(grep -ohE 'https://github.com/andreylukin/where-next/[^) >"]+' "$ROOT"/README.md | sort -u); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -L "$u")
    [ "$code" = "200" ] || { fail "docs: link $u" "HTTP $code"; }
  done
  if grep -qiE "pays? off" "$ROOT/skills/where-next/SKILL.md" "$ROOT"/crates/wn-mcp/src/*.rs \
    || grep -qE "^- Starting a task.*--start" "$ROOT/skills/where-next/SKILL.md"; then
    fail "docs: SKILL.md and MCP treat --start as opt-in" "start hint still recommended by default"
  else pass "docs: SKILL.md and MCP treat --start as opt-in"; fi
fi

# --- install: binary path works on supported Linux, clear refusal on old glibc ------------------
if sel install; then
  if [ -x "$ROOT/packaging/test-install-docker.sh" ] && command -v docker >/dev/null; then
    "$ROOT/packaging/test-install-docker.sh" && pass "install: docker matrix" || fail "install: docker matrix" "see output above"
  else skip "install: docker matrix" "packaging/test-install-docker.sh not present (lane L3 adds it)"; fi
fi

echo
[ $FAILS -eq 0 ] && { echo "ALL CHECKS PASSED"; exit 0; } || { echo "$FAILS FAILED"; exit 1; }
