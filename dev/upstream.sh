#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

usage() { die "usage: dev/upstream.sh [--smp N]... [--timeout SECONDS] [--jobs N] [--no-native] [TEST...]"; }

B=$WORK/build/upstream
TESTDIR=$NANOS/test/runtime
BINDIR=$B/bin
IMGDIR=$B/images
NATIVE=$B/native
DEFAULT_TIMEOUT=60
NATIVE_TIMEOUT=20

runtime_tests() {
	awk '
		/^RUNTIME_TESTS\+?=/ { on = 1; sub(/^RUNTIME_TESTS\+?=/, "") }
		on { more = ($0 ~ /\\[ \t]*$/); gsub(/\\/, ""); for (i = 1; i <= NF; i++) print $i; if (!more) on = 0 }
	' "$NANOS/Makefile"
}

native_not_applicable() {
	case $1 in
	ktest) echo 'needs the kernel klib test harness' ;;
	sandbox) echo 'needs the Nanos sandbox klib' ;;
	syslog) echo 'needs the Nanos syslog klib' ;;
	tun) echo 'needs the Nanos tun klib' ;;
	umcg) echo 'needs the Nanos umcg klib' ;;
	fs_full) echo 'fills a fixed-size guest filesystem' ;;
	esac
}

guest_not_run() {
	case $1 in
	esac
}

guest_timeout() {
	case $1 in
	fs_full | time | mmap) echo 120 ;;
	*) echo "$timeout" ;;
	esac
}

guest_net() {
	case $1 in
	netsock | netlink) echo 1 ;;
	*) echo 0 ;;
	esac
}

run_guest() {
	t=$1 smp=$2
	STATE=$B/state
	netflag=''
	[ "$(guest_net "$t")" -eq 0 ] || netflag=--net
	image=$IMGDIR/$t.smp$smp.img
	cp "$IMGDIR/$t.img" "$image"
	line=$("$ROOT/dev/run.sh" --exit-only --smp "$smp" --mem 2G --timeout "$(guest_timeout "$t")" --label "upstream-$t-smp$smp" $netflag "$image" || :)
	rm -f "$image"
	dir=${line##* dir=}
	head=${line%% dir=*}
	head=${head#RUN verdict=}
	case $head in
	PASS*) verdict=PASS detail=exit=0 ;;
	TIMEOUT*) verdict=TIMEOUT detail=${head#TIMEOUT detail=} ;;
	CRASH*) verdict=CRASH detail=${head#CRASH detail=} ;;
	*) verdict=FAIL detail=${head#* detail=} ;;
	esac
	printf '%s %s smp=%s %s log=%s\n' "$verdict" "$t" "$smp" "$detail" "$dir" >"$STATE/$t.smp$smp"
}

prepare_toolchain() {
	[ -x "$LLVM/bin/clang" ] || die "missing $LLVM/bin/clang: run dev/bootstrap.sh"
	[ -d /usr/include/linux ] && [ -d /usr/include/asm ] && [ -d /usr/include/asm-generic ] ||
		die "missing Linux UAPI headers in /usr/include (linux, asm, asm-generic)"
	mkdir -p "$B/uapi" "$B/shim" "$BINDIR" "$IMGDIR" "$NATIVE"
	for d in linux asm asm-generic; do ln -sfn "/usr/include/$d" "$B/uapi/$d"; done
	cat >"$B/shim/endian.h" <<EOF
#define __bswap16 musl_bswap16
#define __bswap32 musl_bswap32
#define __bswap64 musl_bswap64
#include_next <endian.h>
#undef __bswap16
#undef __bswap32
#undef __bswap64
EOF
	cat >"$B/cc" <<EOF
#!/bin/sh
exec $LLVM/bin/clang --sysroot=$SYSROOT -I$B/shim -isystem $B/uapi "\$@" -Wno-unused-but-set-global -Wno-macro-redefined -Wno-shift-negative-value
EOF
	chmod +x "$B/cc"
}

build_tests() {
	: >"$B/build.log"
	for t in "$@"; do rm -f "$NANOS_OUT/test/runtime/bin/$t" "$NANOS_OUT/test/runtime/bin/$t.dbg"; done
	make -k -j"$(nproc)" -C "$TESTDIR" CC="$B/cc" STRIP="$LLVM/bin/llvm-strip" GO="$GOROOT/bin/go" \
		GO_ENV="CGO_ENABLED=0 GOOS=linux GOARCH=amd64" "$@" >>"$B/build.log" 2>&1 || :
	for t in "$@"; do
		if [ -f "$NANOS_OUT/test/runtime/bin/$t" ]; then cp "$NANOS_OUT/test/runtime/bin/$t" "$BINDIR/$t"
		else rm -f "$BINDIR/$t"; fi
	done
}

musl_lib_tree() {
	loader=$(realpath "$SYSROOT/lib/ld-musl-x86_64.so.1")
	printf 'lib:(children:(ld-musl-x86_64.so.1:(contents:(host:%s)) %s:(contents:(host:%s))))' "$loader" "$MUSL_SONAME" "$loader"
}

make_manifest() {
	sed -e "s|output/test/runtime/bin/|$BINDIR/|g" \
		-e "s|ARCH_DIR|$NANOS_OUT/platform/pc|g" \
		-e "s|ld.so.cache:(contents:(host:/etc/ld.so.cache))||g" \
		-e "s|TEST-LIBS|$(musl_lib_tree)|g" \
		"$TESTDIR/$1.manifest"
}

make_image() {
	name=$1 manifest=$2 image=$3
	(cd "$NANOS" && "$NANOS_OUT/tools/bin/mkfs" -k "$NANOS_OUT/platform/pc/bin/kernel.img" \
		-b "$NANOS_OUT/platform/pc/boot/boot.img" -t '(debug_exit:t)' "$image" <"$manifest") >"$image.mkfs.log" 2>&1 ||
		{ cat "$image.mkfs.log" >&2; return 1; }
	rm -f "$image.mkfs.log"
}

manifest_tree() {
	sed 's/#.*//' "$1" | tr '\n' ' ' | awk '
		function enter(kind, dir,    name, path) {
			while (i <= n && tok[i] != ")") {
				name = tok[i]
				path = (dir == "" ? "" : dir "/") name
				if (tok[i + 3] == "contents") {
					printf "F\t%s\t%s\n", path, tok[i + 8]
					i += 11
				} else {
					printf "D\t%s\n", path
					i += 6
					enter(0, path)
					i += 2
				}
			}
		}
		{
			gsub(/\(/, " ( "); gsub(/\)/, " ) "); gsub(/:/, " : ")
			n = split($0, tok, " ")
			depth = 0
			for (i = 1; i <= n; i++) {
				if (tok[i] == "(") depth++
				else if (tok[i] == ")") depth--
				else if (tok[i] == "children" && depth == 1) { i += 3; enter(1, ""); break }
			}
		}
	'
}

manifest_value() {
	sed 's/#.*//' "$2" | tr '\n' ' ' | sed -n "s/.*[ (]$1:\\(.*\\)/\\1/p"
}

manifest_program() { manifest_value program "$1" | sed 's|^/||;s/[ )].*//'; }
manifest_cwd() { manifest_value cwd "$1" | sed 's/[ )].*//'; }
manifest_args() { manifest_value arguments "$1" | sed -n 's/^\[\([^]]*\)\].*/\1/p'; }
manifest_env() { manifest_value environment "$1" | sed -n 's/^(\([^)]*\)).*/\1/p'; }

native_run() {
	name=$1 manifest=$2
	root=$NATIVE/$name
	rm -rf "$root"
	mkdir -p "$root"
	manifest_tree "$manifest" >"$root.tree"
	while IFS='	' read -r kind path host; do
		case $kind in
		D) mkdir -p "$root/$path" ;;
		F) mkdir -p "$(dirname -- "$root/$path")"; cp "$(cd "$NANOS" && realpath "$host")" "$root/$path" ;;
		esac
	done <"$root.tree"
	program=$(manifest_program "$manifest")
	cwd=$(manifest_cwd "$manifest")
	set --
	first=1
	for a in $(manifest_args "$manifest"); do
		[ "$first" -eq 0 ] || case $a in /*) set -- "$@" "$root$a" ;; *) set -- "$@" "$a" ;; esac
		first=0
	done
	envs=''
	for e in $(manifest_env "$manifest"); do
		case ${e%%:*} in PWD) ;; *) envs="$envs ${e%%:*}=${e#*:}" ;; esac
	done
	set +e
	(exec >"$root.log" 2>&1 </dev/null; ulimit -n "$(ulimit -Hn)"; cd "$root${cwd:-}" && env -i $envs PATH=/usr/bin:/bin timeout "$NATIVE_TIMEOUT" "$root/$program" "$@"; exit $?)
	rc=$?
	set -e
	[ "$rc" -eq 0 ] && return 0
	sig=$((rc > 128 ? rc - 128 : rc))
	for want in $(manifest_value expected_exit_code "$manifest" | sed -n 's/^\[\([^]]*\)\].*/\1/p'); do
		[ "$rc" -ne "$want" ] && [ "$sig" -ne "$want" ] || return 0
	done
	printf 'exit=%s\n' "$rc" >>"$root.log"
	return 1
}

if [ "${1:-}" = --run-guest ]; then
	timeout=$4
	run_guest "$2" "$3"
	exit 0
fi

smps='' timeout=$DEFAULT_TIMEOUT native=1 selected='' jobs=8
while [ $# -gt 0 ]; do
	case $1 in
	--smp) smps="$smps $2"; shift 2 ;;
	--timeout) timeout=$2; shift 2 ;;
	--jobs) jobs=$2; shift 2 ;;
	--no-native) native=0; shift ;;
	-*) usage ;;
	*) selected="$selected $1"; shift ;;
	esac
done
[ -n "$smps" ] || smps='1 2'

prepare_toolchain
[ -f "$NANOS_OUT/platform/pc/bin/kernel.img" ] || die "missing kernel: run dev/build.sh"

[ -n "$selected" ] || selected=$(runtime_tests)
build_tests $selected

STATE=$B/state
rm -rf "$STATE"
mkdir -p "$STATE"

for t in $selected; do
	[ -f "$TESTDIR/$t.manifest" ] || die "no manifest for $t"
	manifest=$B/$t.manifest
	make_manifest "$t" >"$manifest"

	nat=N/A natnote=''
	if [ "$native" -eq 1 ]; then
		na=$(native_not_applicable "$t")
		if [ -n "$na" ]; then nat=N/A natnote=$na
		elif [ ! -f "$BINDIR/$t" ]; then nat=N/A natnote='not built'
		elif native_run "$t" "$manifest"; then nat=PASS
		else nat=FAIL natnote="log=$NATIVE/$t.log"; fi
	fi
	printf 'native=%s%s\n' "$nat" "${natnote:+ ($natnote)}" >"$STATE/$t.native"

	reason=$(guest_not_run "$t")
	if [ -z "$reason" ] && [ ! -f "$BINDIR/$t" ]; then reason="build failed (see $B/build.log)"; fi
	if [ -z "$reason" ] && ! make_image "$t" "$manifest" "$IMGDIR/$t.img"; then reason='mkfs failed'; fi
	printf '%s\n' "$reason" >"$STATE/$t.reason"
done

for t in $selected; do
	if [ -z "$(cat "$STATE/$t.reason")" ]; then
		for smp in $smps; do printf '%s %s %s\n' "$t" "$smp" "$timeout"; done
	fi
done | xargs -P "$jobs" -n 3 sh -c 'exec "$0" --run-guest "$@"' "$0"

total=0 passes=0 fails=0 timeouts=0 crashes=0 notrun=0
for t in $selected; do
	reason=$(cat "$STATE/$t.reason")
	nat=$(cat "$STATE/$t.native")
	for smp in $smps; do
		total=$((total + 1))
		if [ -n "$reason" ]; then
			notrun=$((notrun + 1))
			printf 'NOT_RUN %s smp=%s %s log=%s %s\n' "$t" "$smp" "$reason" "$B" "$nat"
			continue
		fi
		line=$(cat "$STATE/$t.smp$smp")
		case $line in
		PASS*) passes=$((passes + 1)) ;;
		FAIL*) fails=$((fails + 1)) ;;
		TIMEOUT*) timeouts=$((timeouts + 1)) ;;
		CRASH*) crashes=$((crashes + 1)) ;;
		esac
		printf '%s %s\n' "$line" "$nat"
	done
done

printf 'upstream: %d runs: %d pass, %d fail, %d timeout, %d crash, %d not run\n' "$total" "$passes" "$fails" "$timeouts" "$crashes" "$notrun"
[ $((fails + timeouts + crashes)) -eq 0 ]
