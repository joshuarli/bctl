#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

usage() {
	die "usage: dev/run.sh [--smp N] [--mem SIZE] [--timeout SECONDS] [--exit CODE] [--expect-test NAME]... [--net] [--virtiofs TAG:DIR] [--9p ID:DIR] [--exit-only] [--label NAME] IMAGE
       dev/run.sh --native [--timeout SECONDS] [--exit CODE] [--expect-test NAME]... [--label NAME] BINARY [ARG...]"
}

smp=1 mem=1G timeout=60 want_exit=0 net=0 label='' image='' native=0 exit_only=0 virtiofs='' p9=''
grace=${HYDROGEN_RUN_GRACE:-5}
owned=''
sigall='for p; do kill -s "$sig" -- "-$p" 2>/dev/null || kill -s "$sig" "$p" 2>/dev/null || :; done'
signal_owned() { kill -s "$1" -- "-$2" 2>/dev/null || kill -s "$1" "$2" 2>/dev/null || :; }
spawn() {
	if command -v setsid >/dev/null 2>&1; then setsid "$@" </dev/null &
	elif command -v perl >/dev/null 2>&1; then perl -e 'setpgrp(0, 0); exec @ARGV or exit 127' -- "$@" </dev/null &
	else "$@" </dev/null &
	fi
	owned="$owned $!"
}
reap_owned() {
	trap '' INT TERM HUP
	targets=$owned owned=''
	[ -n "$targets" ] || return 0
	for p in $targets; do signal_owned TERM "$p"; done
	spawn sh -c 'sleep "$1"; shift; sig=KILL; '"$sigall" sh "$grace" $targets >/dev/null 2>&1
	killer=$!
	owned=''
	for p in $targets; do wait "$p" 2>/dev/null || :; done
	i=0
	while [ "$i" -lt $(((grace + 2) * 10)) ]; do
		alive=0
		for p in $targets; do ! kill -0 -- "-$p" 2>/dev/null || alive=1; done
		[ "$alive" -eq 1 ] || break
		sleep 0.1
		i=$((i + 1))
	done
	signal_owned KILL "$killer"
	wait "$killer" 2>/dev/null || :
}
expected=$(mktemp "$WORK/tmp/run-expected.XXXXXX")
cleanup() {
	status=$?
	rm -f "$expected"
	reap_owned
	exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
while [ $# -gt 0 ]; do
	case $1 in
	--smp) smp=$2; shift 2 ;;
	--mem) mem=$2; shift 2 ;;
	--timeout) timeout=$2; shift 2 ;;
	--exit) want_exit=$2; shift 2 ;;
	--expect-test) printf '%s\n' "$2" >>"$expected"; shift 2 ;;
	--net) net=1; shift ;;
	--virtiofs) virtiofs=$2; shift 2 ;;
	--9p) p9=$2; shift 2 ;;
	--label) label=$2; shift 2 ;;
	--native) native=1; shift ;;
	--exit-only) exit_only=1; shift ;;
	-*) usage ;;
	*) image=$1; shift; break ;;
	esac
done
[ -n "$image" ] && [ -f "$image" ] || usage
[ "$native" -eq 1 ] || [ $# -eq 0 ] || usage
[ -z "$virtiofs" ] || [ "$native" -eq 0 ] || usage
[ -z "$virtiofs" ] || [ -d "${virtiofs#*:}" ] || die "no virtiofs directory ${virtiofs#*:}"
if [ -n "$p9" ]; then
	[ "$native" -eq 0 ] && [ "$(uname -s)/$ARCH" = Linux/x86_64 ] || die "--9p requires Linux x86_64 QEMU"
	case ${p9%%:*} in '' | *[!0-9]*) die "9p attachment ID must be numeric" ;; esac
	[ "${p9%%:*}" -le 2147483647 ] || die "9p attachment ID is too large"
	[ "$p9" != "${p9#*:}" ] && [ -d "${p9#*:}" ] || die "--9p requires TAG:DIR"
fi
kernel=$NANOS_OUT/platform/$PLATFORM/bin/kernel.img
[ "$native" -eq 1 ] || [ -f "$kernel" ] || die "missing $kernel: run dev/build.sh"
[ -n "$label" ] || label=$(basename -- "$image" .img)
[ "$native" -eq 0 ] || label=native-$label
hvf=0
[ "$(uname -s)" != Darwin ] || [ "$native" -eq 1 ] || hvf=1
if [ "$hvf" -eq 1 ]; then
	[ "$net" -eq 0 ] || die "--net is not supported under HVF"
	[ -x "$KRUN_RUN" ] || die "missing $KRUN_RUN: run dev/build.sh"
	case $mem in
	*G) mib=$((${mem%G} * 1024)) ;;
	*M) mib=${mem%M} ;;
	*) die "--mem must end in G or M under HVF: $mem" ;;
	esac
fi
now() {
	if [ -r /proc/uptime ]; then cut -d' ' -f1 /proc/uptime
	else perl -MTime::HiRes=time -e 'printf "%.2f\n", time'; fi
}

dir=$WORK/runs/$(date -u +%Y%m%dT%H%M%SZ)-$label-$$
mkdir -p "$dir"
serial=$dir/serial.log

if [ "$native" -eq 1 ]; then
	program_host=$(realpath "$image")
	set -- "$program_host" "$@"
	qemu_cmd=''
	start=$(now)
	spawn sh -c 'cd "$0" && exec "$@"' "$dir" "$@" >"$serial" 2>"$dir/stderr.log"
elif [ "$hvf" -eq 1 ]; then
	set -- "$KRUN_RUN" "$image" "$kernel" "$smp" "$mib"
	[ -z "$virtiofs" ] || set -- "$@" "${virtiofs%%:*}" "$(realpath "${virtiofs#*:}")"
	qemu_cmd=$*
	start=$(now)
	spawn "$@" >"$serial" 2>"$dir/qemu.log"
else
if [ "$net" -eq 1 ]; then nic='user,model=virtio-net-pci'; else nic='none'; fi
machine=q35,accel=kvm
if [ -n "$virtiofs" ]; then
	command -v "$VIRTIOFSD" >/dev/null || die "missing $VIRTIOFSD: set VIRTIOFSD to a virtiofsd binary"
	vfs_socket=$dir/virtiofs.sock
	spawn "$VIRTIOFSD" --socket-path="$vfs_socket" --shared-dir="$(realpath "${virtiofs#*:}")" --sandbox=none \
		--cache=auto >"$dir/virtiofsd.log" 2>&1
	i=0
	while [ ! -S "$vfs_socket" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
	[ -S "$vfs_socket" ] || die "virtiofsd did not start; see $dir/virtiofsd.log"
	machine=$machine,memory-backend=mem
fi
set -- qemu-system-x86_64 -machine "$machine" -cpu host -smp "$smp" -m "$mem" \
	-display none -monitor none -serial "file:$serial" -no-reboot \
	-device isa-debug-exit -device virtio-rng-pci -device virtio-balloon-pci,free-page-reporting=on,vectors=5 \
	-drive "if=none,id=hd0,format=raw,file=$image" -device virtio-scsi-pci,id=scsi0 -device scsi-hd,bus=scsi0.0,drive=hd0 \
	-nic "$nic" -kernel "$kernel"
[ -z "$virtiofs" ] || set -- "$@" -object "memory-backend-memfd,id=mem,size=$mem,share=on" \
	-chardev "socket,id=vfs,path=$vfs_socket" -device "vhost-user-fs-pci,chardev=vfs,tag=${virtiofs%%:*}"
[ -z "$p9" ] || set -- "$@" -fsdev "local,id=p9,path=$(realpath "${p9#*:}" | sed 's/,/,,/g'),security_model=none" \
	-device "virtio-9p-pci,fsdev=p9,mount_tag=${p9%%:*}"
qemu_cmd=$*

start=$(now)
spawn "$@" >"$dir/qemu.log" 2>&1
fi
qpid=$!
spawn sh -c 'sleep "$1"; : >"$2"; g=$3; shift 3; sig=TERM; '"$sigall"'; sleep "$g"; sig=KILL; '"$sigall" \
	sh "$timeout" "$dir/deadline" "$grace" "$qpid" >/dev/null 2>&1
set +e
wait "$qpid"
rc=$?
set -e
end=$(now)
reap_owned
[ -f "$serial" ] || : >"$serial"
tr -d '\r' <"$serial" >"$dir/serial.txt"
log=$dir/serial.txt

verdict=PASS detail=''
fail() { [ "$verdict" != PASS ] || { verdict=$1; detail=$2; }; }

if [ "$hvf" -eq 1 ] && [ "$rc" -eq 0 ]; then
	code=$(sed -n 's/^Angel shutdown trap failed; shutting down with PSCI, exit code \([0-9]*\)\.$/\1/p' "$log" | tail -n 1)
	[ -z "$code" ] || rc=$((code * 2 + 1))
fi
guest_code=$((rc >> 1))
[ "$native" -eq 0 ] || guest_code=$rc
if [ -f "$dir/deadline" ]; then
	fail TIMEOUT "no guest exit within ${timeout}s"
elif [ "$native" -eq 1 ]; then
	if [ "$rc" -gt 128 ]; then fail CRASH "killed by signal $((rc - 128))"
	elif [ "$rc" -ne "$want_exit" ]; then fail FAIL "exit=$rc want=$want_exit"; fi
elif qemu_error=$(grep -E '^(qemu-system-x86_64|qemu|krun-run):' "$dir/qemu.log" |
	grep -vE '^qemu(-system-x86_64)?: warning:' | head -n 1) && [ -n "$qemu_error" ]; then
	fail QEMU_ERROR "$qemu_error"
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
if [ "$exit_only" -eq 1 ]; then :
elif [ "$results" -eq 0 ]; then fail FAIL missing-record
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
[ "$native" -eq 1 ] || [ ! -f "$manifest" ] || program=$(sed -n 's/^program:\///p' "$manifest")
binary_sha='' host=''
if [ "$native" -eq 1 ]; then
	program=$(basename -- "$image") host=$program_host
	binary_sha=$(sha256sum "$host" | cut -d' ' -f1)
elif [ -n "$program" ]; then
	host=$(sed -n "s/^ *\"$program\":(contents:(host:\"\([^\"]*\)\"))/\1/p" "$manifest" | head -n 1)
	[ -n "$host" ] && [ -f "$host" ] && binary_sha=$(sha256sum "$host" | cut -d' ' -f1)
fi

symbolize() {
	elf=${kernel%.img}.elf
	offset=$(sed -n 's/^kernel load offset \([0-9a-f]*\)$/\1/p' "$log" | head -n 1)
	awk '/^ *(rip|elr): / { print $2 } /^frame trace:/ { f = 1; next } /^kernel load offset/ { f = 0 } f && /^[0-9a-f]+: / { print $2 }' "$log" | while read -r addr; do
		case $addr in
		ffff*)
			[ -n "$offset" ] && [ -f "$elf" ] || { printf '0x%s kernel (no load offset or kernel.elf)\n' "$addr"; continue; }
			printf '0x%s %s\n' "$addr" "$(linux addr2line -f -i -p -e "$elf" "$(printf '0x%x' $((0x$addr - 0x$offset)))")"
			;;
		*)
			[ -n "$host" ] && [ -f "$host" ] || { printf '0x%s user (no program binary)\n' "$addr"; continue; }
			printf '0x%s %s\n' "$addr" "$(linux addr2line -f -i -p -e "$host" "0x$addr")"
			;;
		esac
	done
}
if [ "$verdict" = CRASH ]; then symbolize >"$dir/frames.txt"; else : >"$dir/frames.txt"; fi
dirty=false
[ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ] || dirty=true
reproduce=''
[ -f "$image.reproduce" ] && reproduce="dev/bootstrap.sh && dev/build.sh && make -s -C tests/mini_chromium && $(cat "$image.reproduce") && dev/run.sh --smp $smp --mem $mem --timeout $timeout --exit $want_exit$(sed 's/^/ --expect-test /' "$expected" | tr '\n' ' ' | sed 's/ $//;s/^/ /;s/^ $//') $image"

printf '%s\n' "$tests" | sed -n 's/^MINI_CHROMIUM_TEST name=\([^ ]*\) status=\([^ ]*\)\(.*\)$/\1\t\2\t\3/p' |
	jaq -R -s '[split("\n")[] | select(length > 0) | split("\t") | {name: .[0], status: .[1], detail: (.[2] | ltrimstr(" ") | ltrimstr("detail="))}]' >"$dir/tests.json"
jaq -n \
	--arg verdict "$verdict" --arg detail "$detail" \
	--arg hydrogen "$(git -C "$ROOT" rev-parse HEAD)" --argjson dirty "$dirty" \
	--arg mode "$([ "$native" -eq 1 ] && echo native || echo nanos)" \
	--arg kernel_sha256 "$([ "$native" -eq 1 ] || sha256sum "$kernel" | cut -d' ' -f1)" \
	--arg image_sha256 "$([ "$native" -eq 1 ] || sha256sum "$image" | cut -d' ' -f1)" \
	--arg binary_sha256 "$binary_sha" --arg program "$program" \
	--arg gcc "$HOST_GCC_VERSION" --arg qemu "$HOST_QEMU_VERSION" --arg llvm "$LLVM_RELEASE" --arg musl "$MUSL_VERSION" \
	--arg qemu_cmd "$qemu_cmd" --argjson qemu_exit "$rc" --argjson guest_exit "$guest_code" \
	--arg elapsed "$(awk -v a="$start" -v b="$end" 'BEGIN { printf "%.2f", b - a }')" \
	--arg reproduce "$reproduce" --slurpfile tests "$dir/tests.json" --rawfile frames "$dir/frames.txt" \
	'{mode: $mode, verdict: $verdict, detail: $detail, hydrogen: $hydrogen, dirty: $dirty,
	  kernel_sha256: $kernel_sha256, image_sha256: $image_sha256, program: $program, binary_sha256: $binary_sha256,
	  toolchain: {gcc: $gcc, qemu: $qemu, llvm: $llvm, musl: $musl},
	  qemu: {command: $qemu_cmd, exit: $qemu_exit, guest_exit: $guest_exit},
	  elapsed_seconds: ($elapsed | tonumber), reproduce: $reproduce, tests: $tests[0],
	  frames: ($frames | split("\n") | map(select(length > 0)))}' >"$dir/result.json"
rm -f "$dir/tests.json"

printf 'RUN verdict=%s%s dir=%s\n' "$verdict" "${detail:+ detail=$detail}" "$dir"
[ "$verdict" = PASS ]
