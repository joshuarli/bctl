#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

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
make -C "$src" -j"$(nproc)" kernel tools CC="ccache gcc" ${kconfig:+KCONFIG=$kconfig} >"$WORK/tmp/build.log" 2>&1 || { tail -n 30 "$WORK/tmp/build.log" >&2; die "build failed; full log in $WORK/tmp/build.log"; }

out=$src/output
bin=$out/platform/pc/bin
for f in "$bin/kernel.img" "$bin/kernel.elf" "$out/platform/pc/boot/boot.img" "$out/tools/bin/mkfs"; do
	[ -f "$f" ] || die "missing build product $f"
done
sha256sum "$bin/kernel.img" "$bin/kernel.elf" "$out/platform/pc/boot/boot.img"
