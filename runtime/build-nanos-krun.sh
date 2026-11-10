#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
runtime=$root/.work/runtime
out=$runtime/nanos-krun
next=$runtime/nanos-krun.next
target=$runtime/nanos-krun-build
revision=c9a695a88d6240214fafbfbd438bfc2851d24ccc
firmware_sha=9ba725c245f634c86d9cc0850ddcb60b7efe05c6abb53f7ebabf9cd0b070d3de
source=https://raw.githubusercontent.com/tsirysndr/libkrun/$revision/src/vmm/edk2

fail() { echo "runtime/build-nanos-krun.sh: $*" >&2; exit 1; }

[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || fail "Apple Silicon macOS is required"
[ -x "$runtime/guest/krun-init" ] || fail "run runtime/guest.sh first"
KRUN_INIT_BINARY_PATH="$runtime/guest/krun-init" CARGO_TARGET_DIR="$target" \
    cargo +stable build --locked --manifest-path "$root/third_party/smolvm/libkrun/Cargo.toml" \
    -p libkrun --no-default-features --features blk,net || fail "cannot build libkrun"
rm -rf "$next"
mkdir -p "$next/lib"
cp "$target/debug/libkrun.dylib" "$next/lib/libkrun.dylib"
install_name_tool -id @rpath/libkrun.dylib "$next/lib/libkrun.dylib" || fail "cannot set libkrun install name"
codesign --force --sign - "$next/lib/libkrun.dylib" 2>/dev/null || fail "cannot sign libkrun"
codesign --verify --strict "$next/lib/libkrun.dylib" || fail "invalid libkrun signature"
curl --proto '=https' --tlsv1.2 -fsSL "$source/KRUN_EFI.silent.fd" -o "$next/firmware.fd" || fail "cannot fetch EFI firmware"
printf '%s  %s\n' "$firmware_sha" "$next/firmware.fd" | shasum -a 256 -c --quiet || fail "EFI firmware checksum mismatch"
curl --proto '=https' --tlsv1.2 -fsSL "$source/License.txt" -o "$next/firmware-license.txt" || fail "cannot fetch the firmware license"
clang -Wall -Wextra -Werror -Wno-comment -I "$root/third_party/smolvm/libkrun/include" \
    "$root/runtime/nanos-krun.c" -L "$next/lib" -lkrun -Wl,-rpath,@executable_path/lib -o "$next/nanos-krun" || fail "cannot build nanos-krun"
codesign --force --sign - --entitlements "$root/third_party/smolvm/smolvm.entitlements" "$next/nanos-krun" 2>/dev/null || fail "cannot sign nanos-krun"
codesign --verify --strict "$next/nanos-krun" || fail "invalid nanos-krun signature"
rm -rf "$out"
mv "$next" "$out"
echo "$out/nanos-krun"
