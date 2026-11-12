#!/bin/sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
name=${1:?usage: images/build.sh IMAGE}
dir="$root/images/$name"
[ -f "$dir/Dockerfile" ] || { echo "images/build.sh: no image named $name" >&2; exit 1; }
set -- --build-context "guest=$root/guest" --build-context "$(sh "$root/guest/harness/tinyland-context.sh")"
if [ -n "${BCTL_BROWSER_ARCHIVE:-}" ]; then
    [ -d "$BCTL_BROWSER_ARCHIVE" ] || { echo "images/build.sh: BCTL_BROWSER_ARCHIVE=$BCTL_BROWSER_ARCHIVE is not a directory" >&2; exit 1; }
    set -- "$@" --build-context "browser-archive=$BCTL_BROWSER_ARCHIVE"
elif [ "$name" = helium ]; then
    echo "images/build.sh: helium needs BCTL_BROWSER_ARCHIVE, a directory holding one helium-*-linux-aarch64-musl.tar.xz and its .sha256 and .acceptance.json" >&2
    exit 1
fi
out="$root/.work/images"
mkdir -p "$out"
cp "$root/images/bctl-guest" "$dir/bctl-guest"
trap 'rm -f "$dir/bctl-guest"' EXIT
docker build -q --platform linux/arm64 "$@" -t "bctl/$name" "$dir" >/dev/null
docker save "bctl/$name" -o "$out/$name.tar.part"
mv "$out/$name.tar.part" "$out/$name.tar"
echo "$out/$name.tar"
