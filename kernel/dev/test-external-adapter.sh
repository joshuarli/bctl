#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

TEST_SH=${TEST_SH:-$ROOT/dev/test.sh}
mkdir -p "$TMPDIR"
fake=$(mktemp -d "$TMPDIR/external-adapter.XXXXXX")
trap 'rm -rf "$fake"' EXIT
mkdir -p "$fake/dev" "$fake/out"

cat >"$fake/dev/image.sh" <<'EOF'
#!/bin/sh
: >"$2"
EOF

cat >"$fake/dev/run.sh" <<'EOF'
#!/bin/sh
native=0 label=''
while [ $# -gt 0 ]; do
	case $1 in
	--native) native=1; shift ;;
	--label) label=$2; shift 2 ;;
	--smp | --timeout) shift 2 ;;
	*) shift ;;
	esac
done
dir=$FAKE_OUT/$label
mkdir -p "$dir"
verdict=PASS detail='' rc=0
tests=''
emit() {
	for t in "$@"; do
		name=${t%%:*} status=${t##*:}
		tests="$tests${tests:+,}{\"name\":\"$name\",\"status\":\"$status\",\"detail\":\"\"}"
	done
}
if [ "$native" -eq 1 ]; then
	emit alpha:PASS beta:PASS gamma:PASS
else
	case $FAKE_SCENARIO in
	empty-crash) verdict=CRASH detail='*** signal 11' rc=1 ;;
	partial-crash) verdict=CRASH detail='*** signal 11' rc=1; emit alpha:PASS ;;
	timeout) verdict=TIMEOUT detail='no guest exit within 120s' rc=1 ;;
	partial-pass) emit alpha:PASS beta:PASS ;;
	complete-pass) emit alpha:PASS beta:PASS gamma:PASS ;;
	known-failure) verdict=FAIL detail='test=gamma:FAIL' rc=1; emit alpha:PASS beta:PASS gamma:FAIL ;;
	known-failure-crash) verdict=CRASH detail='*** signal 11' rc=1; emit alpha:PASS beta:PASS gamma:FAIL ;;
	known-failure-missing) verdict=FAIL detail='test=beta:FAIL' rc=1; emit alpha:PASS beta:FAIL ;;
	regression) verdict=FAIL detail='test=alpha:FAIL' rc=1; emit alpha:FAIL beta:PASS gamma:PASS ;;
	malformed) printf 'not json\n' >"$dir/result.json"; printf 'RUN verdict=CRASH dir=%s\n' "$dir"; exit 1 ;;
	no-result) printf 'RUN verdict=PASS dir=%s\n' "$dir"; exit 0 ;;
	esac
fi
printf '{"verdict":"%s","detail":"%s","tests":[%s]}\n' "$verdict" "$detail" "$tests" >"$dir/result.json"
printf 'RUN verdict=%s%s dir=%s\n' "$verdict" "${detail:+ detail=$detail}" "$dir"
exit "$rc"
EOF
chmod +x "$fake/dev/image.sh" "$fake/dev/run.sh"

eval "$(sed -n '/^EXTERNAL=/,/^\[ \$# -ge 1 \]/p' "$TEST_SH" | sed '$d')"
make() { :; }
ROOT=$fake
IMAGES=$fake/images
EXTERNAL=$fake/external
EXTERNAL_BINS=fake_bin
EXTERNAL_PASSING_ON_NANOS='alpha beta'
export FAKE_OUT=$fake/out
mkdir -p "$IMAGES" "$EXTERNAL"

bad=0
check() {
	scenario=$1 want_failures=$2 pattern=$3
	out=$(FAKE_SCENARIO=$scenario; export FAKE_SCENARIO; failures=0; suite_external; printf 'failures=%s\n' "$failures")
	if printf '%s\n' "$out" | rg -q -e "^failures=$want_failures\$" && printf '%s\n' "$out" | rg -q -e "$pattern"; then
		printf 'ok        %-22s failures=%s\n' "$scenario" "$want_failures"
	else
		bad=$((bad + 1))
		printf 'not ok    %-22s want failures=%s and /%s/\n' "$scenario" "$want_failures" "$pattern"
		printf '%s\n' "$out" | sed 's/^/          | /'
	fi
}

check empty-crash 2 'FAIL .*smp=1  verdict=CRASH'
check partial-crash 2 'FAIL .*smp=2  verdict=CRASH'
check timeout 2 'FAIL .*smp=1  verdict=TIMEOUT'
check partial-pass 2 'FAIL .*smp=1  missing: gamma'
check malformed 2 'FAIL .*smp=1  verdict=none'
check no-result 2 'FAIL .*smp=2  verdict=none'
check known-failure-crash 2 'FAIL .*smp=1  verdict=CRASH'
check known-failure-missing 2 'FAIL .*smp=1  missing: gamma'
check regression 2 'FAIL .*smp=1  regressed: alpha'
check complete-pass 0 'PASS .*smp=2  3/3'
check known-failure 0 'KNOWN-RED .*smp=1  2/3 pass'

[ "$bad" -eq 0 ] || { printf 'test-external-adapter: %d failing\n' "$bad"; exit 1; }
printf 'test-external-adapter: ok\n'
