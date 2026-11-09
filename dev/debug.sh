#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

usage() { die "usage: dev/debug.sh [--smp N] IMAGE | dev/debug.sh --check"; }

smp=1 check=0 image=''
while [ $# -gt 0 ]; do
	case $1 in
	--smp) smp=$2; shift 2 ;;
	--check) check=1; shift ;;
	-*) usage ;;
	*) [ -z "$image" ] || usage; image=$1; shift ;;
	esac
done

"$ROOT/dev/build.sh" --debug >/dev/null
bin=$NANOS_DEBUG/output/platform/pc/bin
if [ "$check" -eq 1 ]; then
	[ -z "$image" ] || usage
	make -s -C "$ROOT/tests/mini_chromium"
	image=$WORK/images/debug-check.img
	"$ROOT/dev/image.sh" -o "$image" "$WORK/build/mini_chromium/fixtures/hello_static" >/dev/null
fi
[ -n "$image" ] && [ -f "$image" ] || usage

kernel_base=$(sed -n 's/^#define KERNEL_BASE[[:space:]]*\(0x[0-9a-f]*\)ull.*/\1/p' "$NANOS/src/x86_64/kernel_machine.h")
link_start=$(nm "$bin/kernel.elf" | sed -n 's/^\([0-9a-f]*\) T START$/\1/p')
[ -n "$kernel_base" ] && [ -n "$link_start" ] || die "cannot derive the kernel load offset"
offset=$(printf '0x%x' $((kernel_base - 0x$link_start)))
symbols="add-symbol-file $bin/kernel.elf -o $offset"
port=$(( 20000 + $$ % 20000 ))
dir=$WORK/runs/$(date -u +%Y%m%dT%H%M%SZ)-debug-$$
mkdir -p "$dir"
set -- qemu-system-x86_64 -machine q35,accel=kvm -cpu host -smp "$smp" -m 1G \
	-display none -monitor none -serial "file:$dir/serial.log" -no-reboot \
	-device isa-debug-exit -device virtio-rng-pci \
	-drive "if=none,id=hd0,format=raw,file=$image" -device virtio-scsi-pci,id=scsi0 -device scsi-hd,bus=scsi0.0,drive=hd0 \
	-nic none -kernel "$bin/kernel.img" -gdb "tcp:127.0.0.1:$port" -S

if [ "$check" -eq 0 ]; then
	printf 'serial: %s\nattach: gdb -nx -ex "%s" -ex "target remote 127.0.0.1:%s"\n' "$dir/serial.log" "$symbols" "$port"
	exec "$@"
fi

"$@" >"$dir/qemu.log" 2>&1 &
qpid=$!
( sleep 60 && kill "$qpid" ) >/dev/null 2>&1 &
wpid=$!
set +e
gdb -nx -batch \
	-ex 'set pagination off' -ex 'set confirm off' \
	-ex "$symbols" \
	-ex "target remote 127.0.0.1:$port" \
	-ex 'hbreak syscall_handler' \
	-ex 'continue' \
	-ex 'bt 4' \
	-ex 'kill' \
	>"$dir/gdb.log" 2>&1
set -e
kill "$qpid" "$wpid" 2>/dev/null || :
wait "$qpid" 2>/dev/null || :
if grep -q '^#0 .*syscall_handler' "$dir/gdb.log"; then
	grep '^#[0-9]' "$dir/gdb.log"
	printf 'debug: breakpoint in syscall_handler reached (%s)\n' "$dir/gdb.log"
else
	cat "$dir/gdb.log" >&2
	die "breakpoint not reached; log in $dir"
fi
