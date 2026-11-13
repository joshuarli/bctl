#!/bin/sh
set -eu

fail() { printf 'release/package.sh: %s\n' "$*" >&2; exit 1; }
target='' version='' commit='' bctl='' runtime='' images_manifest='' out=''
while [ "$#" -gt 0 ]; do
    [ "$#" -ge 2 ] || fail "missing value for $1"
    case $1 in
        --target) [ -z "$target" ] || fail 'duplicate --target'; target=$2 ;;
        --version) [ -z "$version" ] || fail 'duplicate --version'; version=$2 ;;
        --commit) [ -z "$commit" ] || fail 'duplicate --commit'; commit=$2 ;;
        --bctl) [ -z "$bctl" ] || fail 'duplicate --bctl'; bctl=$2 ;;
        --runtime) [ -z "$runtime" ] || fail 'duplicate --runtime'; runtime=$2 ;;
        --images-manifest) [ -z "$images_manifest" ] || fail 'duplicate --images-manifest'; images_manifest=$2 ;;
        --out) [ -z "$out" ] || fail 'duplicate --out'; out=$2 ;;
        *) fail "unknown option $1" ;;
    esac
    shift 2
done
[ -n "$target" ] && [ -n "$version" ] && [ -n "$commit" ] && [ -n "$bctl" ] && [ -n "$runtime" ] && [ -n "$images_manifest" ] && [ -n "$out" ] || fail 'all options are required'
case $target in aarch64-apple-darwin|x86_64-unknown-linux-musl) ;; *) fail 'unknown target' ;; esac
case $version in *[!0-9A-Za-z._-]*|'') fail 'invalid version' ;; esac
case $commit in *[!0-9a-fA-F]*|'') fail 'invalid commit' ;; esac
[ "${#commit}" -eq 40 ] || fail 'invalid commit'
[ -f "$bctl" ] && [ ! -L "$bctl" ] && [ -x "$bctl" ] || fail 'bctl must be an executable regular file'
case $target in
    aarch64-apple-darwin) file "$bctl" | LC_ALL=C grep -E 'Mach-O.*(arm64|aarch64).*executable|Mach-O.*executable.*(arm64|aarch64)' >/dev/null || fail 'bctl is not a Mach-O arm64 executable' ;;
    *) file "$bctl" | LC_ALL=C grep -E 'ELF 64-bit.*(x86-64|x86_64)' >/dev/null || fail 'bctl is not an ELF x86-64 executable' ;;
esac
[ -d "$runtime" ] && [ ! -L "$runtime" ] || fail 'invalid runtime directory'
[ -f "$images_manifest" ] || fail 'invalid images manifest'
jaq -e --arg target "$target" ".schema == 1 and .target == \$target and (.images | type == \"object\")" "$images_manifest" >/dev/null || fail 'invalid images manifest'
if [ -e "$out" ] || [ -L "$out" ]; then
    [ -d "$out" ] && [ ! -L "$out" ] || fail 'out must be an empty directory'
    [ -z "$(ls -A "$out")" ] || fail 'out must be empty'
else
    mkdir -p "$out" || fail 'cannot create out'
fi
scratch=$(mktemp -d "${TMPDIR:-/tmp}/bctl-package.XXXXXXXX") || fail 'cannot create temporary directory'
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
case $target in
    aarch64-apple-darwin)
        printf '%s\n' ./ ./firmware-license.txt ./firmware.fd ./lib/ ./lib/libkrun.dylib ./nanos-krun | LC_ALL=C sort > "$scratch/expected"
        runtime_files='firmware-license.txt firmware.fd lib/libkrun.dylib nanos-krun'
        ;;
    *)
        printf '%s\n' ./ ./vhost-device-vsock ./virtiofsd | LC_ALL=C sort > "$scratch/expected"
        runtime_files='vhost-device-vsock virtiofsd'
        ;;
esac
tar -cf "$scratch/runtime.tar" -C "$runtime" . || fail 'cannot inspect runtime'
tar -tf "$scratch/runtime.tar" | LC_ALL=C sort > "$scratch/actual"
cmp -s "$scratch/expected" "$scratch/actual" || fail 'runtime contains missing or extra entries'
for name in $runtime_files; do
    [ -f "$runtime/$name" ] && [ ! -L "$runtime/$name" ] || fail "runtime file is not regular: $name"
done
if [ "$target" = aarch64-apple-darwin ]; then
    [ -d "$runtime/lib" ] && [ ! -L "$runtime/lib" ] || fail 'runtime lib must be a directory'
    codesign --verify --strict "$runtime/nanos-krun" || fail 'runtime signature verification failed'
    codesign -d --entitlements :- "$runtime/nanos-krun" > "$scratch/entitlements" 2>/dev/null || fail 'cannot read runtime entitlements'
    plutil -convert json -o - "$scratch/entitlements" | jaq -e '.["com.apple.security.hypervisor"] == true' >/dev/null || fail 'runtime lacks hypervisor entitlement'
else
    [ -x "$runtime/vhost-device-vsock" ] && [ -x "$runtime/virtiofsd" ] || fail 'runtime executables must be executable'
fi
base=bctl-$version-$target
mkdir -p "$scratch/stage/$base/bin" "$scratch/stage/$base/runtime/nanos"
cp "$bctl" "$scratch/stage/$base/bin/bctl"
for name in $runtime_files; do
    mkdir -p "$scratch/stage/$base/runtime/nanos/$(dirname "$name")"
    cp "$runtime/$name" "$scratch/stage/$base/runtime/nanos/$name"
done
chmod 755 "$scratch/stage/$base" "$scratch/stage/$base/bin" "$scratch/stage/$base/runtime" "$scratch/stage/$base/runtime/nanos" "$scratch/stage/$base/bin/bctl"
case $target in
    aarch64-apple-darwin)
        chmod 755 "$scratch/stage/$base/runtime/nanos/lib" "$scratch/stage/$base/runtime/nanos/nanos-krun"
        chmod 644 "$scratch/stage/$base/runtime/nanos/firmware-license.txt" "$scratch/stage/$base/runtime/nanos/firmware.fd" "$scratch/stage/$base/runtime/nanos/lib/libkrun.dylib"
        codesign --verify --strict "$scratch/stage/$base/runtime/nanos/nanos-krun" || fail 'staged runtime signature verification failed'
        ;;
    *) chmod 755 "$scratch/stage/$base/runtime/nanos/vhost-device-vsock" "$scratch/stage/$base/runtime/nanos/virtiofsd" ;;
esac
printf '%s\n' "$base/" "$base/bin/" "$base/bin/bctl" "$base/runtime/" "$base/runtime/nanos/" > "$scratch/list"
[ "$target" != aarch64-apple-darwin ] || printf '%s\n' "$base/runtime/nanos/lib/" >> "$scratch/list"
for name in $runtime_files; do printf '%s\n' "$base/runtime/nanos/$name" >> "$scratch/list"; done
LC_ALL=C sort -o "$scratch/list" "$scratch/list"
while IFS= read -r name; do TZ=UTC0 touch -t 200001010000 "$scratch/stage/$name"; done < "$scratch/list"
case $(tar --version) in
    *bsdtar*) tar -cf "$scratch/archive.tar" --format ustar --uid 0 --gid 0 --uname '' --gname '' --no-recursion --no-xattrs --no-acls --no-fflags -C "$scratch/stage" -T "$scratch/list" ;;
    *GNU*) tar -cf "$scratch/archive.tar" --format=ustar --owner=0 --group=0 --numeric-owner --no-recursion -C "$scratch/stage" -T "$scratch/list" ;;
    *) fail 'unsupported tar' ;;
esac
xz -9e -T1 -c "$scratch/archive.tar" > "$out/$base.tar.xz"
hash() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d ' ' -f 1; }
archive_hash=$(hash "$out/$base.tar.xz")
jaq -nc --arg version "$version" --arg target "$target" --arg commit "$commit" --arg archive "$base.tar.xz" --arg digest "$archive_hash" --argjson images "$(jaq -c '.images' "$images_manifest")" "{schema: 1, version: \$version, target: \$target, commit: \$commit, release: (\"release-\" + \$commit), archive: {name: \$archive, sha256: \$digest}, images: \$images}" > "$out/manifest.json"
jaq . "$out/manifest.json" >/dev/null || fail 'invalid manifest'
(cd "$out" && { printf '%s  %s\n' "$archive_hash" "$base.tar.xz"; printf '%s  %s\n' "$(hash manifest.json)" manifest.json; } | LC_ALL=C sort -k 2) > "$out/SHA256SUMS"
