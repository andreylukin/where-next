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

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

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
