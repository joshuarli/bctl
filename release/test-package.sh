#!/bin/sh
set -eu
TMPDIR="$(pwd)/.work/tmp"
export TMPDIR
mkdir -p "$TMPDIR"
testdir=$(mktemp -d "$TMPDIR/package-test.XXXXXXXX")
trap 'rm -rf "$testdir"' EXIT HUP INT TERM
package=$(pwd)/release/package.sh
commit=0123456789012345678901234567890123456789
mkdir -p "$testdir/runtime" "$testdir/images"
cp /bin/sh "$testdir/bctl"
if [ "$(uname -s)" = Darwin ]; then
mkdir -p "$testdir/runtime/lib"
cp /bin/sh "$testdir/runtime/nanos-krun"
printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>' > "$testdir/entitlements.plist"
codesign -fs - --entitlements "$testdir/entitlements.plist" "$testdir/runtime/nanos-krun" 2>/dev/null
printf fixture > "$testdir/runtime/lib/libkrun.dylib"
printf fixture > "$testdir/runtime/firmware.fd"
printf fixture > "$testdir/runtime/firmware-license.txt"
else
cp /bin/sh "$testdir/runtime/vhost-device-vsock"
cp /bin/sh "$testdir/runtime/virtiofsd"
printf kernel > "$testdir/images/sample.kernel.img"
fi
printf image > "$testdir/images/sample.img"
printf profile > "$testdir/images/sample.profile.img"
run() { sh "$package" --target "$1" --version test --commit "$2" --bctl "$testdir/bctl" --runtime "$testdir/runtime" --images "$testdir/images" --image sample --out "$3"; }
reject() { if "$@" > "$testdir/error" 2>&1; then printf 'unexpected success: %s\n' "$*" >&2; exit 1; fi; grep 'release/package.sh:' "$testdir/error" >/dev/null; }
reject run invalid "$commit" "$testdir/bad-target"
reject run x86_64-unknown-linux-musl bad "$testdir/bad-commit"
if [ "$(uname -s)" = Darwin ]; then
    target=aarch64-apple-darwin
    run "$target" "$commit" "$testdir/one"
    run "$target" "$commit" "$testdir/two"
    archive=bctl-test-$target.tar.xz
    cmp "$testdir/one/$archive" "$testdir/two/$archive"
    tar -tvf "$testdir/one/$archive" > "$testdir/list"
    cat "$testdir/list"
    [ "$(wc -l < "$testdir/list" | tr -d ' ')" = 10 ]
    grep '^-rwxr-xr-x .*bin/bctl$' "$testdir/list" >/dev/null
    grep '^-rwxr-xr-x .*runtime/nanos/nanos-krun$' "$testdir/list" >/dev/null
    grep '^-rw-r--r-- .*runtime/nanos/lib/libkrun.dylib$' "$testdir/list" >/dev/null
    grep '^-rw-r--r-- .*runtime/nanos/firmware.fd$' "$testdir/list" >/dev/null
    grep '^-rw-r--r-- .*runtime/nanos/firmware-license.txt$' "$testdir/list" >/dev/null
    [ "$(grep -c '^drwxr-xr-x ' "$testdir/list")" = 5 ]
    jaq -e --arg commit "$commit" --arg archive "$archive" ".schema == 1 and .version == \"test\" and .target == \"aarch64-apple-darwin\" and .commit == \$commit and .archive.name == \$archive and (.archive.sha256 | length == 64) and (.images.sample | keys == [\"sample.img\", \"sample.profile.img\"]) and ([.images.sample[] | length == 64] | all)" "$testdir/one/manifest.json" >/dev/null
    (cd "$testdir/one" && shasum -a 256 -c SHA256SUMS)
    mv "$testdir/runtime/firmware.fd" "$testdir/firmware.fd"
    reject run "$target" "$commit" "$testdir/missing-runtime"
    mv "$testdir/firmware.fd" "$testdir/runtime/firmware.fd"
    touch "$testdir/runtime/extra"
    reject run "$target" "$commit" "$testdir/extra-runtime"
    rm "$testdir/runtime/extra"
    mv "$testdir/images/sample.img" "$testdir/sample.img"
    reject run "$target" "$commit" "$testdir/missing-image"
    mv "$testdir/sample.img" "$testdir/images/sample.img"
    reject run "$target" "$commit" "$testdir/one"
    cp /bin/sh "$testdir/runtime/nanos-krun"
    codesign -fs - "$testdir/runtime/nanos-krun" 2>/dev/null
    reject run "$target" "$commit" "$testdir/no-entitlement"
else
    target=x86_64-unknown-linux-musl
    run "$target" "$commit" "$testdir/one"
    run "$target" "$commit" "$testdir/two"
    archive=bctl-test-$target.tar.xz
    cmp "$testdir/one/$archive" "$testdir/two/$archive"
    tar -tvf "$testdir/one/$archive" > "$testdir/list"
    [ "$(wc -l < "$testdir/list" | tr -d ' ')" = 7 ]
    [ "$(grep -c '^-rwxr-xr-x ' "$testdir/list")" = 3 ]
    [ "$(grep -c '^drwxr-xr-x ' "$testdir/list")" = 4 ]
    jaq -e '.schema == 1 and .target == "x86_64-unknown-linux-musl" and (.images.sample | keys == ["sample.img", "sample.kernel.img", "sample.profile.img"])' "$testdir/one/manifest.json" >/dev/null
    (cd "$testdir/one" && sha256sum -c SHA256SUMS)
    mv "$testdir/runtime/virtiofsd" "$testdir/virtiofsd"
    reject run "$target" "$commit" "$testdir/missing-runtime"
    mv "$testdir/virtiofsd" "$testdir/runtime/virtiofsd"
    touch "$testdir/runtime/extra"
    reject run "$target" "$commit" "$testdir/extra-runtime"
    rm "$testdir/runtime/extra"
    mv "$testdir/images/sample.kernel.img" "$testdir/kernel.img"
    reject run "$target" "$commit" "$testdir/missing-image"
    mv "$testdir/kernel.img" "$testdir/images/sample.kernel.img"
    reject run "$target" "$commit" "$testdir/one"
fi
printf '%s\n' 'package fixture tests passed'
