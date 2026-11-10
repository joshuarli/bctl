#!/bin/sh
set -eu

hydrogen_commit=868726ad45a2b391367a094b1d54010dbb0fe7c7
vhost_vsock_version=0.3.0
vhost_vsock_sha256=52ce95d81af89ad693d067c650e42df0f217addfbc96cc4820f7c62eb4a28b13
vhost_vsock_patch=vhost-device-vsock-0001-forward-guest-half-close.patch
root=$(cd "$(dirname "$0")/.." && pwd)
hydrogen=${HYDROGEN:-$HOME/d/hydrogen}
out=$root/.work/runtime/nanos
tree=$out/hydrogen

fail() { echo "runtime/build-nanos.sh: $*" >&2; exit 1; }

[ "$(uname -s)/$(uname -m)" = Linux/x86_64 ] || fail "the Nanos runtime builds only on Linux x86_64 hosts"
[ -c /dev/kvm ] || fail "/dev/kvm is missing"
command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"
git -C "$hydrogen" cat-file -e "$hydrogen_commit^{commit}" 2>/dev/null ||
    fail "hydrogen commit $hydrogen_commit is not in $hydrogen (set HYDROGEN to a hydrogen clone)"

bin=$tree/nanos/output/platform/pc/bin
if [ "${FORCE:-0}" != 1 ] && [ "$(cat "$tree/COMMIT" 2>/dev/null)" = "$hydrogen_commit" ] &&
    [ -s "$bin/kernel.img" ] && [ -x "$tree/nanos/output/tools/bin/mkfs" ] &&
    [ -x "$out/bin/vhost-device-vsock" ] && [ -x "$out/llvm/bin/clang" ]; then
    echo "$out is current; set FORCE=1 to rebuild"
    exit 0
fi

rm -rf "$tree"
mkdir -p "$tree"
git -C "$hydrogen" archive "$hydrogen_commit" | tar -x -C "$tree"
make -C "$tree/nanos" -j"$(nproc)" kernel tools CC=gcc >"$out/nanos-build.log" 2>&1 ||
    { tail -n 30 "$out/nanos-build.log" >&2; fail "kernel build failed; log in $out/nanos-build.log"; }
[ -s "$bin/kernel.img" ] && [ -x "$tree/nanos/output/tools/bin/mkfs" ] || fail "kernel build produced no kernel.img or mkfs"

mkdir -p "$out/dl"
vhost_vsock_crate=$out/dl/vhost-device-vsock-$vhost_vsock_version.crate
if ! printf '%s  %s\n' "$vhost_vsock_sha256" "$vhost_vsock_crate" | sha256sum -c -s 2>/dev/null; then
    curl --proto '=https' --tlsv1.2 -fsSL -o "$vhost_vsock_crate.part" \
        "https://static.crates.io/crates/vhost-device-vsock/vhost-device-vsock-$vhost_vsock_version.crate" ||
        fail "cannot fetch vhost-device-vsock $vhost_vsock_version"
    mv "$vhost_vsock_crate.part" "$vhost_vsock_crate"
    printf '%s  %s\n' "$vhost_vsock_sha256" "$vhost_vsock_crate" | sha256sum -c -s ||
        fail "vhost-device-vsock $vhost_vsock_version does not match its crates.io checksum"
fi
vhost_vsock_src=$out/src/vhost-device-vsock-$vhost_vsock_version
rm -rf "$vhost_vsock_src"
mkdir -p "$out/src"
tar -xzf "$vhost_vsock_crate" -C "$out/src"
patch -p1 -d "$vhost_vsock_src" --batch --forward --silent -i "$root/runtime/patches/$vhost_vsock_patch" ||
    fail "runtime/patches/$vhost_vsock_patch does not apply to vhost-device-vsock $vhost_vsock_version"
cargo install --quiet --locked --force --root "$out" --path "$vhost_vsock_src" ||
    fail "cannot build vhost-device-vsock $vhost_vsock_version"

. "$tree/dev/toolchain.lock"
llvm_tar=$out/dl/$LLVM_DIR.tar.xz
mkdir -p "$out/dl"
if ! printf '%s  %s\n' "$LLVM_SHA256" "$llvm_tar" | sha256sum -c -s 2>/dev/null; then
    curl --proto '=https' --tlsv1.2 -fsSL "$LLVM_URL" -o "$llvm_tar.part" || fail "cannot fetch $LLVM_URL"
    mv "$llvm_tar.part" "$llvm_tar"
    printf '%s  %s\n' "$LLVM_SHA256" "$llvm_tar" | sha256sum -c -s || fail "$LLVM_RELEASE does not match its pinned SHA-256"
fi
rm -rf "$out/llvm"
mkdir -p "$out/llvm"
tar -xJf "$llvm_tar" -C "$out/llvm" --strip-components=1

printf '%s\n' "$hydrogen_commit" >"$tree/COMMIT"
echo "kernel $bin/kernel.img, mkfs $tree/nanos/output/tools/bin/mkfs, $out/bin/vhost-device-vsock, $out/llvm"
