#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

ARCHIVE=${HELIUM_ARCHIVE:-/home/josh/d/chromium-build/.work/artifacts/helium-0.20.3.1-r5/helium-0.20.3.1-r5-linux-x86_64-musl.tar.xz}
ARCHIVE_SHA256=f4b06e80a097587e508982a3c73f043dbd80e3660aab6d47f72648b6ffc36a05
ARCHIVE_ROOT=helium-0.20.3.1-r5-linux-x86_64-musl
MIRROR=https://dl-cdn.alpinelinux.org/alpine/v3.24
MAIN_INDEX_SHA256=e43a5514fbef0c34f74c68992a8d110fd4e525cf1eb7e114f8d93b800407c9d4
COMMUNITY_INDEX_SHA256=470c4f89b42a9118b3d0dfd81215f0b925271b23ee6996eeb2ea44d50e1205ba
ROOTS='musl'

DL=$WORK/dl/apk-3.24.2
OUT=$WORK/helium
ROOTFS=$OUT/rootfs

fetch() {
	url=$1 out=$2
	curl -fsSL --retry 2 -o "$out.part" "$url" || { rm -f "$out.part"; die "download failed: $url"; }
	mv "$out.part" "$out"
}

fetch_index() {
	repo=$1 want=$2 file=$DL/APKINDEX-$1.tar.gz
	if [ ! -f "$file" ] || ! printf '%s  %s\n' "$want" "$file" | sha256sum -c -s; then
		fetch "$MIRROR/$repo/x86_64/APKINDEX.tar.gz" "$file"
		printf '%s  %s\n' "$want" "$file" | sha256sum -c -s || { rm -f "$file"; die "APKINDEX $repo changed upstream: expected sha256 $want"; }
	fi
	mkdir -p "$DL/index-$repo"
	tar -xzf "$file" -C "$DL/index-$repo" APKINDEX
}

CLOSURE_AWK='
BEGIN { RS = ""; FS = "\n" }
{
	name = ""; ver = ""; size = ""; isize = ""; deps = ""; prov = ""
	for (i = 1; i <= NF; i++) {
		k = substr($i, 1, 2); v = substr($i, 3)
		if (k == "P:") name = v
		else if (k == "V:") ver = v
		else if (k == "S:") size = v
		else if (k == "I:") isize = v
		else if (k == "D:") deps = v
		else if (k == "p:") prov = v
	}
	if (name in version) next
	repo[name] = FILENAME; version[name] = ver; csize[name] = size; usize[name] = isize; depends[name] = deps
	provider[name] = name
	n = split(prov, parts, " ")
	for (j = 1; j <= n; j++) {
		p = parts[j]; sub(/[=<>~].*/, "", p)
		if (!(p in provider)) provider[p] = name
	}
}
END {
	nroots = split(ROOTS, queue, " "); head = 1; tail = nroots
	for (i = 1; i <= nroots; i++) if (!(queue[i] in version)) { print "unknown root " queue[i] > "/dev/stderr"; exit 1 }
	while (head <= tail) {
		cur = queue[head++]
		if (cur in seen) continue
		seen[cur] = 1
		n = split(depends[cur], d, " ")
		for (j = 1; j <= n; j++) {
			x = d[j]
			if (x ~ /^!/) continue
			sub(/[=<>~].*/, "", x)
			if (!(x in provider)) { print "unresolved dependency " x " of " cur > "/dev/stderr"; exit 1 }
			queue[++tail] = provider[x]
		}
		order[++cnt] = cur
	}
	for (i = 1; i <= cnt; i++) {
		c = order[i]
		sub(/.*index-/, "", repo[c]); sub(/\/.*/, "", repo[c])
		printf "%s %s %s %s %s\n", repo[c], c, version[c], csize[c], usize[c]
	}
}'

ensure_work
mkdir -p "$DL" "$OUT"

if [ ! -f "$OUT/archive.ok" ] || ! [ "$(cat "$OUT/archive.ok")" = "$ARCHIVE_SHA256" ]; then
	[ -f "$ARCHIVE" ] || die "missing Helium archive $ARCHIVE"
	printf '%s  %s\n' "$ARCHIVE_SHA256" "$ARCHIVE" | sha256sum -c -s || die "Helium archive sha256 mismatch: $ARCHIVE"
	printf '%s\n' "$ARCHIVE_SHA256" >"$OUT/archive.ok"
fi

fetch_index main "$MAIN_INDEX_SHA256"
fetch_index community "$COMMUNITY_INDEX_SHA256"

closure=$OUT/closure.txt
awk -v ROOTS="$ROOTS" "$CLOSURE_AWK" "$DL/index-main/APKINDEX" "$DL/index-community/APKINDEX" >"$closure"

while read -r repo name version size isize; do
	file=$DL/$name-$version.apk
	if [ -f "$file" ] && [ "$(wc -c <"$file")" -eq "$size" ] && apk verify "$file" >/dev/null 2>&1; then continue; fi
	fetch "$MIRROR/$repo/x86_64/$name-$version.apk" "$file"
	[ "$(wc -c <"$file")" -eq "$size" ] || { rm -f "$file"; die "$name-$version.apk: size differs from APKINDEX ($size)"; }
	apk verify "$file" >/dev/null 2>&1 || { rm -f "$file"; die "$name-$version.apk: signature or content check failed"; }
done <"$closure"

rm -rf "$ROOTFS"
mkdir -p "$ROOTFS"
while read -r repo name version size isize; do
	tar -xzf "$DL/$name-$version.apk" -C "$ROOTFS" 2>/dev/null || tar -xzf "$DL/$name-$version.apk" -C "$ROOTFS" || die "extract failed: $name-$version.apk"
done <"$closure"
find "$ROOTFS" -maxdepth 1 -name '.*' -type f -exec rm -f {} +

tar -xJf "$ARCHIVE" -C "$OUT"
mkdir -p "$ROOTFS/opt"
mv "$OUT/$ARCHIVE_ROOT" "$ROOTFS/opt/helium"

sha256sum "$ROOTFS"/opt/helium/lib/* 2>/dev/null | sed 's/ .*//' | sort -u >"$OUT/helium-lib.sha256"
dedup=0
for dir in "$ROOTFS/usr/lib" "$ROOTFS/lib"; do
	[ -d "$dir" ] || continue
	for f in "$dir"/*; do
		[ -f "$f" ] && [ ! -L "$f" ] || continue
		sum=$(sha256sum "$f" | cut -d' ' -f1)
		if grep -qx "$sum" "$OUT/helium-lib.sha256"; then rm -f "$f"; dedup=$((dedup + 1)); fi
	done
	find "$dir" -maxdepth 1 -type l ! -exec test -e {} \; -exec rm -f {} +
done
rm -f "$OUT/helium-lib.sha256"

mkdir -p "$ROOTFS/proc" "$ROOTFS/dev" "$ROOTFS/tmp" "$ROOTFS/sys" "$ROOTFS/run" "$ROOTFS/profile" "$ROOTFS/etc"

printf 'packages: %s\n' "$(wc -l <"$closure")"
printf 'duplicate libraries removed (identical to /opt/helium/lib): %s\n' "$dedup"
printf 'rootfs: %s\n' "$ROOTFS"
du -sh "$ROOTFS"
