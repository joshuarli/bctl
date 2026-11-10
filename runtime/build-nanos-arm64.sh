#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
hydrogen=${HYDROGEN:-$root/../hydrogen}
revision=05a4c9878f16e02e4ec4206fbe992bf2f9331ff0
work=$root/.work/runtime/nanos-arm64
tree=$work/hydrogen

fail() { echo "runtime/build-nanos-arm64.sh: $*" >&2; exit 1; }

[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || fail "Apple Silicon macOS is required"
git -C "$hydrogen" cat-file -e "$revision^{commit}" 2>/dev/null || fail "hydrogen $revision is missing from $hydrogen"
docker build --platform linux/arm64 -q -t bctl/nanos-arm64-builder -f "$root/runtime/nanos-arm64.Dockerfile" "$root/runtime" >/dev/null || fail "cannot build the Nanos compiler image"
rm -rf "$tree"
mkdir -p "$tree"
git -C "$hydrogen" archive "$revision" | tar -xf - -C "$tree"
mkdir -p "$tree/.work/tmp"
docker run --rm --platform linux/arm64 -v "$tree:/src" -w /src -e TMPDIR=/src/.work/tmp \
    bctl/nanos-arm64-builder sh dev/build.sh || fail "cannot build the ARM Nanos kernel"
echo "$tree"
