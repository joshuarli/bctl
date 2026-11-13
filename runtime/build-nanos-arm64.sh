#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
work=$root/.work/runtime/nanos-arm64
tree=$work/hydrogen

fail() { echo "runtime/build-nanos-arm64.sh: $*" >&2; exit 1; }

[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || fail "Apple Silicon macOS is required"
[ -z "$(git -C "$root" status --porcelain -- kernel)" ] || fail "kernel/ has uncommitted changes; this builds the committed kernel"
kernel_tree=$(git -C "$root" rev-parse HEAD:kernel) || fail "cannot read the kernel tree"
docker build --platform linux/arm64 -q -t bctl/nanos-arm64-builder -f "$root/runtime/nanos-arm64.Dockerfile" "$root/runtime" >/dev/null || fail "cannot build the Nanos compiler image"
rm -rf "$tree"
mkdir -p "$tree"
git -C "$root" archive "$kernel_tree" | tar -xf - -C "$tree"
printf '%s\n' "$kernel_tree" >"$tree/COMMIT"
mkdir -p "$tree/.work/tmp"
docker run --rm --platform linux/arm64 -v "$tree:/src" -w /src -e TMPDIR=/src/.work/tmp ${KERNEL_TEST_CFLAGS:+-e KERNEL_TEST_CFLAGS} \
    bctl/nanos-arm64-builder sh dev/build.sh || fail "cannot build the ARM Nanos kernel"
echo "$tree"
