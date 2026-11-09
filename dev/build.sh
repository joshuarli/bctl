#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

case ${1:-} in
--clean) rm -rf "$NANOS_OUT" ;;
'') ;;
*) die "usage: dev/build.sh [--clean]" ;;
esac

ensure_work
make -C "$NANOS" -j"$(nproc)" kernel tools >"$WORK/tmp/build.log" 2>&1 || { tail -n 30 "$WORK/tmp/build.log" >&2; die "build failed; full log in $WORK/tmp/build.log"; }

bin=$NANOS_OUT/platform/pc/bin
for f in "$bin/kernel.img" "$bin/kernel.elf" "$NANOS_OUT/platform/pc/boot/boot.img" "$NANOS_OUT/tools/bin/mkfs"; do
	[ -f "$f" ] || die "missing build product $f"
done
sha256sum "$bin/kernel.img" "$bin/kernel.elf" "$NANOS_OUT/platform/pc/boot/boot.img"
