#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
script=$root/images/nanos-helium/build.sh
work=$root/.work/images/nanos-helium
out=$root/.work/images/nanos-helium.img
kernel=$root/.work/images/nanos-helium.kernel.img
tree=$root/.work/runtime/nanos/hydrogen
tree_kernel=$tree/nanos/output/platform/pc/bin/kernel.img
archives=${BCTL_BROWSER_ARCHIVE:?set BCTL_BROWSER_ARCHIVE to a directory holding one helium-*-linux-x86_64-musl.tar.xz with its sidecars}
tmp=$root/.work/tmp/nanos-helium-cache

fail() { echo "tests/build/nanos-helium-cache.sh: $*" >&2; exit 1; }
run() { output=$(BCTL_BROWSER_ARCHIVE=$archives FORCE=0 sh "$script" 2>&1) || fail "build failed:\n$output"; }
show() {
    echo "== $1"
    printf '%s\n' "$output" | grep -E '^(installer|rootfs|guest|image): '
    for part in installer rootfs guest image; do
        count=$(printf '%s\n' "$output" | grep -c "^$part: ")
        [ "$count" = 1 ] || fail "want one $part line, got $count in:\n$output"
    done
}
expect() {
    for line in "$@"; do
        printf '%s\n' "$output" | grep -qxF "$line" || fail "want '$line' in:\n$output"
    done
}

[ -d "$archives" ] || fail "set BCTL_BROWSER_ARCHIVE to the directory holding the helium x86_64 archive (looked in $archives)"
set -- "$archives"/*.tar.xz
[ $# = 1 ] && [ -f "$1" ] || fail "want exactly one *.tar.xz in $archives, found: $*"
[ -x "$tree/nanos/output/tools/bin/mkfs" ] || fail "run runtime/build-nanos.sh first"

rm -rf "$tmp"
mkdir -p "$tmp"
rm -rf "$work" "$out" "$kernel"

run
show cold
expect "installer: built" "rootfs: built" "guest: built" "image: built"
[ -s "$out" ] || fail "no $out"
[ -s "$kernel" ] || fail "no $kernel"
cmp -s "$kernel" "$tree_kernel" || fail "$kernel is not the kernel.img from the tree whose klibs were packed"

run
show unchanged
expect "installer: reused" "rootfs: reused" "guest: reused" "image: reused"

rm -f "$kernel"
run
show "kernel copy removed"
expect "installer: reused" "rootfs: reused" "guest: reused" "image: built"
cmp -s "$kernel" "$tree_kernel" || fail "$kernel was not rewritten from the packed tree"

source_file=$root/guest/appliance/src/main.rs
source_backup=$tmp/main.rs.orig
cp -p "$source_file" "$source_backup"
restored=0
restore_source() {
    if [ "$restored" = 0 ]; then
        cp -p "$source_backup" "$source_file"
        restored=1
    fi
}
trap restore_source EXIT
[ -d "$work/target" ] || fail "the guest target is not persistent at $work/target"
printf '\n#[unsafe(no_mangle)]\npub extern "C" fn bctl_nanos_cache_test() {}\n' >>"$source_file"
run
show "guest code changed"
expect "installer: reused" "rootfs: reused" "guest: built" "image: built"
[ -d "$work/target" ] || fail "the guest build deleted $work/target"
[ -d "$work/target/x86_64-unknown-linux-musl/release" ] || fail "the guest build did not keep its release target"

restore_source
touch "$source_file"
run
show "guest code restored"
expect "installer: reused" "rootfs: reused" "guest: built" "image: built"

printf 'corrupt\n' >>"$work/rootfs/etc/passwd"
run
show "rootfs corrupted"
expect "installer: reused" "rootfs: built" "guest: reused" "image: reused"

rm -f "$work/rootfs/etc/passwd"
run
show "rootfs file removed"
expect "installer: reused" "rootfs: built" "guest: reused" "image: reused"

archive=$1
name=${archive##*/}
scratch=$tmp/sidecars
mkdir -p "$scratch"
ln -s "$(cd "$(dirname "$archive")" && pwd)/$name" "$scratch/$name"
cp "$archive.sha256" "$scratch/$name.sha256"
cp "$archive.acceptance.json" "$scratch/$name.acceptance.json"
printf '0000000000000000000000000000000000000000000000000000000000000000  %s\n' "$name" >"$scratch/$name.sha256"
if BCTL_BROWSER_ARCHIVE=$scratch sh "$script" >"$tmp/bad-sha.log" 2>&1; then
    fail "a .sha256 sidecar that does not bind the archive was accepted"
fi
grep -q 'the .sha256 sidecar' "$tmp/bad-sha.log" || fail "the .sha256 refusal did not name the sidecar:\n$(cat "$tmp/bad-sha.log")"
echo "== mismatched .sha256 refused"

cp "$archive.acceptance.json" "$scratch/$name.acceptance.json"
cp "$archive.sha256" "$scratch/$name.sha256"
sed 's/"archive_sha256": "[0-9a-f]*"/"archive_sha256": "0000000000000000000000000000000000000000000000000000000000000000"/' \
    "$archive.acceptance.json" >"$scratch/$name.acceptance.json"
if BCTL_BROWSER_ARCHIVE=$scratch sh "$script" >"$tmp/bad-acceptance.log" 2>&1; then
    fail "an .acceptance.json sidecar that does not name the archive digest was accepted"
fi
grep -q 'the acceptance sidecar has archive_sha256' "$tmp/bad-acceptance.log" ||
    fail "the .acceptance.json refusal did not name archive_sha256:\n$(cat "$tmp/bad-acceptance.log")"
echo "== mismatched .acceptance.json refused"

[ -s "$out" ] || fail "no $out at the end"
[ -s "$kernel" ] || fail "no $kernel at the end"
echo "nanos-helium cache checks passed"
