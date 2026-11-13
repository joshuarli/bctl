#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
script=$root/runtime/build-nanos.sh
out=$root/.work/runtime/nanos
tmp=$root/.work/tmp/build-nanos-cache
patches=$(sed -n 's/^vhost_vsock_patches="\(.*\)"$/\1/p' "$script")
patch=${patches%% *}

fail() { echo "tests/build/build-nanos-cache.sh: $*" >&2; exit 1; }
run() { output=$("$@" 2>&1) || fail "$* failed:
$output"; }
show() { echo "== $1"; printf '%s\n' "$output" | grep -E '^(kernel|vhost-device-vsock|llvm): '; }
expect() {
    for line in "kernel: $1" "vhost-device-vsock: $2" "llvm: $3"; do
        printf '%s\n' "$output" | grep -qxF "$line" || fail "want '$line' in:
$output"
    done
}

[ -n "$patch" ] || fail "cannot read vhost_vsock_patches from $script"
[ -z "$(git -C "$root" status --porcelain -- kernel runtime)" ] || fail "commit kernel/ and runtime/ first; the builder reads the committed tree"

rm -rf "$tmp"
mkdir -p "$tmp"

run env FORCE=1 sh "$script"
show "cold"
expect built built built

run env FORCE=0 sh "$script"
show "unchanged"
expect reused reused reused

rm -f "$out/hydrogen/nanos/output/platform/pc/bin/kernel.img"
run env FORCE=0 sh "$script"
show "kernel.img deleted"
expect built reused reused

: >"$out/bin/vhost-device-vsock"
run env FORCE=0 sh "$script"
show "vhost-device-vsock truncated"
expect reused built reused

scratch=$tmp/patch
mkdir -p "$scratch/runtime/patches"
cp "$script" "$scratch/runtime/build-nanos.sh"
for p in $patches; do cp "$root/runtime/patches/$p" "$scratch/runtime/patches/$p"; done
ln -s "$root/.work" "$scratch/.work"
sed -i 's/send no more data/send no more data./' "$scratch/runtime/patches/$patch"
run env FORCE=0 sh "$scratch/runtime/build-nanos.sh"
show "vsock patch changed"
expect reused built reused

run env FORCE=0 sh "$script"
show "vsock patch restored"
expect reused built reused

changed=$tmp/changed
git clone -q --shared "$root" "$changed"
printf 'build-nanos-cache\n' >"$changed/kernel/.build-nanos-cache"
git -C "$changed" add kernel/.build-nanos-cache
git -C "$changed" -c user.name=build-nanos-cache -c user.email=build-nanos-cache@example.invalid commit -q -m 'test: change the kernel tree'
ln -s "$root/.work" "$changed/.work"
run env FORCE=0 sh "$changed/runtime/build-nanos.sh"
show "kernel tree changed"
expect built reused reused

compiler=$tmp/compiler
mkdir -p "$compiler"
real_gcc=$(command -v gcc)
{
    echo '#!/bin/sh'
    echo 'if [ "${1:-}" = --version ]; then'
    echo "    echo 'gcc (GCC) 15.2.99 build-nanos-cache'"
    echo '    exit 0'
    echo 'fi'
    printf 'exec %s "$@"\n' "$real_gcc"
} >"$compiler/gcc"
chmod +x "$compiler/gcc"
run env FORCE=0 PATH="$compiler:$PATH" sh "$changed/runtime/build-nanos.sh"
show "kernel compiler changed"
expect built reused reused

run env FORCE=0 sh "$script"
show "kernel tree and compiler restored"
expect built reused reused

[ -s "$out/hydrogen/nanos/output/platform/pc/bin/kernel.img" ] || fail "no kernel.img at the end"
[ -x "$out/bin/vhost-device-vsock" ] || fail "no vhost-device-vsock at the end"
[ -x "$out/llvm/bin/clang" ] || fail "no llvm/bin/clang at the end"
echo "build-nanos cache checks passed"
