#!/bin/sh
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
tree=$root/.work/runtime/nanos-arm64/hydrogen
rootfs=$root/.work/images/nanos-helium-arm64/rootfs
work=$root/.work/tmp/nanos-full-disk
mkdir -p "$work"
docker run --rm --platform linux/arm64 -v "$root/tests/runtime-nanos:/tests:ro" -v "$work:/scratch" \
    bctl/nanos-arm64-builder gcc -O2 -Wall -Wextra -Werror -static /tests/full-disk.c -o /scratch/full-disk
docker run --rm --platform linux/arm64 -v "$tree:/src" -v "$rootfs:/rootfs:ro" -v "$work:/scratch" -w /src \
    -e TMPDIR=/src/.work/tmp bctl/nanos-arm64-builder sh dev/image.sh \
    -o /scratch/full-disk.img --size 2g --klib tmpfs --klib shmem --root /rootfs --tree /rootfs:/ \
    --dir /profile --dir /tmp --dir /dev/shm /scratch/full-disk >/dev/null
printf '%s\n' "$work/full-disk.img"
