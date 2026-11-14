#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
runtime=$root/.work/runtime
out=$runtime/nanos-krun
next=$runtime/nanos-krun.next
target=$runtime/nanos-krun-build
revision=c9a695a88d6240214fafbfbd438bfc2851d24ccc
runtime=$root/.work/runtime
out=$runtime/nanos-krun
next=$runtime/nanos-krun.next
target=$runtime/nanos-krun-build
kernel=$runtime/nanos-arm64/hydrogen/nanos/output/platform/virt/bin/kernel.img

fail() { echo "runtime/build-nanos-krun.sh: $*" >&2; exit 1; }

[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || fail "Apple Silicon macOS is required"
[ -x "$runtime/guest/krun-init" ] || fail "run runtime/guest.sh first"
[ -s "$kernel" ] || fail "run runtime/build-nanos-arm64.sh first"
KRUN_INIT_BINARY_PATH="$runtime/guest/krun-init" CARGO_TARGET_DIR="$target" \
    cargo +stable build --release --locked --manifest-path "$root/third_party/smolvm/libkrun/Cargo.toml" \
    -p libkrun --no-default-features --features blk,net || fail "cannot build libkrun"
rm -rf "$next"
mkdir -p "$next/lib"
cp "$target/release/libkrun.dylib" "$next/lib/libkrun.dylib"
install_name_tool -id @rpath/libkrun.dylib "$next/lib/libkrun.dylib" || fail "cannot set libkrun install name"
codesign --force --sign - "$next/lib/libkrun.dylib" 2>/dev/null || fail "cannot sign libkrun"
codesign --verify --strict "$next/lib/libkrun.dylib" || fail "invalid libkrun signature"
cp "$kernel" "$next/kernel.img" || fail "cannot copy nanos kernel"
clang -Wall -Wextra -Werror -Wno-comment -I "$root/third_party/smolvm/libkrun/include" \
    "$root/runtime/nanos-krun.c" -L "$next/lib" -lkrun -Wl,-rpath,@executable_path/lib -o "$next/nanos-krun" || fail "cannot build nanos-krun"
codesign --force --sign - --entitlements "$root/third_party/smolvm/smolvm.entitlements" "$next/nanos-krun" 2>/dev/null || fail "cannot sign nanos-krun"
codesign --verify --strict "$next/nanos-krun" || fail "invalid nanos-krun signature"
rm -rf "$out"
mv "$next" "$out"
echo "$out/nanos-krun"
