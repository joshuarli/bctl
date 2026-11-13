#!/bin/sh
set -eu

fail() { printf 'release/package-images.sh: %s\n' "$*" >&2; exit 1; }
target='' commit='' repo='' images='' image='' out=''
while [ "$#" -gt 0 ]; do
    [ "$#" -ge 2 ] || fail "missing value for $1"
    case $1 in
        --target) [ -z "$target" ] || fail 'duplicate --target'; target=$2 ;;
        --commit) [ -z "$commit" ] || fail 'duplicate --commit'; commit=$2 ;;
        --repo) [ -z "$repo" ] || fail 'duplicate --repo'; repo=$2 ;;
        --images) [ -z "$images" ] || fail 'duplicate --images'; images=$2 ;;
        --image) [ -z "$image" ] || fail 'duplicate --image'; image=$2 ;;
        --out) [ -z "$out" ] || fail 'duplicate --out'; out=$2 ;;
        *) fail "unknown option $1" ;;
    esac
    shift 2
done
[ -n "$target" ] && [ -n "$commit" ] && [ -n "$repo" ] && [ -n "$images" ] && [ -n "$image" ] && [ -n "$out" ] || fail 'all options are required'
case $target in aarch64-apple-darwin|x86_64-unknown-linux-musl) ;; *) fail 'unknown target' ;; esac
case $commit in *[!0-9a-f]*|'') fail 'invalid commit' ;; esac
[ "${#commit}" -eq 40 ] || fail 'invalid commit'
case $image in *[!0-9A-Za-z._-]*|''|.|..) fail 'invalid image name' ;; esac
case $repo in */*) owner=${repo%%/*}; repository=${repo#*/} ;; *) fail 'invalid repo' ;; esac
case $owner in *[!0-9A-Za-z._-]*|'') fail 'invalid repo' ;; esac
case $repository in *[!0-9A-Za-z._-]*|'') fail 'invalid repo' ;; esac
[ -d "$images" ] || fail 'invalid images directory'
image_files="$image.img $image.profile.img"
[ "$target" != x86_64-unknown-linux-musl ] || image_files="$image.img $image.kernel.img $image.profile.img"
for name in $image_files; do [ -f "$images/$name" ] || fail "missing image file: $name"; done
if [ -e "$out" ] || [ -L "$out" ]; then
    [ -d "$out" ] && [ ! -L "$out" ] || fail 'out must be an empty directory'
    [ -z "$(ls -A "$out")" ] || fail 'out must be empty'
else
    mkdir -p "$out" || fail 'cannot create out'
fi
scratch=$(mktemp -d "${TMPDIR:-/tmp}/bctl-images.XXXXXXXX") || fail 'cannot create temporary directory'
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
hash() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d ' ' -f 1; }
image_json='{}'
for name in $image_files; do
    digest=$(hash "$images/$name")
    image_json=$(jaq -nc --argjson files "$image_json" --arg name "$name" --arg digest "$digest" "\$files + {(\$name): \$digest}")
    case $(uname -s) in
        Darwin) cp -c "$images/$name" "$scratch/$name" || fail "cannot stage image: $name" ;;
        *) cp --sparse=always "$images/$name" "$scratch/$name" || fail "cannot stage image: $name" ;;
    esac
    TZ=UTC0 touch -t 200001010000 "$scratch/$name"
done
archive=$image-$target.zip
set -- "./$image.img" "./$image.profile.img"
[ "$target" != x86_64-unknown-linux-musl ] || set -- "./$image.img" "./$image.kernel.img" "./$image.profile.img"
(cd "$scratch" && TZ=UTC0 zip -9 -X -j "./$archive" "$@" >/dev/null) || fail 'cannot create zip'
mv "$scratch/$archive" "$out/$archive"
archive_hash=$(hash "$out/$archive")
jaq -nc --arg target "$target" --arg commit "$commit" --arg repo "$repo" --arg image "$image" --arg archive "$archive" --arg digest "$archive_hash" --argjson files "$image_json" "{schema: 1, target: \$target, release: (\"release-\" + \$commit), base_url: (\"https://github.com/\" + \$repo + \"/releases/download/release-\" + \$commit + \"/\"), images: {(\$image): {archive: {name: \$archive, sha256: \$digest}, files: \$files}}}" > "$out/images.json"
(cd "$out" && { printf '%s  %s\n' "$archive_hash" "$archive"; printf '%s  %s\n' "$(hash images.json)" images.json; } | LC_ALL=C sort -k 2) > "$out/SHA256SUMS"
