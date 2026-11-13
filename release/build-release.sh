#!/bin/sh
set -eu

fail() { printf 'release/build-release.sh: %s\n' "$*" >&2; exit 1; }
target='' runtime='' images='' image='' out='' publish=''
while [ "$#" -gt 0 ]; do
    [ "$#" -ge 2 ] || fail "missing value for $1"
    case $1 in
        --target) [ -z "$target" ] || fail 'duplicate --target'; target=$2 ;;
        --runtime) [ -z "$runtime" ] || fail 'duplicate --runtime'; runtime=$2 ;;
        --images) [ -z "$images" ] || fail 'duplicate --images'; images=$2 ;;
        --image) [ -z "$image" ] || fail 'duplicate --image'; image=$2 ;;
        --out) [ -z "$out" ] || fail 'duplicate --out'; out=$2 ;;
        --publish) [ -z "$publish" ] || fail 'duplicate --publish'; publish=$2 ;;
        *) fail "unknown option $1" ;;
    esac
    shift 2
done
[ -n "$target" ] && [ -n "$runtime" ] && [ -n "$images" ] && [ -n "$out" ] || fail 'usage: build-release.sh --target T --runtime DIR --images DIR [--image NAME] --out DIR [--publish OWNER/REPO]'
image=${image:-nanos-helium}
root=$(cd "$(dirname "$0")/.." && pwd)
[ -z "$(git -C "$root" status --porcelain --untracked-files=no)" ] || fail 'the checkout has uncommitted changes'
commit=$(git -C "$root" rev-parse HEAD)
repo=${publish:-$(git -C "$root" remote get-url origin | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##')}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)-$(printf '%s' "$commit" | cut -c 1-12)
case $target in
    aarch64-apple-darwin) [ "$(uname -s)-$(uname -m)" = Darwin-arm64 ] || fail 'build aarch64-apple-darwin on an Apple Silicon Mac' ;;
    x86_64-unknown-linux-musl) [ "$(uname -s)" = Linux ] || fail 'build x86_64-unknown-linux-musl on Linux' ;;
    *) fail 'unknown target' ;;
esac
if [ -e "$out" ] || [ -L "$out" ]; then
    [ -d "$out" ] && [ ! -L "$out" ] && [ -z "$(ls -A "$out")" ] || fail 'out must be an empty directory'
fi
mkdir -p "$out"
out=$(cd "$out" && pwd)
mkdir -p "$out/.stage"
sh "$root/release/package-images.sh" --target "$target" --commit "$commit" --repo "$repo" --images "$images" --image "$image" --out "$out/.stage/images"
BCTL_IMAGES_MANIFEST="$out/.stage/images/images.json" cargo build --locked --release -p bctl --target "$target" --manifest-path "$root/Cargo.toml" ||
    fail 'cannot build bctl'
sh "$root/release/package.sh" --target "$target" --version "$version" --commit "$commit" \
    --bctl "$root/target/$target/release/bctl" --runtime "$runtime" \
    --images-manifest "$out/.stage/images/images.json" --out "$out/.stage/package"
mv "$out/.stage/package/bctl-$version-$target.tar.xz" "$out/"
mv "$out/.stage/package/manifest.json" "$out/bctl-$target.manifest.json"
mv "$out/.stage/images/$image-$target.zip" "$out/"
rm -rf "$out/.stage"
hash() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d ' ' -f 1; }
(cd "$out" && for name in "bctl-$version-$target.tar.xz" "bctl-$target.manifest.json" "$image-$target.zip"; do
    printf '%s  %s\n' "$(hash "$name")" "$name"
done | LC_ALL=C sort -k 2) > "$out/SHA256SUMS-$target"
printf '%s\n' "release-$commit" > "$out/.tag"
[ -n "$publish" ] || { printf 'built release-%s for %s in %s\n' "$commit" "$target" "$out"; exit 0; }
tag=release-$commit
if gh release view "$tag" -R "$publish" >/dev/null 2>&1; then
    gh release upload "$tag" -R "$publish" "$out"/bctl-* "$out/$image-$target.zip" "$out/SHA256SUMS-$target"
else
    gh release create "$tag" -R "$publish" --target "$commit" --title "$tag" --notes "bctl $version" \
        "$out"/bctl-* "$out/$image-$target.zip" "$out/SHA256SUMS-$target"
fi
printf 'published %s for %s to %s\n' "$tag" "$target" "$publish"
