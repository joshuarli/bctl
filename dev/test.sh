#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

usage() { die "usage: dev/test.sh harness"; }

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
	case_verdict crash 1 20 CRASH '' "$FIXTURES/crash"
	case_verdict silent_exit 1 20 'FAIL detail=missing-record' '' "$FIXTURES/silent_exit"
	case_verdict duplicate_result 1 20 'FAIL detail=duplicate-record' '' "$FIXTURES/duplicate_result"
	case_verdict hang 1 5 TIMEOUT '' "$FIXTURES/hang"
}

[ $# -ge 1 ] || usage
mkdir -p "$IMAGES"
for suite in "$@"; do
	case $suite in
	harness) suite_harness ;;
	*) usage ;;
	esac
done
[ "$failures" -eq 0 ] || { printf 'test: %d failing\n' "$failures"; exit 1; }
printf 'test: ok\n'
