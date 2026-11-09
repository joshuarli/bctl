#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

usage() {
	die "usage: dev/run.sh [--smp N] [--mem SIZE] [--timeout SECONDS] [--exit CODE] [--expect-test NAME]... [--net] [--label NAME] IMAGE"
}

smp=1 mem=1G timeout=60 want_exit=0 net=0 label='' image=''
expected=$(mktemp "$WORK/tmp/run-expected.XXXXXX")
trap 'rm -f "$expected"' EXIT
while [ $# -gt 0 ]; do
	case $1 in
	--smp) smp=$2; shift 2 ;;
	--mem) mem=$2; shift 2 ;;
	--timeout) timeout=$2; shift 2 ;;
	--exit) want_exit=$2; shift 2 ;;
	--expect-test) printf '%s\n' "$2" >>"$expected"; shift 2 ;;
	--net) net=1; shift ;;
	--label) label=$2; shift 2 ;;
	-*) usage ;;
	*) [ -z "$image" ] || usage; image=$1; shift ;;
	esac
done
[ -n "$image" ] && [ -f "$image" ] || usage
kernel=$NANOS_OUT/platform/pc/bin/kernel.img
[ -f "$kernel" ] || die "missing $kernel: run dev/build.sh"
[ -n "$label" ] || label=$(basename -- "$image" .img)

dir=$WORK/runs/$(date -u +%Y%m%dT%H%M%SZ)-$label-$$
mkdir -p "$dir"
serial=$dir/serial.log

if [ "$net" -eq 1 ]; then nic='user,model=virtio-net-pci'; else nic='none'; fi
set -- qemu-system-x86_64 -machine q35,accel=kvm -cpu host -smp "$smp" -m "$mem" \
	-display none -monitor none -serial "file:$serial" -no-reboot \
	-device isa-debug-exit -device virtio-rng-pci \
	-drive "if=none,id=hd0,format=raw,file=$image" -device virtio-scsi-pci,id=scsi0 -device scsi-hd,bus=scsi0.0,drive=hd0 \
	-nic "$nic" -kernel "$kernel"
qemu_cmd=$*

start=$(cut -d' ' -f1 /proc/uptime)
"$@" >"$dir/qemu.log" 2>&1 &
qpid=$!
( sleep "$timeout" && : >"$dir/deadline" && kill "$qpid" ) >/dev/null 2>&1 &
wpid=$!
set +e
wait "$qpid"
rc=$?
set -e
kill "$wpid" 2>/dev/null || :
wait "$wpid" 2>/dev/null || :
end=$(cut -d' ' -f1 /proc/uptime)
[ -f "$serial" ] || : >"$serial"
tr -d '\r' <"$serial" >"$dir/serial.txt"
log=$dir/serial.txt

verdict=PASS detail=''
fail() { [ "$verdict" != PASS ] || { verdict=$1; detail=$2; }; }

guest_code=$((rc >> 1))
if [ -f "$dir/deadline" ]; then
	fail TIMEOUT "no guest exit within ${timeout}s"
elif grep -q '^qemu-system-x86_64:' "$dir/qemu.log"; then
	fail QEMU_ERROR "$(grep -m1 '^qemu-system-x86_64:' "$dir/qemu.log")"
elif grep -qE '^\*\*\* signal|^\*\*\* Thread context|assertion .* failed|kernel page fault|^panic' "$log" ||
	{ [ $((rc & 1)) -eq 1 ] && { [ "$guest_code" -eq 126 ] || [ "$guest_code" -eq 127 ]; }; }; then
	fail CRASH "$(grep -m1 -E '^\*\*\* signal|assertion .* failed|kernel page fault|^panic' "$log" || printf 'guest exit %s' "$guest_code")"
elif [ $((rc & 1)) -eq 0 ]; then
	fail FAIL "no-debug-exit (qemu exit $rc)"
elif [ "$guest_code" -ne "$want_exit" ]; then
	fail FAIL "exit=$guest_code want=$want_exit"
fi

results=$(grep -c '^MINI_CHROMIUM_RESULT ' "$log" || :)
tests=$(grep '^MINI_CHROMIUM_TEST ' "$log" || :)
if [ "$results" -eq 0 ]; then fail FAIL missing-record
elif [ "$results" -gt 1 ]; then fail FAIL duplicate-record
fi
if [ -n "$tests" ]; then
	bad=$(printf '%s\n' "$tests" | grep -v ' status=PASS\( \|$\)' | sed -n 's/.* name=\([^ ]*\).* status=\([^ ]*\).*/\1:\2/p' | head -n 1)
	[ -z "$bad" ] || fail FAIL "test=$bad"
	dup=$(printf '%s\n' "$tests" | sed -n 's/.* name=\([^ ]*\).*/\1/p' | sort | uniq -d | head -n 1)
	[ -z "$dup" ] || fail FAIL "duplicate-test=$dup"
fi
while IFS= read -r name; do
	printf '%s\n' "$tests" | grep -q " name=$name status=PASS\( \|$\)" || fail FAIL "missing-test=$name"
done <"$expected"
if [ "$results" -eq 1 ]; then
	line=$(grep '^MINI_CHROMIUM_RESULT ' "$log")
	passes=$(printf '%s\n' "$tests" | grep -c ' status=PASS\( \|$\)' || :)
	case $line in
	*" status=PASS passed=$passes failed=0") ;;
	*) fail FAIL "result-mismatch: $line (saw $passes passing tests)" ;;
	esac
fi

manifest=$image.manifest
program=''
[ -f "$manifest" ] && program=$(sed -n 's/^program:\///p' "$manifest")
binary_sha=''
if [ -n "$program" ]; then
	host=$(sed -n "s/^ *$program:(contents:(host:\([^)]*\)))/\1/p" "$manifest" | head -n 1)
	[ -n "$host" ] && [ -f "$host" ] && binary_sha=$(sha256sum "$host" | cut -d' ' -f1)
fi
dirty=false
[ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ] || dirty=true
reproduce=''
[ -f "$image.reproduce" ] && reproduce="dev/bootstrap.sh && dev/build.sh && make -s -C tests/mini_chromium && $(cat "$image.reproduce") && dev/run.sh --smp $smp --mem $mem --timeout $timeout --exit $want_exit$(sed 's/^/ --expect-test /' "$expected" | tr '\n' ' ' | sed 's/ $//;s/^/ /;s/^ $//') $image"

printf '%s\n' "$tests" | sed -n 's/^MINI_CHROMIUM_TEST name=\([^ ]*\) status=\([^ ]*\)\(.*\)$/\1\t\2\t\3/p' |
	jaq -R -s '[split("\n")[] | select(length > 0) | split("\t") | {name: .[0], status: .[1], detail: (.[2] | ltrimstr(" "))}]' >"$dir/tests.json"
jaq -n \
	--arg verdict "$verdict" --arg detail "$detail" \
	--arg hydrogen "$(git -C "$ROOT" rev-parse HEAD)" --argjson dirty "$dirty" \
	--arg kernel_sha256 "$(sha256sum "$kernel" | cut -d' ' -f1)" \
	--arg image_sha256 "$(sha256sum "$image" | cut -d' ' -f1)" \
	--arg binary_sha256 "$binary_sha" --arg program "$program" \
	--arg gcc "$HOST_GCC_VERSION" --arg qemu "$HOST_QEMU_VERSION" --arg llvm "$LLVM_RELEASE" --arg musl "$MUSL_VERSION" \
	--arg qemu_cmd "$qemu_cmd" --argjson qemu_exit "$rc" --argjson guest_exit "$guest_code" \
	--arg elapsed "$(awk -v a="$start" -v b="$end" 'BEGIN { printf "%.2f", b - a }')" \
	--arg reproduce "$reproduce" --slurpfile tests "$dir/tests.json" \
	'{verdict: $verdict, detail: $detail, hydrogen: $hydrogen, dirty: $dirty,
	  kernel_sha256: $kernel_sha256, image_sha256: $image_sha256, program: $program, binary_sha256: $binary_sha256,
	  toolchain: {gcc: $gcc, qemu: $qemu, llvm: $llvm, musl: $musl},
	  qemu: {command: $qemu_cmd, exit: $qemu_exit, guest_exit: $guest_exit},
	  elapsed_seconds: ($elapsed | tonumber), reproduce: $reproduce, tests: $tests[0]}' >"$dir/result.json"
rm -f "$dir/tests.json"

printf 'RUN verdict=%s%s dir=%s\n' "$verdict" "${detail:+ detail=$detail}" "$dir"
[ "$verdict" = PASS ]
