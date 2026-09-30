#!/usr/bin/env bash
# Package the built `wn` binary for one target into dist/, with a per-archive .sha256 file.
#
#   packaging/package.sh <target> [version]
#
# Archives are named wn-<target>.tar.gz (wn-<target>.zip on Windows) so the install script can
# find them without knowing the version. Each contains wn, LICENSE, NOTICE and README.md.
set -euo pipefail

target="${1:?usage: package.sh <target> [version]}"
version="${2:-dev}"
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="wn"
case "$target" in *windows*) bin="wn.exe" ;; esac

src="$root/target/$target/release/$bin"
[ -f "$src" ] || { echo "missing $src (build first)" >&2; exit 1; }

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
name="wn-$target"
mkdir -p "$stage/$name" "$root/dist"
cp "$src" "$root/LICENSE" "$root/NOTICE" "$root/README.md" "$stage/$name/"
printf '%s\n' "$version" > "$stage/$name/VERSION"

case "$target" in
  x86_64-unknown-linux-gnu) ort_arch=x64; ort_sha=a3e1b79d7bb1bf09696ce675f49e4064e6c81f6202b8225624fff0e93f8d6407 ;;
  aarch64-unknown-linux-gnu) ort_arch=aarch64; ort_sha=e15ff8b5d85afe6c144d97c6fd432254bf76a219daaf17658087d6ecb3e8f0bb ;;
esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

if [ -n "${ort_arch:-}" ]; then
  # ort 2.0.0-rc.13 uses ORT API 27, supplied by Microsoft's 1.28.0 release.
  ort_file="onnxruntime-linux-$ort_arch-1.28.0.tgz"
  ort_tar="${WN_ORT_CACHE:-$stage}/$ort_file"
  if [ ! -f "$ort_tar" ]; then
    curl -fL --retry 3 "https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/$ort_file" -o "$ort_tar"
  fi
  [ "$(sha256 "$ort_tar" | cut -d' ' -f1)" = "$ort_sha" ] || { echo "ORT checksum mismatch: $ort_file" >&2; exit 1; }
  ort_dir="onnxruntime-linux-$ort_arch-1.28.0"
  tar -xzf "$ort_tar" -C "$stage" "$ort_dir/lib/libonnxruntime.so.1.28.0" "$ort_dir/LICENSE" "$ort_dir/ThirdPartyNotices.txt"
  cp "$stage/$ort_dir/lib/libonnxruntime.so.1.28.0" "$stage/$name/libonnxruntime.so"
  cp "$stage/$ort_dir/LICENSE" "$stage/$name/ONNXRUNTIME-LICENSE"
  cp "$stage/$ort_dir/ThirdPartyNotices.txt" "$stage/$name/ONNXRUNTIME-ThirdPartyNotices.txt"
fi

cd "$stage"
case "$target" in
  *windows*)
    archive="$name.zip"
    if command -v 7z >/dev/null 2>&1; then 7z a -tzip "$archive" "$name" >/dev/null
    else powershell -NoProfile -Command "Compress-Archive -Path '$name' -DestinationPath '$archive'"; fi
    ;;
  *)
    archive="$name.tar.gz"
    tar -czf "$archive" "$name"
    ;;
esac
mv "$archive" "$root/dist/"
cd "$root/dist"
sha256 "$archive" > "$archive.sha256"
echo "packaged dist/$archive"
cat "$archive.sha256"
