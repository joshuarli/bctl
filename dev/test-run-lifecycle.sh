#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

src=${RUN_SH:-$ROOT/dev/run.sh}
grace=2
failures=0
mkdir -p "$TMPDIR"
box=$(mktemp -d "$TMPDIR/run-lifecycle.XXXXXX")
tracked='' everything=''
cleanup() {
	status=$?
	for p in $everything; do ! alive "$p" || kill -KILL "$p" 2>/dev/null || :; done
	rm -rf "$box"
	exit "$status"
}
alive() {
	kill -0 "$1" 2>/dev/null || return 1
	[ "$(ps -o stat= -p "$1" 2>/dev/null | cut -c1)" != Z ]
}
now() {
	if [ -r /proc/uptime ]; then cut -d' ' -f1 /proc/uptime
	else perl -MTime::HiRes=time -e 'printf "%.2f\n", time'; fi
}
pass() { printf 'PASS %s\n' "$1"; }
fail() { printf 'FAIL %s: %s\n' "$1" "$2"; failures=$((failures + 1)); }
trap cleanup EXIT

mkdir -p "$box/dev" "$box/.work/tmp" "$box/nanos/output/platform/$PLATFORM/bin" "$box/fx/share"
cp "$ROOT/dev/common.sh" "$ROOT/dev/toolchain.lock" "$box/dev/"
cp "$src" "$box/dev/run.sh"
: >"$box/nanos/output/platform/$PLATFORM/bin/kernel.img"
: >"$box/fx/image.img"
run=$box/dev/run.sh
fx=$box/fx

cat >"$fx/ignore-term.sh" <<'EOF'
#!/bin/sh
trap '' TERM
echo $$ >"$1/main.pid"
( trap '' TERM; while :; do sleep 1; done ) &
echo $! >"$1/desc.pid"
while :; do sleep 1; done
EOF
cat >"$fx/virtiofsd" <<'EOF'
#!/bin/sh
trap '' TERM
echo $$ >"$FX/virtiofsd.pid"
while :; do sleep 1; done
EOF
chmod +x "$fx/ignore-term.sh" "$fx/virtiofsd"

# launch NAME LIMIT ARGS...: run the harness in the background, poll up to LIMIT seconds
# (never waiting on it directly), then leave rc/out/err files and $elapsed behind.
launch() {
	l_name=$1 l_limit=$2
	shift 2
	rm -f "$box/$l_name.rc" "$box/$l_name.rpid"
	(
		"$@" >"$box/$l_name.out" 2>"$box/$l_name.err" </dev/null &
		echo $! >"$box/$l_name.rpid"
		r=0
		wait $! || r=$?
		echo $r >"$box/$l_name.rc"
	) &
	l_start=$(now)
}
settle() {
	s_name=$1 s_limit=$2
	i=0
	while [ ! -f "$box/$s_name.rc" ] && [ "$i" -lt $((s_limit * 10)) ]; do sleep 0.1; i=$((i + 1)); done
	elapsed=$(awk -v a="$l_start" -v b="$(now)" 'BEGIN { printf "%.2f", b - a }')
	if [ ! -f "$box/$s_name.rc" ]; then
		kill -KILL "$(cat "$box/$s_name.rpid")" 2>/dev/null || :
		return 1
	fi
	rc=$(cat "$box/$s_name.rc")
}
track() {
	for f in "$@"; do
		[ ! -f "$f" ] || { tracked="$tracked $(cat "$f")"; everything="$everything $(cat "$f")"; }
	done
}
survivors() {
	live=''
	for p in $tracked; do ! alive "$p" || live="$live $p"; done
}
waitfile() {
	i=0
	while [ ! -s "$1" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
	[ -s "$1" ]
}

name=native-ignored-term-timeout
limit=$((1 + grace + 2))
export HYDROGEN_RUN_GRACE=$grace FX=$fx
launch $name "$limit" sh "$run" --native --timeout 1 --label lifecycle "$fx/ignore-term.sh" "$fx"
settled=0
settle $name "$limit" && settled=1
track "$fx/main.pid" "$fx/desc.pid"
survivors
dir=$(sed -n 's/^RUN .* dir=//p' "$box/$name.out" 2>/dev/null | head -n 1)
if [ "$settled" -eq 0 ]; then fail $name "run.sh still running after ${limit}s"
elif [ "$rc" -eq 0 ]; then fail $name "run.sh exited 0"
elif [ -z "$dir" ] || [ ! -f "$dir/result.json" ]; then fail $name "no result.json (out: $(cat "$box/$name.out"))"
elif [ "$(jaq -r .verdict "$dir/result.json")" != TIMEOUT ]; then fail $name "verdict $(jaq -r .verdict "$dir/result.json")"
elif ! grep -q '^RUN verdict=TIMEOUT detail=no guest exit within 1s dir=' "$box/$name.out"; then fail $name "verdict line: $(cat "$box/$name.out")"
elif [ -n "$live" ]; then fail $name "fixture processes alive:$live"
else pass "$name (${elapsed}s <= ${limit}s)"; fi

name=native-term-interrupt
tracked=''
rm -f "$fx/main.pid" "$fx/desc.pid"
launch $name 30 sh "$run" --native --timeout 30 --label lifecycle "$fx/ignore-term.sh" "$fx"
waitfile "$fx/desc.pid" || fail $name "fixture did not start"
track "$fx/main.pid" "$fx/desc.pid"
rpid=$(cat "$box/$name.rpid")
l_start=$(now)
kill -TERM "$rpid"
limit=$((grace + 2))
settled=0
settle $name "$limit" && settled=1
survivors
if [ "$settled" -eq 0 ]; then fail $name "run.sh still running ${limit}s after TERM"
elif [ "$rc" -ne 143 ]; then fail $name "exit status $rc, want 143"
elif [ -n "$live" ]; then fail $name "fixture processes alive:$live"
else pass "$name (${elapsed}s <= ${limit}s)"; fi

name=virtiofsd-never-creates-socket
tracked=''
if [ "$(uname -s)" != Linux ]; then
	printf 'SKIP %s: needs the Linux QEMU path\n' $name
else
	limit=$((5 + grace + 3))
	launch $name "$limit" env VIRTIOFSD="$fx/virtiofsd" sh "$run" --virtiofs "tag:$fx/share" --label lifecycle "$fx/image.img"
	settled=0
	settle $name "$limit" && settled=1
	track "$fx/virtiofsd.pid"
	survivors
	if [ "$settled" -eq 0 ]; then fail $name "run.sh still running after ${limit}s"
	elif [ "$rc" -eq 0 ]; then fail $name "run.sh exited 0"
	elif ! grep -q 'virtiofsd did not start' "$box/$name.err"; then fail $name "no startup error (err: $(cat "$box/$name.err"))"
	elif [ -z "$tracked" ]; then fail $name "fake virtiofsd never ran"
	elif [ -n "$live" ]; then fail $name "helper alive:$live"
	else pass "$name (${elapsed}s <= ${limit}s)"; fi
fi

[ "$failures" -eq 0 ]
