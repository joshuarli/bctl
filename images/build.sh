#!/bin/sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
name=${1:?usage: images/build.sh IMAGE}
dir="$root/images/$name"
[ -f "$dir/Dockerfile" ] || { echo "images/build.sh: no image named $name" >&2; exit 1; }
out="$root/.work/images"
mkdir -p "$out"
cp "$root/images/bctl-guest" "$dir/bctl-guest"
trap 'rm -f "$dir/bctl-guest"' EXIT
docker build -q --platform linux/arm64 --build-context guest="$root/guest" -t "bctl/$name" "$dir" >/dev/null
docker save "bctl/$name" -o "$out/$name.tar.part"
mv "$out/$name.tar.part" "$out/$name.tar"
echo "$out/$name.tar"
