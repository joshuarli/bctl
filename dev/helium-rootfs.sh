#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

ARCHIVE=${HELIUM_ARCHIVE:-/home/josh/d/chromium-build/.work/artifacts/helium-0.20.3.1-r5/helium-0.20.3.1-r5-linux-x86_64-musl.tar.xz}
ARCHIVE_SHA256=f4b06e80a097587e508982a3c73f043dbd80e3660aab6d47f72648b6ffc36a05
ARCHIVE_ROOT=helium-0.20.3.1-r5-linux-x86_64-musl
MUSL_APK_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/main/x86_64/musl-1.2.6-r3.apk
MUSL_APK_SHA256=e142255d0fa397f14318996bc10378093dcc069dc3dd1b6818c8f28a789c21da

DL=$WORK/dl/apk-3.24.2
OUT=$WORK/helium
ROOTFS=$OUT/rootfs

fetch() {
	url=$1 out=$2
	curl -fsSL --retry 2 -o "$out.part" "$url" || { rm -f "$out.part"; die "download failed: $url"; }
	mv "$out.part" "$out"
}

ensure_work
mkdir -p "$DL" "$OUT"

if [ ! -f "$OUT/archive.ok" ] || ! [ "$(cat "$OUT/archive.ok")" = "$ARCHIVE_SHA256" ]; then
	[ -f "$ARCHIVE" ] || die "missing Helium archive $ARCHIVE"
	printf '%s  %s\n' "$ARCHIVE_SHA256" "$ARCHIVE" | sha256sum -c -s || die "Helium archive sha256 mismatch: $ARCHIVE"
	printf '%s\n' "$ARCHIVE_SHA256" >"$OUT/archive.ok"
fi

musl_apk=$DL/${MUSL_APK_URL##*/}
if ! printf '%s  %s\n' "$MUSL_APK_SHA256" "$musl_apk" | sha256sum -c -s 2>/dev/null; then
	fetch "$MUSL_APK_URL" "$musl_apk"
	printf '%s  %s\n' "$MUSL_APK_SHA256" "$musl_apk" | sha256sum -c -s || { rm -f "$musl_apk"; die "$MUSL_APK_URL does not match its pinned sha256"; }
fi
apk verify "$musl_apk" >/dev/null 2>&1 || { rm -f "$musl_apk"; die "$musl_apk: signature or content check failed"; }

rm -rf "$ROOTFS"
mkdir -p "$ROOTFS"
tar -xzf "$musl_apk" -C "$ROOTFS" 2>/dev/null || tar -xzf "$musl_apk" -C "$ROOTFS" || die "extract failed: $musl_apk"
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

printf 'duplicate libraries removed (identical to /opt/helium/lib): %s\n' "$dedup"
printf 'rootfs: %s\n' "$ROOTFS"
du -sh "$ROOTFS"
