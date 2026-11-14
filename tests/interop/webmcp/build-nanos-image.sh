#!/bin/sh
set -eu

[ $# -eq 2 ] || { echo 'usage: build-nanos-image.sh TEST_TAR nanos-webmcp-NAME' >&2; exit 2; }
root=$(cd "$(dirname "$0")/../../.." && pwd)
archive=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
name=$2
case $name in nanos-webmcp-*) ;; *) echo 'use an isolated nanos-webmcp-NAME image name' >&2; exit 2 ;; esac
case $name in *[!a-zA-Z0-9_-]*) echo 'invalid image name' >&2; exit 2 ;; esac
out=$root/.work/images/$name
[ ! -e "$out.img" ] || { echo "$out.img already exists; choose an unused image name" >&2; exit 1; }
case $(uname -s)/$(uname -m) in
Darwin/arm64) platform=virt ;;
Linux/x86_64) platform=pc ;;
*) echo 'only macOS ARM64 and Linux x86_64 are qualified' >&2; exit 1 ;;
esac
common=$(git -C "$root" rev-parse --path-format=absolute --git-common-dir)
if ! git -C "$root" check-ignore -q .work/tmp; then
    exclude=$(git -C "$root" rev-parse --path-format=absolute --git-path info/exclude)
    printf '\n.work/\n' >>"$exclude"
fi
linux() {
    if [ "$platform" = virt ]; then
        docker run --rm --platform linux/arm64 -v "$root:$root" -v "$common:$common:ro" \
            -w "$root" hydrogen/aarch64-builder "$@"
    else
        "$@"
    fi
}
mkdir -p "$root/.work/tmp" "$root/.work/images"
scratch=$(mktemp -d "$root/.work/tmp/webmcp-nanos.XXXXXX")
container=''
cleanup() {
    status=$?
    trap - EXIT
    if [ -n "$container" ] && ! docker rm "$container" >/dev/null; then
        printf 'Could not remove test container %s\n' "$container" >&2
    fi
    rm -rf "$scratch"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
tag=$(tar -xOf "$archive" manifest.json | jaq -er '.[0].RepoTags[0]')
case $tag in bctl/webmcp-test:*) ;; *) echo 'input must come from build-image.sh' >&2; exit 1 ;; esac
docker load -i "$archive" >&2
container=$(docker create "$tag")
docker export -o "$scratch/rootfs.tar" "$container"
mkdir "$scratch/rootfs"
tar -xf "$scratch/rootfs.tar" -C "$scratch/rootfs"
docker rm "$container" >/dev/null
container=''
sh "$root/kernel/dev/build.sh" >&2
linux sh "$root/kernel/dev/image.sh" -o "$scratch/base.img" --size 2g --klib tmpfs --klib shmem \
    --root "$scratch/rootfs" --tree "$scratch/rootfs:/" \
    --dir /profile --dir /tmp --dir /dev/shm --dir /home/browser/Downloads \
    --arg run-headless-persistent "$scratch/rootfs/usr/local/bin/bctl-guest" >&2
kernel=$root/kernel/nanos/output/platform/$platform/bin/kernel.img
mkfs=$root/kernel/nanos/output/tools/bin/mkfs
if [ "$platform" = virt ]; then
    set -- -b "$root/kernel/nanos/output/platform/virt/boot-stub.img" \
        -u "$root/kernel/nanos/output/platform/virt/boot/bootaa64.efi"
else
    set -- -b "$root/kernel/nanos/output/platform/pc/boot/boot.img"
fi
linux "$mkfs" -s 2g -k "$kernel" "$@" \
    -t '(mounts:(profile:"/profile" files:"/home/browser/Downloads"))' "$out.img" <"$scratch/base.img.manifest" >&2
linux "$mkfs" -e -l profile -s 2g "$out.profile.img" >&2
cp "$kernel" "$out.kernel.img"
printf '%s\n' "$out.img"
