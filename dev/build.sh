#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

if [ "$(uname -s)" = Darwin ]; then
	[ -n "${HYDROGEN_KRUN:-}" ] && [ -f "$HYDROGEN_KRUN/lib/libkrun.dylib" ] && [ -f "$HYDROGEN_KRUN/firmware.fd" ] ||
		die "set HYDROGEN_KRUN to a libkrun bundle holding lib/libkrun.dylib and firmware.fd"
	ensure_work
	ensure_builder
	linux "$ROOT/dev/build.sh" "$@"
	mkdir -p "$WORK/build"
	clang -O2 -Wall -Wextra -Werror "$ROOT/dev/krun-run.c" -L "$HYDROGEN_KRUN/lib" -lkrun \
		-Wl,-rpath,"$HYDROGEN_KRUN/lib" -o "$KRUN_RUN" || die "cannot build $KRUN_RUN"
	codesign --force --sign - --entitlements "$ROOT/dev/hypervisor.entitlements" "$KRUN_RUN" 2>/dev/null ||
		die "cannot sign $KRUN_RUN"
	exit 0
fi

src=$NANOS kconfig=''
case ${1:-} in
--clean) rm -rf "$NANOS_OUT" ;;
--debug)
	src=$NANOS_DEBUG kconfig=no-kaslr
	mkdir -p "$src"
	tar -C "$NANOS" --exclude=./output -cf - . | tar -C "$src" -xf -
	;;
'') ;;
*) die "usage: dev/build.sh [--clean | --debug]" ;;
esac

ensure_work
src=$(realpath "$src")
make -C "$src" -j"$(nproc)" kernel tools CC="ccache gcc" ${kconfig:+KCONFIG=$kconfig} >"$WORK/tmp/build.log" 2>&1 || { tail -n 30 "$WORK/tmp/build.log" >&2; die "build failed; full log in $WORK/tmp/build.log"; }

out=$src/output
case $(uname -m) in
aarch64 | arm64)
	platform=virt
	boot=$out/platform/virt/boot-stub.img
	uefi=$out/platform/virt/boot/bootaa64.efi
	make -s -C "$src/platform/virt" "$boot" || die "cannot build the ARM boot stub"
	;;
x86_64) platform=pc; boot=$out/platform/pc/boot/boot.img; uefi='' ;;
*) die "unsupported architecture: $(uname -m)" ;;
esac
bin=$out/platform/$platform/bin
for f in "$bin/kernel.img" "$bin/kernel.elf" "$boot" ${uefi:+"$uefi"} "$out/tools/bin/mkfs"; do
	[ -f "$f" ] || die "missing build product $f"
done
sha256sum "$bin/kernel.img" "$bin/kernel.elf" "$boot" ${uefi:+"$uefi"}
