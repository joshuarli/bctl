#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

usage() {
	die "usage: dev/image.sh -o IMAGE [--root DIR] [--lib HOST]... [--file HOST:GUEST]... [--tree HOSTDIR:GUESTDIR]... [--link GUEST:TARGET]... [--dir GUESTDIR]... [--arg ARG]... [--env K=V]... [--klib NAME]... [--size SIZE] [--trace] BINARY"
}

quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }
invocation=dev/image.sh
for a in "$@"; do invocation="$invocation $(quote "$a")"; done

out='' root='' trace=0 binary='' klibs='' size=''
files=$(mktemp "$WORK/tmp/image-files.XXXXXX")
args=$(mktemp "$WORK/tmp/image-args.XXXXXX")
envs=$(mktemp "$WORK/tmp/image-envs.XXXXXX")
trap 'rm -f "$files" "$args" "$envs"' EXIT

while [ $# -gt 0 ]; do
	case $1 in
	-o) out=$2; shift 2 ;;
	--root) root=$2; shift 2 ;;
	--lib) printf '/lib/%s\t%s\n' "$(basename -- "$2")" "$(realpath "$2")" >>"$files"; shift 2 ;;
	--file) printf '%s\t%s\n' "${2#*:}" "$(realpath "${2%%:*}")" >>"$files"; shift 2 ;;
	--tree)
		src=$(realpath "${2%%:*}") dst=${2#*:}
		[ -d "$src" ] || die "--tree source is not a directory: $src"
		find "$src" \( -type f -o -type l \) | while IFS= read -r f; do
			[ -e "$f" ] || continue
			[ ! -d "$f" ] || continue
			printf '%s/%s\t%s\n' "${dst%/}" "${f#"$src"/}" "$(realpath "$f")"
		done >>"$files"
		shift 2 ;;
	--link) printf '%s\tlink:%s\n' "${2%%:*}" "${2#*:}" >>"$files"; shift 2 ;;
	--dir) printf '%s\t\n' "$2" >>"$files"; shift 2 ;;
	--arg) printf '%s\n' "$2" >>"$args"; shift 2 ;;
	--env) printf '%s\n' "$2" >>"$envs"; shift 2 ;;
	--klib) klibs="$klibs $2"; shift 2 ;;
	--size) size=$2; shift 2 ;;
	--trace) trace=1; shift ;;
	-*) usage ;;
	*) [ -z "$binary" ] || usage; binary=$1; shift ;;
	esac
done
[ -n "$out" ] && [ -n "$binary" ] || usage
[ -f "$binary" ] || die "no such binary: $binary"
for f in "$NANOS_OUT/platform/pc/bin/kernel.img" "$NANOS_OUT/platform/pc/boot/boot.img" "$NANOS_OUT/tools/bin/mkfs"; do
	[ -f "$f" ] || die "missing $f: run dev/build.sh"
done

name=$(basename -- "$binary")
printf '/%s\t%s\n' "$name" "$(realpath "$binary")" >>"$files"

resolve_lib() {
	for dir in ${root:+"$root/lib" "$root/usr/lib"} "$SYSROOT/lib"; do
		[ -e "$dir/$1" ] && { realpath "$dir/$1"; return 0; }
	done
	return 1
}

interp=$(readelf -lW "$binary" | sed -n 's/.*\[Requesting program interpreter: \(.*\)\]/\1/p')
if [ -n "$interp" ]; then
	host=$(resolve_lib "$(basename -- "$interp")") || die "interpreter $interp not found in ${root:+$root or }$SYSROOT"
	printf '%s\t%s\n' "$interp" "$host" >>"$files"
	pending=$(realpath "$binary")
	seen=' '
	while [ -n "$pending" ]; do
		next=''
		for elf in $pending; do
			for lib in $(readelf -dW "$elf" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
				case $seen in *" $lib "*) continue ;; esac
				seen="$seen$lib "
				host=$(resolve_lib "$lib") || die "$elf needs $lib, not found in ${root:+$root or }$SYSROOT"
				printf '/lib/%s\t%s\n' "$lib" "$host" >>"$files"
				next="$next $host"
			done
		done
		pending=$next
	done
fi

manifest=$(mktemp "$WORK/tmp/manifest.XXXXXX")
trap 'rm -f "$files" "$args" "$envs" "$manifest"' EXIT
{
	sort -u "$files" | awk -F '\t' '
		function emit(dir, indent,   n, i, k, child, list, path) {
			n = 0
			for (k in kids) if (index(k, dir SUBSEP) == 1) { child = substr(k, length(dir) + 2); list[++n] = child }
			for (i = 1; i <= n; i++) {
				child = list[i]
				delete list[i]
				path = (dir == "" ? "" : dir "/") child
				if (path in host && substr(host[path], 1, 5) == "link:") printf "%s%s:(linktarget:%s)\n", indent, child, substr(host[path], 6)
				else if (path in host && host[path] != "") printf "%s%s:(contents:(host:%s))\n", indent, child, host[path]
				else { printf "%s%s:(children:(\n", indent, child; emit(path, indent "  "); printf "%s))\n", indent }
			}
		}
		{
			p = substr($1, 2)
			if (p in host && host[p] != $2) { print "conflicting sources for /" p > "/dev/stderr"; exit 1 }
			host[p] = $2
			parent = ""
			n = split(p, part, "/")
			for (i = 1; i <= n; i++) { kids[parent SUBSEP part[i]] = 1; parent = (parent == "" ? "" : parent "/") part[i] }
		}
		END { printf "(\nchildren:(\n"; emit("", "  "); printf ")\n" }
	'
	if [ -n "$klibs" ]; then
		printf 'boot:(children:(klib:(children:(\n'
		for k in $klibs; do
			[ -f "$NANOS_OUT/platform/pc/bin/$k" ] || die "no klib $k in $NANOS_OUT/platform/pc/bin"
			printf '  %s:(contents:(host:%s))\n' "$k" "$NANOS_OUT/platform/pc/bin/$k"
		done
		printf '))))\nklibs:bootfs\n'
	fi
	printf 'program:/%s\n' "$name"
	printf 'arguments:[%s' "$name"
	while IFS= read -r a; do printf ' %s' "$a"; done <"$args"
	printf ']\n'
	printf 'environment:(PWD:/'
	while IFS= read -r e; do printf ' %s:%s' "${e%%=*}" "${e#*=}"; done <"$envs"
	printf ')\n'
	printf 'debug_exit:t\nfault:t\n'
	[ "$trace" -eq 0 ] || printf 'trace:t\n'
	printf ')\n'
} >"$manifest"

mkdir -p "$(dirname -- "$out")"
"$NANOS_OUT/tools/bin/mkfs" ${size:+-s "$size"} -k "$NANOS_OUT/platform/pc/bin/kernel.img" -b "$NANOS_OUT/platform/pc/boot/boot.img" "$out" <"$manifest" >"$out.mkfs.log" 2>&1 ||
	{ cat "$out.mkfs.log" >&2; die "mkfs failed"; }
cp "$manifest" "$out.manifest"
printf '%s\n' "$invocation" >"$out.reproduce"
rm -f "$out.mkfs.log"
sha256sum "$out" | cut -d' ' -f1
