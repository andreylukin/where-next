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
  # Build on bullseye so the binary supports Ubuntu 22.04 and Debian 12.
  tar -C "$root" --exclude=target --exclude=.git --exclude=dist -cf "$work/source.tar" .
  container="$(docker create --platform linux/amd64 -e RUSTUP_TOOLCHAIN=1.90.0 rust:1.90-bullseye sh -c 'mkdir /work && tar -xf /source.tar -C /work && cd /work && nice cargo build --release --locked -p where-next --target x86_64-unknown-linux-gnu && packaging/package.sh x86_64-unknown-linux-gnu test')"
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
for image in debian:12 ubuntu:22.04; do
  echo "testing $image"
  # shellcheck disable=SC2016
  command='apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends curl >/dev/null && WN_RELEASE_BASE=file:///release WN_NO_MODEL=1 sh /install.sh && "$HOME/.local/bin/wn" --version && test -f "$HOME/.local/bin/libonnxruntime.so"'
  container="$(docker create --platform linux/amd64 "$image" sh -c "$command")"
  docker cp "$root/install.sh" "$container:/install.sh"
  docker cp "$work" "$container:/release"
  docker start -a "$container" >"$work/out" 2>&1 || true
  code="$(docker inspect -f '{{.State.ExitCode}}' "$container")"
  docker rm "$container" >/dev/null; container=""
  if [ "$code" != 0 ]; then cat "$work/out" >&2; exit 1; fi
  grep -q '^wn ' "$work/out" || { cat "$work/out" >&2; exit 1; }
  echo "PASS $image"
done

echo "testing debian:11 refusal"
container="$(docker create --platform linux/amd64 debian:11 sh /install.sh)"
docker cp "$root/install.sh" "$container:/install.sh"
docker start -a "$container" >"$work/out" 2>&1 || true
code="$(docker inspect -f '{{.State.ExitCode}}' "$container")"
docker rm "$container" >/dev/null; container=""
if [ "$code" = 0 ] || ! grep -q 'detected glibc version: 2.31' "$work/out" ||
   ! grep -q 'prebuilt binaries need glibc >= 2.35 (Ubuntu 22.04+, Debian 12+)' "$work/out" ||
   grep -q 'downloading\|cloning' "$work/out"; then
  cat "$work/out" >&2
  exit 1
fi
echo "PASS debian:11 refusal"
