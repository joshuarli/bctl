#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
pin="$root/third_party/UPSTREAM.lock"
work="$root/.work"
source_dir="$root/third_party/smolvm"
guest_out="$work/runtime/guest"
src="$work/runtime/src/smolvm"
out="$work/runtime/smolvm"

fail() { echo "runtime/build.sh: $*" >&2; exit 1; }
lock() { jaq -r --from toml "$1" "$pin"; }

[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || fail "only Apple Silicon macOS hosts are supported"
command -v jaq >/dev/null || fail "jaq is required"
[ -f "$pin" ] || fail "third_party/UPSTREAM.lock is missing; run scripts/update-smolvm.sh"
[ -f "$source_dir/Cargo.toml" ] || fail "vendored SmolVM source is missing; run scripts/update-smolvm.sh"
virgl=$(brew --prefix virglrenderer-krun 2>/dev/null) || fail "brew install libkrun/krun/virglrenderer-krun"
epoxy=$(brew --prefix libepoxy 2>/dev/null) || fail "brew install libepoxy"
moltenvk=$(brew --prefix molten-vk 2>/dev/null) || fail "brew install molten-vk"
libclang=/Library/Developer/CommandLineTools/usr/lib
[ -f "$libclang/libclang.dylib" ] || fail "Xcode Command Line Tools libclang is required"
toolchain=$(lock .toolchain.init)
rustup target list --installed --toolchain "$toolchain" | grep -qx aarch64-unknown-linux-musl ||
    fail "rustup target add --toolchain $toolchain aarch64-unknown-linux-musl"
rustup run stable rustc --version >/dev/null 2>&1 || fail "rustup toolchain install stable"

mkdir -p "$work/tmp" "$work/runtime" "$work/ld-shim" "$work/cargo-home"
export TMPDIR="$work/tmp"
rust_lld="$(rustc +"$toolchain" --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
[ -x "$rust_lld" ] || fail "rust-lld missing from $toolchain"
ln -sf "$rust_lld" "$work/ld-shim/ld.lld"

# The global cargo config points the musl target at a mold this Mac does not
# have, forces the kache wrapper, and rewrites HOST_CC/HOST_CXX to it; the
# libkrun build runs several nested cargo and cc invocations that break under
# it. A private CARGO_HOME with only the registry linked in, plus an empty
# wrapper named on the command line (which outranks every config file), keeps
# the linkers and compilers this build names.
printf '[net]\nretry = 3\n' > "$work/cargo-home/config.toml"
for dir in registry git; do
    [ -d "$HOME/.cargo/$dir" ] && ln -sfn "$HOME/.cargo/$dir" "$work/cargo-home/$dir"
done
cargo_home="$work/cargo-home"
unset RUSTC_WRAPPER 2>/dev/null || true
unset HOST_CC HOST_CXX CC_KNOWN_WRAPPER_CUSTOM 2>/dev/null || true
cargo_flags() { cargo +"$1" --config 'build.rustc-wrapper=""' "${@:2}"; }

stamp="$work/runtime/.stamp"
rev=$(lock .smolvm.revision)
if [ ! -f "$stamp" ] || [ "$(cat "$stamp")" != "$rev" ]; then
    rm -rf "$src"
    mkdir -p "$work/runtime/src"
    cp -a "$source_dir" "$src"
    echo "$rev" > "$stamp"
fi
rsync -a "$source_dir/" "$src/"

for name in $(lock '.patch | to_entries[] | .value.path'); do
    [ -f "$root/runtime/patches/$name" ] || fail "runtime/patches/$name is missing"
done
tree_index=0
while [ "$tree_index" -lt "$(lock '.patch | length')" ]; do
    name=$(lock ".patch[$tree_index].path")
    tree=$(lock ".patch[$tree_index].tree")
    sha=$(lock ".patch[$tree_index].sha256")
    echo "$sha  $root/runtime/patches/$name" | shasum -a 256 -c --quiet - || fail "runtime/patches/$name differs from the lock"
    if patch -p1 -d "$src/$tree" --dry-run --batch --forward --silent -i "$root/runtime/patches/$name" >/dev/null 2>&1; then
        patch -p1 -d "$src/$tree" --batch --forward --silent -i "$root/runtime/patches/$name"
    elif ! patch -p1 -d "$src/$tree" --dry-run --batch --reverse --silent -i "$root/runtime/patches/$name" >/dev/null 2>&1; then
        fail "runtime/patches/$name applies to neither the pristine nor the patched $tree tree"
    fi
    tree_index=$((tree_index + 1))
done

if [ ! -s "$guest_out/kernel.c" ] || [ ! -x "$guest_out/krun-init" ] || [ ! -d "$guest_out/rootfs" ]; then
    sh "$root/runtime/guest.sh" || fail "cannot build the guest inputs"
fi

cp "$guest_out/kernel.c" "$src/libkrunfw/kernel.c"
(
    cd "$src/libkrunfw"
    SDKROOT=$(xcrun --show-sdk-path) make OS=Darwin ARCH=arm64 CC="$(xcrun --find clang)" >/dev/null
) || fail "cannot build libkrunfw"
[ -s "$src/libkrunfw/libkrunfw.5.dylib" ] || fail "libkrunfw.5.dylib was not built"

(
    cd "$src/libkrun"
    KRUN_INIT_BINARY_PATH="$guest_out/krun-init" \
    CARGO_HOME="$cargo_home" \
    LIBCLANG_PATH="$libclang" \
    PKG_CONFIG_PATH="$virgl/lib/pkgconfig:$epoxy/lib/pkgconfig" \
    LIBRARY_PATH="$virgl/lib:$epoxy/lib:$moltenvk/lib" \
        cargo_flags stable build --release --locked -p libkrun --features "$(lock .libkrun.features)" >/dev/null
) || fail "cannot build libkrun"
[ -s "$src/libkrun/target/release/libkrun.dylib" ] || fail "libkrun.dylib was not built"

rm -rf "$src/lib"
mkdir -p "$src/lib"
cp "$src/libkrun/target/release/libkrun.dylib" "$src/lib/libkrun.dylib"
cp "$src/libkrunfw/libkrunfw.5.dylib" "$src/lib/libkrunfw.5.dylib"
ln -sf libkrunfw.5.dylib "$src/lib/libkrunfw.dylib"
for lib in "libvirglrenderer.1.dylib:$virgl/lib" "libMoltenVK.dylib:$moltenvk/lib" "libepoxy.0.dylib:$epoxy/lib"; do
    ln -sf "${lib#*:}/${lib%%:*}" "$src/lib/${lib%%:*}"
done

(
    cd "$src"
    LIBKRUN_BUNDLE="$src/lib" CARGO_HOME="$cargo_home" \
        cargo_flags stable build --locked --release --bin smolvm >/dev/null
) || fail "cannot build the smolvm CLI"
[ -s "$src/target/release/smolvm" ] || fail "smolvm was not built"

rm -rf "$out.next"
mkdir -p "$out.next/lib"
cp "$src/scripts/smolvm-wrapper.sh" "$out.next/smolvm"
chmod 755 "$out.next/smolvm"
cp "$src/target/release/smolvm" "$out.next/smolvm-bin"
cp "$src/lib/libkrun.dylib" "$src/lib/libkrunfw.5.dylib" "$out.next/lib/"
ln -sf libkrunfw.5.dylib "$out.next/lib/libkrunfw.dylib"

# The GPU dylibs come from Homebrew, so every dependency they name is rewritten
# to a sibling in lib/ (each one must be bundled) and each is re-signed, since
# install_name_tool invalidates the signature.
for lib in libvirglrenderer.1.dylib libMoltenVK.dylib libepoxy.0.dylib; do
    cp -L "$src/lib/$lib" "$out.next/lib/$lib"
done
for lib in libvirglrenderer.1.dylib libMoltenVK.dylib libepoxy.0.dylib; do
    install_name_tool -id "@loader_path/$lib" "$out.next/lib/$lib" 2>/dev/null || true
    for dep in $(otool -L "$out.next/lib/$lib" | tail -n +2 | awk '{print $1}' | grep -E '^(/opt/homebrew|/usr/local)/'); do
        [ -f "$out.next/lib/$(basename "$dep")" ] || fail "$lib depends on $dep, which is not bundled"
        install_name_tool -change "$dep" "@loader_path/$(basename "$dep")" "$out.next/lib/$lib"
    done
    codesign --force --sign - "$out.next/lib/$lib" 2>/dev/null
done

dylib="$out.next/lib/libkrun.dylib"
install_name_tool -id @rpath/libkrun.dylib "$dylib"
install_name_tool -change "$virgl/lib/libvirglrenderer.1.dylib" @loader_path/libvirglrenderer.1.dylib "$dylib" 2>/dev/null || true
install_name_tool -change "$(brew --prefix)/opt/virglrenderer-krun/lib/libvirglrenderer.1.dylib" @loader_path/libvirglrenderer.1.dylib "$dylib" 2>/dev/null || true
codesign --force --sign - "$dylib" 2>/dev/null
codesign --force --sign - "$out.next/lib/libkrunfw.5.dylib" 2>/dev/null

cp -a "$guest_out/rootfs" "$out.next/agent-rootfs"
cp "$guest_out/storage-template.ext4.zst" "$guest_out/overlay-template.ext4.zst" "$out.next/"

codesign --force --sign - --entitlements "$src/smolvm.entitlements" "$out.next/smolvm-bin" 2>/dev/null
codesign --verify --strict "$out.next/smolvm-bin" || fail "the smolvm binary has no valid signature"
codesign --verify --strict "$dylib" || fail "libkrun.dylib has no valid signature"
codesign --verify --strict "$out.next/lib/libkrunfw.5.dylib" || fail "libkrunfw.5.dylib has no valid signature"

for artifact in "$out.next/smolvm-bin" "$dylib" "$out.next/lib/libkrunfw.5.dylib"; do
    [ "$(lipo -archs "$artifact")" = arm64 ] || fail "not arm64: $artifact"
done
for symbol in krun_add_display krun_add_input_device krun_set_display_backend; do
    nm -gU "$dylib" | grep -q " _$symbol\$" || fail "libkrun.dylib lacks $symbol"
done
if otool -L "$dylib" | grep -q /opt/homebrew; then
    fail "libkrun.dylib still references Homebrew paths"
fi
python3 -B - "$src/smolvm.entitlements" "$out.next/smolvm-bin" <<'PY' || fail "smolvm-bin entitlements do not match smolvm.entitlements"
import plistlib, subprocess, sys
expected = plistlib.loads(open(sys.argv[1], "rb").read())
signed = subprocess.run(
    ["codesign", "--display", "--entitlements", "-", "--xml", sys.argv[2]],
    check=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
).stdout
actual = plistlib.loads(signed)
if actual != expected:
    print(f"entitlements differ: {actual} != {expected}", file=sys.stderr)
    sys.exit(1)
if actual.get("com.apple.security.hypervisor") is not True:
    print("the hypervisor entitlement is missing", file=sys.stderr)
    sys.exit(1)
PY
version=$(HOME="$work/tmp/home" "$out.next/smolvm" --version)
[ "$version" = "smolvm $(lock .smolvm.version)" ] || fail "unexpected $version"

rm -rf "$out"
mv "$out.next" "$out"
echo "$out/smolvm ($version, smolvm $(lock .smolvm.revision | cut -c1-12), libkrun $(lock .libkrun.revision | cut -c1-12) + $(lock '.patch | length') patch)"
