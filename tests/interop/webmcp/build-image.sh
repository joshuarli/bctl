#!/bin/sh
set -eu

[ $# -eq 3 ] || { echo 'usage: build-image.sh BASE_TAR CA_CERT OUTPUT_TAR' >&2; exit 2; }
root=$(cd "$(dirname "$0")/../../.." && pwd)
source_dir=$root/tests/interop/webmcp
archive=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
certificate=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
mkdir -p "$(dirname "$3")"
output=$(cd "$(dirname "$3")" && pwd)/$(basename "$3")
if ! git -C "$root" check-ignore -q .work/tmp; then
    exclude=$(git -C "$root" rev-parse --path-format=absolute --git-path info/exclude)
    printf '\n.work/\n' >>"$exclude"
fi
mkdir -p "$root/.work/tmp"
scratch=$(mktemp -d "$root/.work/tmp/webmcp-image.XXXXXX")
base_tag=bctl/webmcp-base:qualification-$$
test_tag=bctl/webmcp-test:qualification-$$
cleanup() {
    status=$?
    trap - EXIT
    if ! docker image rm "$base_tag" "$test_tag" >/dev/null 2>&1; then
        printf 'Could not remove all test image tags: %s %s\n' "$base_tag" "$test_tag" >&2
    fi
    rm -rf "$scratch"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$scratch/archive" "$scratch/context"
tar -xf "$archive" -C "$scratch/archive"
jaq -e 'length == 1' "$scratch/archive/manifest.json" >/dev/null
jaq --arg tag "$base_tag" 'map(.RepoTags=[$tag])' "$scratch/archive/manifest.json" >"$scratch/manifest.json"
mv "$scratch/manifest.json" "$scratch/archive/manifest.json"
tar -cf "$scratch/base.tar" -C "$scratch/archive" manifest.json blobs
docker load -i "$scratch/base.tar" >&2
cp "$source_dir/browser.c" "$source_dir/Dockerfile" "$scratch/context/"
cp "$certificate" "$scratch/context/ca.crt"
docker build --build-arg BASE_IMAGE="$base_tag" -t "$test_tag" "$scratch/context" >&2
docker save "$test_tag" -o "$output"
printf '%s\n' "$output"
