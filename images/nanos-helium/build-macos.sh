#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
tree=$root/.work/runtime/nanos-arm64/hydrogen
work=$root/.work/images/nanos-helium-arm64
rootfs=$work/rootfs
images=$root/.work/images

fail() { echo "images/nanos-helium/build-macos.sh: $*" >&2; exit 1; }

[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || fail "Apple Silicon macOS is required"
[ -x "$tree/nanos/output/tools/bin/mkfs" ] && [ -s "$tree/nanos/output/platform/virt/bin/kernel.img" ] ||
    fail "run runtime/build-nanos-arm64.sh first"
[ -n "${BCTL_BROWSER_ARCHIVE:-}" ] || fail "set BCTL_BROWSER_ARCHIVE to the arm64 Helium archive directory"
sh "$root/images/build.sh" helium || fail "cannot build the Helium rootfs"
rm -rf "$rootfs"
mkdir -p "$rootfs" "$images"
container=$(docker create bctl/helium:latest) || fail "cannot create the Helium container"
trap 'docker rm "$container" >/dev/null 2>&1 || :' EXIT
docker export "$container" | tar -xf - -C "$rootfs" || fail "cannot extract the Helium rootfs"
docker rm "$container" >/dev/null || fail "cannot remove the Helium container"
trap - EXIT
docker run --rm --platform linux/arm64 -v "$tree:/src" -v "$rootfs:/rootfs:ro" -v "$images:/images" -w /src \
    -e TMPDIR=/src/.work/tmp bctl/nanos-arm64-builder sh dev/image.sh \
    -o /images/nanos-helium.img --size 2g --klib tmpfs --klib shmem --root /rootfs --tree /rootfs:/ \
    --dir /profile --dir /tmp --dir /dev/shm --arg run-headless /rootfs/usr/local/bin/bctl-guest >/dev/null || fail "cannot pack the ARM Nanos image"
echo "$images/nanos-helium.img"
