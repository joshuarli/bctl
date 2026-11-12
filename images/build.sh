#!/bin/sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
name=${1:?usage: images/build.sh IMAGE}
dir="$root/images/$name"
[ -f "$dir/Dockerfile" ] || { echo "images/build.sh: no image named $name" >&2; exit 1; }
set -- --build-context "guest=$root/guest" --build-context "$(sh "$root/guest/harness/tinyland-context.sh")"
case "$(uname -m)" in
    arm64|aarch64) guest_arch=aarch64 platform=linux/arm64 ;;
    x86_64)
        guest_arch=x86_64 platform=linux/amd64
        set -- "$@" \
            --build-arg ROOTFS_BASE=alpine@sha256:79ff19e9084a00eece421b2523fb93e22d730e2c0e525905de047e848e56d95f \
            --build-arg LLVM_URL=https://github.com/laputa-systems/llvm-prebuilt-musl/releases/download/llvm-musl-23.1.5-3afbb74/clang%2Bllvm-23.1.5-x86_64-linux-musl.tar.xz \
            --build-arg LLVM_SHA256=c7c29a9c5eb56ab7f395b5cc9e2d6aa44b16fa07a4d22bcf0014d7d67f7a30fb
        ;;
    *) echo "images/build.sh: unsupported host architecture $(uname -m)" >&2; exit 1 ;;
esac
if [ -n "${BCTL_BROWSER_ARCHIVE:-}" ]; then
    [ -d "$BCTL_BROWSER_ARCHIVE" ] || { echo "images/build.sh: BCTL_BROWSER_ARCHIVE=$BCTL_BROWSER_ARCHIVE is not a directory" >&2; exit 1; }
    set -- "$@" --build-context "browser-archive=$BCTL_BROWSER_ARCHIVE"
elif [ "$name" = helium ]; then
    echo "images/build.sh: helium needs BCTL_BROWSER_ARCHIVE, a directory holding one helium-*-linux-$guest_arch-musl.tar.xz and its .sha256 and .acceptance.json" >&2
    exit 1
fi
out="$root/.work/images"
mkdir -p "$out"
cp "$root/images/bctl-guest" "$dir/bctl-guest"
trap 'rm -f "$dir/bctl-guest"' EXIT
docker build -q --platform "$platform" --build-arg GUEST_ARCH="$guest_arch" "$@" -t "bctl/$name" "$dir" >/dev/null
docker save "bctl/$name" -o "$out/$name.tar.part"
mv "$out/$name.tar.part" "$out/$name.tar"
echo "$out/$name.tar"
