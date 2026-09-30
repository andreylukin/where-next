#!/usr/bin/env bash
# Smoke-test the release installer in supported and older glibc containers.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
target="x86_64-unknown-linux-gnu"
work="$(mktemp -d)"
container=""
cleanup() { [ -z "$container" ] || docker rm -f "$container" >/dev/null 2>&1 || true; rm -rf "$work"; }
trap cleanup EXIT
if [ -n "${WN_TEST_TARBALL:-}" ]; then
  cp "$WN_TEST_TARBALL" "$work/wn-$target.tar.gz"
else
  # Use Ubuntu 24.04 with rustup: its glibc and GCC match the release runner.
  tar -C "$root" --exclude=target --exclude=.git --exclude=dist -cf "$work/source.tar" .
  container="$(docker create --platform linux/amd64 ubuntu:24.04 sh -c 'mkdir /work && tar -xf /source.tar -C /work && cd /work && apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates build-essential pkg-config git libssl-dev >/dev/null && curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path >/dev/null && . "$HOME/.cargo/env" && nice cargo build --release --locked -p where-next --target x86_64-unknown-linux-gnu && packaging/package.sh x86_64-unknown-linux-gnu test')"
  docker cp "$work/source.tar" "$container:/source.tar"
  docker start -a "$container"
  [ "$(docker inspect -f '{{.State.ExitCode}}' "$container")" = 0 ]
  docker cp "$container:/work/dist/wn-$target.tar.gz" "$work/"
  docker rm "$container" >/dev/null; container=""
fi
if command -v sha256sum >/dev/null; then
  (cd "$work" && sha256sum "wn-$target.tar.gz" > "wn-$target.tar.gz.sha256")
else
  (cd "$work" && shasum -a 256 "wn-$target.tar.gz" > "wn-$target.tar.gz.sha256")
fi
for image in ubuntu:24.04 debian:12 ubuntu:22.04; do
  echo "testing $image"
  if [ "$image" = ubuntu:24.04 ]; then
    # shellcheck disable=SC2016
    command='apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates >/dev/null && WN_RELEASE_BASE=file:///release WN_NO_MODEL=1 sh /install.sh && "$HOME/.local/bin/wn" --version'
  else
    command='WN_RELEASE_BASE=file:///release WN_NO_MODEL=1 sh /install.sh'
  fi
  container="$(docker create --platform linux/amd64 "$image" sh -c "$command")"
  docker cp "$root/install.sh" "$container:/install.sh"
  docker cp "$work" "$container:/release"
  docker start -a "$container" >"$work/out" 2>&1 || true
  code="$(docker inspect -f '{{.State.ExitCode}}' "$container")"
  docker rm "$container" >/dev/null; container=""
  if [ "$image" = ubuntu:24.04 ]; then
    if [ "$code" != 0 ]; then cat "$work/out" >&2; exit 1; fi
    grep -q '^wn ' "$work/out" || { cat "$work/out" >&2; exit 1; }
  else
    if [ "$code" = 0 ]; then cat "$work/out" >&2; exit 1; fi
    case "$image" in debian:12) libc=2.36 ;; ubuntu:22.04) libc=2.35 ;; esac
    grep -q "detected glibc version: $libc" "$work/out" || { cat "$work/out" >&2; exit 1; }
    grep -q 'prebuilt binaries need glibc >= 2.39 (Ubuntu 24.04+, Debian 13+)' "$work/out" || { cat "$work/out" >&2; exit 1; }
    grep -q 'source build will also fail on this system' "$work/out" || { cat "$work/out" >&2; exit 1; }
    grep -q 'ubuntu:24.04' "$work/out" || { cat "$work/out" >&2; exit 1; }
    grep -q 'WN_FROM=source' "$work/out" || { cat "$work/out" >&2; exit 1; }
    ! grep -q 'downloading\|cloning' "$work/out" || { cat "$work/out" >&2; exit 1; }
  fi
  echo "PASS $image"
done
