#!/bin/sh
set -u
. "$(dirname -- "$0")/common.sh"

failed=0
bad() { printf 'FAIL %s\n' "$*"; failed=1; }
ok() { printf 'ok   %s\n' "$*"; }

need() {
	if command -v "$1" >/dev/null 2>&1; then ok "$1"; else bad "$1 not found: $2"; fi
}

version_is() {
	name=$1 want=$2 got=$3
	if [ "$got" = "$want" ]; then ok "$name $got"; else bad "$name is '${got:-absent}', toolchain.lock pins $want: $4"; fi
}

for t in git curl sha256sum tar xz make gcc ccache ld objcopy strip nasm qemu-system-x86_64 qemu-img gdb addr2line readelf; do
	need "$t" "install it on the host (Alpine: apk add $t)"
done

command -v gcc >/dev/null 2>&1 && version_is gcc "$HOST_GCC_VERSION" "$(gcc -dumpfullversion)" "the kernel is built with the host gcc"
command -v ld >/dev/null 2>&1 && version_is binutils "$HOST_BINUTILS_VERSION" "$(ld --version | sed -n '1s/.* //p')" "install binutils $HOST_BINUTILS_VERSION"
command -v nasm >/dev/null 2>&1 && version_is nasm "$HOST_NASM_VERSION" "$(nasm -v | sed -n 's/^NASM version \([^ ]*\).*/\1/p')" "install nasm $HOST_NASM_VERSION"
command -v make >/dev/null 2>&1 && version_is make "$HOST_MAKE_VERSION" "$(make --version | sed -n '1s/.* //p')" "install GNU make $HOST_MAKE_VERSION"
command -v ccache >/dev/null 2>&1 && version_is ccache "$HOST_CCACHE_VERSION" "$(ccache --version | sed -n '1s/^ccache version //p')" "install ccache $HOST_CCACHE_VERSION"
command -v qemu-system-x86_64 >/dev/null 2>&1 && version_is qemu "$HOST_QEMU_VERSION" "$(qemu-system-x86_64 --version | sed -n '1s/.*version \([^ ]*\).*/\1/p')" "install qemu $HOST_QEMU_VERSION"

if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ok "/dev/kvm read/write"; else bad "/dev/kvm is not read/write for $(id -un): add the user to the kvm group or fix its mode"; fi

if command -v qemu-system-x86_64 >/dev/null 2>&1; then
	mkdir -p "$WORK/tmp"
	probe=$WORK/tmp/kvm-probe.img
	{ printf '\372\260\061\346\364\364'; head -c 504 /dev/zero; printf '\125\252'; } >"$probe"
	qemu-system-x86_64 -machine pc,accel=kvm -cpu host -m 16M -display none -serial none -monitor none \
		-no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04 -drive file="$probe",format=raw,if=floppy \
		>/dev/null 2>"$WORK/tmp/kvm-probe.err" &
	qpid=$!
	( sleep 10; kill "$qpid" ) >/dev/null 2>&1 &
	wpid=$!
	wait "$qpid"
	rc=$?
	kill "$wpid" 2>/dev/null
	if [ "$rc" -eq 99 ]; then ok "KVM guest executed and exited through isa-debug-exit"
	else bad "KVM probe guest exit $rc (want 99): $(head -c 300 "$WORK/tmp/kvm-probe.err" | tr '\n' ' ')"; fi
fi

[ -x "$LLVM/bin/clang" ] && ok "guest clang $LLVM_RELEASE" || bad "guest clang missing: run dev/bootstrap.sh"
[ -f "$SYSROOT/.musl-$MUSL_VERSION" ] && ok "guest sysroot musl $MUSL_VERSION" || bad "guest musl sysroot missing: run dev/bootstrap.sh"
if [ -x "$GOROOT/bin/go" ] && [ "$("$GOROOT/bin/go" env GOVERSION 2>/dev/null)" = "$GO_VERSION" ]; then ok "go $GO_VERSION"; else bad "go $GO_VERSION missing: run dev/bootstrap.sh"; fi

if command -v make >/dev/null 2>&1 && command -v git >/dev/null 2>&1; then
	if out=$(make -s -C "$NANOS" vendor-check 2>&1); then ok "vendored kernel sources at their pinned commits"
	else bad "vendored kernel sources: $(printf '%s' "$out" | tail -n 1)"; fi
fi

[ "$failed" -eq 0 ] && { printf 'doctor: all checks passed\n'; exit 0; }
printf 'doctor: some checks failed\n'
exit 1
