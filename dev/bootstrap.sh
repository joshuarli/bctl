#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

fetch() {
	url=$1 sum=$2 out=$WORK/dl/$(basename -- "$1")
	if sha256_is "$sum" "$out"; then return 0; fi
	curl -fsSL --retry 2 -o "$out.part" "$url" || die "download failed: $url"
	sha256_is "$sum" "$out.part" || { rm -f "$out.part"; die "sha256 mismatch: $url"; }
	mv "$out.part" "$out"
}

ensure_work
if [ "$(uname -s)" = Darwin ]; then
	ensure_builder
	linux "$ROOT/dev/bootstrap.sh"
	exit 0
fi

if [ "$ARCH" = x86_64 ] && [ ! -x "$LLVM/bin/clang" ]; then
	fetch "$LLVM_URL" "$LLVM_SHA256"
	tar -xf "$WORK/dl/$(basename -- "$LLVM_URL")" -C "$TOOLCHAIN"
fi

if [ "$ARCH" = x86_64 ] && [ ! -x "$GOROOT/bin/go" ]; then
	fetch "$GO_URL" "$GO_SHA256"
	rm -rf "$GOROOT"
	tar -xf "$WORK/dl/$(basename -- "$GO_URL")" -C "$TOOLCHAIN"
fi

if [ ! -f "$SYSROOT/.musl-$MUSL_VERSION" ]; then
	fetch "$MUSL_URL" "$MUSL_SHA256"
	src=$WORK/src/musl-$MUSL_VERSION
	rm -rf "$src" "$SYSROOT"
	mkdir -p "$WORK/src"
	tar -xzf "$WORK/dl/$(basename -- "$MUSL_URL")" -C "$WORK/src"
	(
		cd "$src"
		if [ "$ARCH" = x86_64 ]; then
			./configure --quiet --prefix="$SYSROOT" --syslibdir="$SYSROOT/lib" --target=x86_64-linux-musl \
				CC="$LLVM/bin/clang" AR="$LLVM/bin/llvm-ar" RANLIB="$LLVM/bin/llvm-ranlib" \
				LDFLAGS="-Wl,-soname,$MUSL_SONAME"
		else
			./configure --quiet --prefix="$SYSROOT" --syslibdir="$SYSROOT/lib" \
				--enable-wrapper=gcc CC=gcc LDFLAGS="-Wl,-soname,$MUSL_SONAME"
		fi
		make -s -j"$(nproc)"
		make -s install
	)
	ln -sf libc.so "$SYSROOT/lib/$MUSL_SONAME"
	touch "$SYSROOT/.musl-$MUSL_VERSION"
fi

make -s -C "$NANOS" vendor-check
if [ "$ARCH" = x86_64 ]; then
	printf 'bootstrap: ok (llvm %s, musl %s, %s)\n' "$LLVM_RELEASE" "$MUSL_VERSION" "$GO_VERSION"
else
	printf 'bootstrap: ok (gcc %s, musl %s)\n' "$(gcc -dumpfullversion)" "$MUSL_VERSION"
fi
