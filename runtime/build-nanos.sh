#!/bin/sh
set -eu

hydrogen_commit=d103587c39c234f5497297dd2975ae9b2fedbba2
vhost_vsock_version=0.3.0
vhost_vsock_sha256=52ce95d81af89ad693d067c650e42df0f217addfbc96cc4820f7c62eb4a28b13
vhost_vsock_patch=vhost-device-vsock-0001-forward-guest-half-close.patch
root=$(cd "$(dirname "$0")/.." && pwd)
hydrogen=${HYDROGEN:-$HOME/d/hydrogen}
out=$root/.work/runtime/nanos
tree=$out/hydrogen
cc="ccache gcc"
force=${FORCE:-0}

fail() { echo "runtime/build-nanos.sh: $*" >&2; exit 1; }

sha() { sha256sum "$1" | cut -d' ' -f1; }
digest() { sha256sum | cut -d' ' -f1; }

kernel_identity() {
    printf 'hydrogen %s\n' "$hydrogen_commit"
    printf 'compiler %s\n' "$cc"
    printf 'gcc %s\n' "$(gcc --version | sed -n '1p')"
    printf 'ccache %s\n' "$(ccache --version | sed -n '1p')"
    printf 'targets kernel tools\n'
}

vsock_identity() {
    printf 'version %s\n' "$vhost_vsock_version"
    printf 'sha256 %s\n' "$vhost_vsock_sha256"
    printf 'patch %s\n' "$(sha "$root/runtime/patches/$vhost_vsock_patch")"
    printf 'rustc %s\n' "$(rustc -V)"
    printf 'cargo %s\n' "$(cargo -V)"
}

llvm_identity() {
    printf 'url %s\n' "$LLVM_URL"
    printf 'sha256 %s\n' "$LLVM_SHA256"
    printf 'dir %s\n' "$LLVM_DIR"
    printf 'release %s\n' "$LLVM_RELEASE"
}

stamp_valid() {
    stamp=$1 identity=$2
    shift 2
    [ -f "$stamp" ] || return 1
    [ "$(sed -n '1p' "$stamp")" = "$identity" ] || return 1
    for rel in "$@"; do
        [ -e "$out/$rel" ] || return 1
        want=$(awk -v path="$rel" '$2 == path { print $1; exit }' "$stamp")
        [ -n "$want" ] || return 1
        [ "$(sha "$out/$rel")" = "$want" ] || return 1
    done
}

write_stamp() {
    stamp=$1 identity=$2
    shift 2
    {
        printf '%s\n' "$identity"
        for rel in "$@"; do
            printf '%s  %s\n' "$(sha "$out/$rel")" "$rel"
        done
    } >"$stamp.new"
    mv "$stamp.new" "$stamp"
}

[ "$(uname -s)/$(uname -m)" = Linux/x86_64 ] || fail "the Nanos runtime builds only on Linux x86_64 hosts"
[ -c /dev/kvm ] || fail "/dev/kvm is missing"
command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"
command -v gcc >/dev/null || fail "gcc is not installed"
command -v ccache >/dev/null || fail "ccache is not installed"
command -v rustc >/dev/null || fail "rustc is not installed"
command -v cargo >/dev/null || fail "cargo is not installed"
git -C "$hydrogen" cat-file -e "$hydrogen_commit^{commit}" 2>/dev/null ||
    fail "hydrogen commit $hydrogen_commit is not in $hydrogen (set HYDROGEN to a hydrogen clone)"

mkdir -p "$out"

bin=$tree/nanos/output/platform/pc/bin
boot=$tree/nanos/output/platform/pc/boot
tools=$tree/nanos/output/tools/bin
mkfs=$tools/mkfs
kernel_stamp=$bin/.build-nanos-kernel.stamp
kernel_rel=hydrogen/nanos/output/platform/pc/bin/kernel.img
boot_rel=hydrogen/nanos/output/platform/pc/boot/boot.img
mkfs_rel=hydrogen/nanos/output/tools/bin/mkfs

kernel_id=$(kernel_identity | digest)
if [ "$force" = 1 ] || [ ! -s "$bin/kernel.img" ] || [ ! -s "$boot/boot.img" ] || [ ! -s "$mkfs" ] || [ ! -x "$mkfs" ] ||
    ! stamp_valid "$kernel_stamp" "$kernel_id" "$kernel_rel" "$boot_rel" "$mkfs_rel"; then
    rm -rf "$tree"
    mkdir -p "$tree"
    git -C "$hydrogen" archive "$hydrogen_commit" | tar -x -C "$tree"
    make -C "$tree/nanos" -j"$(nproc)" kernel tools CC="$cc" >"$out/nanos-build.log" 2>&1 ||
        { tail -n 30 "$out/nanos-build.log" >&2; fail "kernel build failed; log in $out/nanos-build.log"; }
    [ -s "$bin/kernel.img" ] && [ -s "$boot/boot.img" ] && [ -s "$mkfs" ] && [ -x "$mkfs" ] ||
        fail "kernel build produced no kernel.img, boot.img or mkfs"
    write_stamp "$kernel_stamp" "$kernel_id" "$kernel_rel" "$boot_rel" "$mkfs_rel"
    echo "kernel: built"
else
    echo "kernel: reused"
fi
printf '%s\n' "$hydrogen_commit" >"$tree/COMMIT"

vsock_bin=$out/bin/vhost-device-vsock
vsock_stamp=$out/bin/.build-nanos-vsock.stamp
vsock_rel=bin/vhost-device-vsock
vsock_id=$(vsock_identity | digest)
if [ "$force" = 1 ] || [ ! -s "$vsock_bin" ] || [ ! -x "$vsock_bin" ] || ! stamp_valid "$vsock_stamp" "$vsock_id" "$vsock_rel"; then
    mkdir -p "$out/dl" "$out/bin"
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
    [ -s "$vsock_bin" ] && [ -x "$vsock_bin" ] || fail "vhost-device-vsock built no $vsock_bin"
    write_stamp "$vsock_stamp" "$vsock_id" "$vsock_rel"
    echo "vhost-device-vsock: built"
else
    echo "vhost-device-vsock: reused"
fi

mkdir -p "$out/dl"
git -C "$hydrogen" show "$hydrogen_commit:dev/toolchain.lock" >"$out/toolchain.lock" ||
    fail "hydrogen commit $hydrogen_commit has no dev/toolchain.lock"
. "$out/toolchain.lock"

llvm_bin=$out/llvm/bin/clang
llvm_stamp=$out/llvm/bin/.build-nanos-llvm.stamp
llvm_rel=llvm/bin/clang
llvm_id=$(llvm_identity | digest)
if [ "$force" = 1 ] || [ ! -s "$llvm_bin" ] || [ ! -x "$llvm_bin" ] || ! stamp_valid "$llvm_stamp" "$llvm_id" "$llvm_rel"; then
    llvm_tar=$out/dl/$LLVM_DIR.tar.xz
    if ! printf '%s  %s\n' "$LLVM_SHA256" "$llvm_tar" | sha256sum -c -s 2>/dev/null; then
        curl --proto '=https' --tlsv1.2 -fsSL "$LLVM_URL" -o "$llvm_tar.part" || fail "cannot fetch $LLVM_URL"
        mv "$llvm_tar.part" "$llvm_tar"
        printf '%s  %s\n' "$LLVM_SHA256" "$llvm_tar" | sha256sum -c -s || fail "$LLVM_RELEASE does not match its pinned SHA-256"
    fi
    rm -rf "$out/llvm"
    mkdir -p "$out/llvm"
    tar -xJf "$llvm_tar" -C "$out/llvm" --strip-components=1
    [ -s "$llvm_bin" ] && [ -x "$llvm_bin" ] || fail "LLVM archive produced no $llvm_bin"
    write_stamp "$llvm_stamp" "$llvm_id" "$llvm_rel"
    echo "llvm: built"
else
    echo "llvm: reused"
fi

echo "kernel $bin/kernel.img, mkfs $mkfs, $vsock_bin, $out/llvm"
