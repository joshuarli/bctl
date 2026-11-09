#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

usage() { die "usage: dev/test.sh harness|debug|metrics|probe-native|probe|upstream|g0..."; }

FIXTURES=$WORK/build/mini_chromium/fixtures
IMAGES=$WORK/images
failures=0

case_verdict() {
	name=$1 smp=$2 timeout=$3 want=$4 known_red=$5
	shift 5
	image=$IMAGES/$name.img
	"$ROOT/dev/image.sh" -o "$image" "$@" >/dev/null
	line=$("$ROOT/dev/run.sh" --smp "$smp" --timeout "$timeout" --label "$name-smp$smp" "$image" || :)
	got=${line%% dir=*}
	got=${got#RUN verdict=}
	dir=${line##* dir=}
	case $got in "$want"*) matched=1 ;; *) matched=0 ;; esac
	if [ -n "$known_red" ]; then
		if [ "$matched" -eq 1 ]; then status=FAIL note="known-red case now gives $want: delete its known-red marker"
		else status=KNOWN-RED note="$known_red; got $got"; fi
	elif [ "$matched" -eq 1 ]; then status=PASS note="$got"
	else status=FAIL note="want $want; got $got"; fi
	if [ "$status" = PASS ] && [ -n "${want_frame:-}" ] && ! grep -q "$want_frame" "$dir/frames.txt"; then
		status=FAIL note="no symbolized frame matching '$want_frame' in $dir/frames.txt"
	fi
	[ "$status" != FAIL ] || failures=$((failures + 1))
	printf '%-9s %-18s smp=%s  %s\n' "$status" "$name" "$smp" "$note"
	[ "$status" = PASS ] || printf '          log: %s\n' "$dir"
}

suite_harness() {
	make -s -C "$ROOT/tests/mini_chromium"
	for smp in 1 2; do
		case_verdict hello_static "$smp" 20 PASS '' "$FIXTURES/hello_static"
		case_verdict hello_stdio "$smp" 20 PASS '' "$FIXTURES/hello_stdio"
		case_verdict hello_pthread "$smp" 20 PASS '' "$FIXTURES/hello_pthread"
		case_verdict writev_iov "$smp" 20 PASS '' "$FIXTURES/writev_iov"
		case_verdict wait_nochild "$smp" 20 PASS '' "$FIXTURES/wait_nochild"
		case_verdict low_user_va "$smp" 20 PASS '' "$FIXTURES/low_user_va"
	done
	want_frame=' main at .*fixtures/crash.c:' case_verdict crash 1 20 CRASH '' "$FIXTURES/crash"
	case_verdict silent_exit 1 20 'FAIL detail=missing-record' '' "$FIXTURES/silent_exit"
	case_verdict duplicate_result 1 20 'FAIL detail=duplicate-record' '' "$FIXTURES/duplicate_result"
	case_verdict hang 1 5 TIMEOUT '' "$FIXTURES/hang"
}

suite_metrics() {
	make -s -C "$ROOT/tests/mini_chromium"
	"$ROOT/dev/image.sh" -o "$IMAGES/metrics-hello.img" "$FIXTURES/hello_static" >/dev/null
	"$ROOT/dev/image.sh" -o "$IMAGES/metrics-meminfo.img" "$FIXTURES/meminfo" >/dev/null
	bin=$NANOS_OUT/platform/pc/bin
	printf 'metric kernel_img_bytes %s\n' "$(wc -c <"$bin/kernel.img")"
	printf 'metric kernel_elf_bytes %s\n' "$(wc -c <"$bin/kernel.elf")"
	printf 'metric hello_image_bytes %s\n' "$(wc -c <"$IMAGES/metrics-hello.img")"
	line=$("$ROOT/dev/run.sh" --mem 1G --label metrics-meminfo "$IMAGES/metrics-meminfo.img") || { failures=$((failures + 1)); printf 'FAIL meminfo: %s\n' "$line"; return; }
	printf 'metric guest_meminfo_1G %s\n' "$(jaq -r '.tests[0].detail' "${line##* dir=}/result.json")"
	samples=''
	for i in 1 2 3 4 5 6 7 8 9 10; do
		line=$("$ROOT/dev/run.sh" --label "metrics-boot-$i" "$IMAGES/metrics-hello.img") || { failures=$((failures + 1)); printf 'FAIL boot %s: %s\n' "$i" "$line"; return; }
		samples="$samples $(jaq -r '.elapsed_seconds' "${line##* dir=}/result.json")"
	done
	printf '%s\n' $samples | sort -n | awk '{ v[NR] = $1 } END { printf "metric boot_to_exit_seconds median=%.2f min=%.2f max=%.2f n=%d\n", (v[5] + v[6]) / 2, v[1], v[NR], NR }'
}

PROBES=$WORK/build/mini_chromium

probe_native() {
	bin=$1
	line=$("$ROOT/dev/run.sh" --native --timeout 120 --label "probe-$bin" "$PROBES/$bin" || :)
	dir=${line##* dir=}
	probe_names=$(jaq -r '.tests[].name' "$dir/result.json")
	case $line in
	"RUN verdict=PASS "*) printf 'PASS      %-18s native  %s checks\n' "$bin" "$(printf '%s\n' $probe_names | wc -l)" ;;
	*) failures=$((failures + 1)); printf 'FAIL      %-18s native  %s\n          log: %s\n' "$bin" "${line%% dir=*}" "$dir" ;;
	esac
}

probe_nanos() {
	bin=$1 known_red=$2
	shift 2
	probe_native "$bin"
	[ -n "$probe_names" ] || { failures=$((failures + 1)); printf 'FAIL      %-18s no native check names\n' "$bin"; return; }
	"$ROOT/dev/image.sh" "$@" -o "$IMAGES/$bin.img" "$PROBES/$bin" >/dev/null
	set --
	for n in $probe_names; do set -- "$@" --expect-test "$n"; done
	total=$(printf '%s\n' $probe_names | wc -l)
	for smp in 1 2 4; do
		line=$("$ROOT/dev/run.sh" --smp "$smp" --timeout 180 --label "probe-$bin-smp$smp" "$@" "$IMAGES/$bin.img" || :)
		dir=${line##* dir=}
		pass=$(jaq '[.tests[] | select(.status == "PASS")] | length' "$dir/result.json")
		case $line in
		"RUN verdict=PASS "*)
			if [ -n "$known_red" ]; then status=FAIL note="known-red case now passes: delete its known-red marker"; failures=$((failures + 1))
			else status=PASS note="$pass/$total checks"; fi ;;
		*)
			if [ -n "$known_red" ]; then status=KNOWN-RED note="$pass/$total checks pass; $known_red"
			else status=FAIL note="${line%% dir=*}"; failures=$((failures + 1)); fi ;;
		esac
		printf '%-9s %-18s smp=%s  %s\n' "$status" "$bin" "$smp" "$note"
		[ "$status" = PASS ] || printf '          log: %s\n' "$dir"
	done
}

suite_probe() {
	make -s -C "$ROOT/tests/mini_chromium"
	no_processes='no fork, exec, wait4, seccomp or namespaces yet'
	probe_nanos mini_chromium "$no_processes" --klib tmpfs --klib shmem
	probe_nanos mini_chromium_raw "$no_processes"
}

upstream_known() {
	case $1 in
	filesystem@*) echo 'faccessat2 is missing, so musl faccessat(AT_SYMLINK_NOFOLLOW) fails (links.c:92)' ;;
	io_uring@*) echo 'mmap of the rings does not give EINVAL where expected (io_uring.c:553)' ;;
	mmap@*) echo 'user write fault in memset at an odd address' ;;
	netsock@*) echo 'crash reading the msghdr in sendmsg (netsock_test_msg)' ;;
	sigloop@*) echo 'sigaction rejects SIG_DFL/SIG_IGN without SA_RESTORER, which musl abort() uses' ;;
	signal@*) echo 'silent hang' ;;
	tun@*) echo 'TUNGETIFF does not return EBADFD (tun.c:49)' ;;
	tlbshootdown@2) echo 'futex op 3 (FUTEX_REQUEUE) not implemented, hangs' ;;
	esac
}

suite_upstream() {
	"$ROOT/dev/upstream.sh" >"$WORK/tmp/upstream.txt" 2>&1 || :
	if ! grep -q '^upstream: ' "$WORK/tmp/upstream.txt"; then
		failures=$((failures + 1))
		printf 'FAIL      upstream suite did not finish: %s\n' "$(tail -n 3 "$WORK/tmp/upstream.txt" | tr '\n' ' ')"
		return
	fi
	while read -r verdict name smp rest; do
		case $verdict in PASS | FAIL | TIMEOUT | CRASH | NOT_RUN) ;; *) continue ;; esac
		key=$name@${smp#smp=}
		known=$(upstream_known "$key")
		if [ -n "$known" ] && [ "$verdict" = PASS ]; then status=FAIL note="known-red case now passes: delete its marker"
		elif [ -n "$known" ]; then status=KNOWN-RED note="$verdict; $known"
		elif [ "$verdict" = PASS ]; then status=PASS note=''
		else status=FAIL note="$verdict $rest"; fi
		[ "$status" != FAIL ] || failures=$((failures + 1))
		printf '%-9s upstream %-14s %s  %s\n' "$status" "$name" "$smp" "$note"
	done <"$WORK/tmp/upstream.txt"
	grep '^upstream: ' "$WORK/tmp/upstream.txt"
}

[ $# -ge 1 ] || usage
mkdir -p "$IMAGES"
"$ROOT/dev/build.sh" >/dev/null
for suite in "$@"; do
	case $suite in
	harness) suite_harness ;;
	metrics) suite_metrics ;;
	probe-native) make -s -C "$ROOT/tests/mini_chromium"; probe_native mini_chromium; probe_native mini_chromium_raw ;;
	probe) suite_probe ;;
	upstream) suite_upstream ;;
	g0) suite_harness; suite_upstream; "$ROOT/dev/debug.sh" --check || failures=$((failures + 1)) ;;
	debug) "$ROOT/dev/debug.sh" --check || failures=$((failures + 1)) ;;
	*) usage ;;
	esac
done
[ "$failures" -eq 0 ] || { printf 'test: %d failing\n' "$failures"; exit 1; }
printf 'test: ok\n'
