#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
script=$root/runtime/build-nanos.sh
out=$root/.work/runtime/nanos
hydrogen=${HYDROGEN:-$HOME/d/hydrogen}
tmp=$root/.work/tmp/build-nanos-cache
patch=$(sed -n 's/^vhost_vsock_patch=//p' "$script")
pin=$(sed -n 's/^hydrogen_commit=//p' "$script")

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

[ -n "$patch" ] || fail "cannot read vhost_vsock_patch from $script"
[ -n "$pin" ] || fail "cannot read the pinned hydrogen commit from $script"

rm -rf "$tmp"
mkdir -p "$tmp"

run env FORCE=1 HYDROGEN="$hydrogen" sh "$script"
show "cold"
expect built built built

run env FORCE=0 HYDROGEN="$hydrogen" sh "$script"
show "unchanged"
expect reused reused reused

rm -f "$out/hydrogen/nanos/output/platform/pc/bin/kernel.img"
run env FORCE=0 HYDROGEN="$hydrogen" sh "$script"
show "kernel.img deleted"
expect built reused reused

: >"$out/bin/vhost-device-vsock"
run env FORCE=0 HYDROGEN="$hydrogen" sh "$script"
show "vhost-device-vsock truncated"
expect reused built reused

scratch=$tmp/patch
mkdir -p "$scratch/runtime/patches"
cp "$script" "$scratch/runtime/build-nanos.sh"
cp "$root/runtime/patches/$patch" "$scratch/runtime/patches/$patch"
ln -s "$root/.work" "$scratch/.work"
sed -i 's/send no more data/send no more data./' "$scratch/runtime/patches/$patch"
run env FORCE=0 HYDROGEN="$hydrogen" sh "$scratch/runtime/build-nanos.sh"
show "vsock patch changed"
expect reused built reused

run env FORCE=0 HYDROGEN="$hydrogen" sh "$script"
show "vsock patch restored"
expect reused built reused

clone=$tmp/hydrogen
git clone -q --shared --no-checkout "$hydrogen" "$clone"
tree=$(git -C "$clone" rev-parse "$pin^{tree}")
new=$(printf 'build-nanos-cache\n' | git -C "$clone" -c user.name=build-nanos-cache -c user.email=build-nanos-cache@example.invalid commit-tree "$tree" -p "$pin")
repinned=$tmp/repinned
mkdir -p "$repinned/runtime/patches"
sed "s/^hydrogen_commit=.*/hydrogen_commit=$new/" "$script" >"$repinned/runtime/build-nanos.sh"
cp "$root/runtime/patches/$patch" "$repinned/runtime/patches/$patch"
ln -s "$root/.work" "$repinned/.work"
run env FORCE=0 HYDROGEN="$clone" sh "$repinned/runtime/build-nanos.sh"
show "hydrogen commit changed"
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
run env FORCE=0 PATH="$compiler:$PATH" HYDROGEN="$clone" sh "$repinned/runtime/build-nanos.sh"
show "kernel compiler changed"
expect built reused reused

run env FORCE=0 HYDROGEN="$hydrogen" sh "$script"
show "pinned commit and compiler restored"
expect built reused reused

[ -s "$out/hydrogen/nanos/output/platform/pc/bin/kernel.img" ] || fail "no kernel.img at the end"
[ -x "$out/bin/vhost-device-vsock" ] || fail "no vhost-device-vsock at the end"
[ -x "$out/llvm/bin/clang" ] || fail "no llvm/bin/clang at the end"
echo "build-nanos cache checks passed"
