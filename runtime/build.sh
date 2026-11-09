#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
pins="$root/runtime/runtime.toml"
work="$root/.work"
out="$work/runtime/smolvm"

fail() { echo "runtime/build.sh: $*" >&2; exit 1; }
pin() { jaq -r --from toml "$1" "$pins"; }

[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || fail "only Apple Silicon macOS hosts are supported"
command -v jaq >/dev/null || fail "jaq is required"
virgl=$(brew --prefix virglrenderer-krun 2>/dev/null) || fail "brew install libkrun/krun/virglrenderer-krun"
epoxy=$(brew --prefix libepoxy 2>/dev/null) || fail "brew install libepoxy"
libclang=/Library/Developer/CommandLineTools/usr/lib
[ -f "$libclang/libclang.dylib" ] || fail "Xcode Command Line Tools libclang is required"
init_toolchain=$(pin .toolchain.init)
rustup target list --installed --toolchain "$init_toolchain" | grep -qx aarch64-unknown-linux-musl ||
    fail "rustup target add --toolchain $init_toolchain aarch64-unknown-linux-musl"
rust_lld="$(rustc +"$init_toolchain" --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld"
[ -x "$rust_lld" ] || fail "rust-lld missing from $init_toolchain"

mkdir -p "$work/tmp" "$work/runtime" "$work/ld-shim"
export TMPDIR="$work/tmp"
ln -sf "$rust_lld" "$work/ld-shim/ld.lld"

archive="$work/runtime/smolvm-$(pin .smolvm.version).tar.gz"
if [ ! -f "$archive" ]; then
    curl -fsSL -o "$archive.part" "$(pin .smolvm.url)"
    mv "$archive.part" "$archive"
fi
echo "$(pin .smolvm.sha256)  $archive" | shasum -a 256 -c --quiet - || fail "SmolVM release digest mismatch: $archive"

src="$work/src/libkrun"
revision=$(pin .libkrun.revision)
[ -d "$src/.git" ] || git clone -q "$(pin .libkrun.url)" "$src"
git -C "$src" fetch -q origin "$revision"
git -C "$src" checkout -q --force "$revision"
git -C "$src" clean -qfd -e target
for patch in $(pin '.libkrun.patches[]'); do
    git -C "$src" apply "$root/runtime/patches/$patch"
done

(
    cd "$src"
    LIBCLANG_PATH="$libclang" \
    PKG_CONFIG_PATH="$virgl/lib/pkgconfig" \
    LIBRARY_PATH="$virgl/lib:$epoxy/lib" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=clang \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-Clink-arg=--target=aarch64-unknown-linux-musl -Clink-arg=-fuse-ld=lld -Clink-arg=-B$work/ld-shim" \
        cargo build --release --locked -p libkrun --features "$(pin .libkrun.features)"
)

rm -rf "$out.next"
mkdir -p "$out.next"
tar -xzf "$archive" -C "$out.next" --strip-components=1
dylib="$out.next/lib/libkrun.dylib"
cp "$src/target/release/libkrun.dylib" "$dylib"
install_name_tool -id @rpath/libkrun.dylib "$dylib"
install_name_tool -change "$virgl/lib/libvirglrenderer.1.dylib" @loader_path/libvirglrenderer.1.dylib "$dylib"
install_name_tool -change "$(brew --prefix)/opt/virglrenderer-krun/lib/libvirglrenderer.1.dylib" @loader_path/libvirglrenderer.1.dylib "$dylib"
codesign --force --sign - "$dylib" 2>/dev/null

if otool -L "$dylib" | grep -q /opt/homebrew; then
    fail "libkrun.dylib still references Homebrew paths"
fi
for symbol in krun_add_display krun_add_input_device krun_set_display_backend; do
    nm -gU "$dylib" | grep -q " _$symbol\$" || fail "libkrun.dylib lacks $symbol"
done
version=$("$out.next/smolvm" --version)
[ "$version" = "smolvm $(pin .smolvm.version)" ] || fail "unexpected $version"

rm -rf "$out"
mv "$out.next" "$out"
echo "$out/smolvm ($version, libkrun $(git -C "$src" rev-parse --short HEAD) + $(pin '.libkrun.patches | length') patch)"
