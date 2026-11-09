#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

usage() { die "usage: dev/test.sh harness|debug|metrics|probe-native|probe..."; }

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
	writev_red='console writev completes iovecs with 0 bytes, so musl stdio loops'
	for smp in 1 2; do
		case_verdict hello_static "$smp" 20 PASS '' "$FIXTURES/hello_static"
		case_verdict hello_stdio "$smp" 8 PASS "$writev_red" "$FIXTURES/hello_stdio"
		case_verdict hello_pthread "$smp" 8 PASS "$writev_red" "$FIXTURES/hello_pthread"
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

PROBE=$WORK/build/mini_chromium/mini_chromium

probe_native() {
	line=$("$ROOT/dev/run.sh" --native --timeout 120 --label probe "$PROBE" || :)
	dir=${line##* dir=}
	case $line in
	"RUN verdict=PASS "*) printf 'PASS      probe              native  %s passing checks\n' "$(jaq '.tests | length' "$dir/result.json")" ;;
	*) failures=$((failures + 1)); printf 'FAIL      probe              native  %s\n          log: %s\n' "${line%% dir=*}" "$dir" ;;
	esac
	probe_names=$(jaq -r '.tests[].name' "$dir/result.json")
}

suite_probe() {
	make -s -C "$ROOT/tests/mini_chromium"
	probe_native
	[ -n "$probe_names" ] || { failures=$((failures + 1)); printf 'FAIL      probe              no native check names\n'; return; }
	"$ROOT/dev/image.sh" --klib tmpfs --klib shmem -o "$IMAGES/mini_chromium.img" "$PROBE" >/dev/null
	set --
	for n in $probe_names; do set -- "$@" --expect-test "$n"; done
	for smp in 1 2 4; do
		line=$("$ROOT/dev/run.sh" --smp "$smp" --timeout 180 --label "probe-smp$smp" "$@" "$IMAGES/mini_chromium.img" || :)
		dir=${line##* dir=}
		pass=$(jaq '[.tests[] | select(.status == "PASS")] | length' "$dir/result.json")
		total=$(printf '%s\n' $probe_names | wc -l)
		case $line in
		"RUN verdict=PASS "*) printf 'FAIL      probe              smp=%s  known-red case now passes: delete its known-red marker\n' "$smp"; failures=$((failures + 1)) ;;
		*) printf 'KNOWN-RED probe              smp=%s  %s/%s checks pass on Nanos; no fork, exec, wait4, seccomp or namespaces yet\n          log: %s\n' "$smp" "$pass" "$total" "$dir" ;;
		esac
	done
}

[ $# -ge 1 ] || usage
mkdir -p "$IMAGES"
for suite in "$@"; do
	case $suite in
	harness) suite_harness ;;
	metrics) suite_metrics ;;
	probe-native) make -s -C "$ROOT/tests/mini_chromium"; probe_native ;;
	probe) suite_probe ;;
	debug) "$ROOT/dev/debug.sh" --check || failures=$((failures + 1)) ;;
	*) usage ;;
	esac
done
[ "$failures" -eq 0 ] || { printf 'test: %d failing\n' "$failures"; exit 1; }
printf 'test: ok\n'
