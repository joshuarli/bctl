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
docker run --rm --platform linux/arm64 -v "$tree:/src" -v "$rootfs:/rootfs:ro" -v "$work:/scratch" -w /src \
    bctl/nanos-arm64-builder sh -c '
        mkfs=nanos/output/tools/bin/mkfs
        "$mkfs" -s 2g -k nanos/output/platform/virt/bin/kernel.img -b nanos/output/platform/virt/boot-stub.img \
            -u nanos/output/platform/virt/boot/bootaa64.efi -t '\''(mounts:(profile:"/profile"))'\'' \
            /scratch/full-disk.img.new < /scratch/full-disk.img.manifest &&
        rm -f /scratch/full-disk.profile.img &&
        "$mkfs" -e -l profile -s 256m /scratch/full-disk.profile.img
    ' >/dev/null
mv "$work/full-disk.img.new" "$work/full-disk.img"
printf '%s\n' "$work/full-disk.img"
