#!/bin/sh
set -eu
. "$(dirname -- "$0")/common.sh"

if [ "$ARCH" = x86_64 ]; then
	ARCHIVE=${HELIUM_ARCHIVE:-/home/josh/d/chromium-build/.work/artifacts/helium-0.20.3.1-r5/helium-0.20.3.1-r5-linux-x86_64-musl.tar.xz}
	ARCHIVE_SHA256=f4b06e80a097587e508982a3c73f043dbd80e3660aab6d47f72648b6ffc36a05
	ARCHIVE_ROOT=helium-0.20.3.1-r5-linux-x86_64-musl
	MUSL_APK_SHA256=e142255d0fa397f14318996bc10378093dcc069dc3dd1b6818c8f28a789c21da
else
	ARCHIVE=${HELIUM_ARCHIVE:-}
	ARCHIVE_SHA256=7289b25b30fa86d41928e63a4210b3006f43ba8da35d9cf5c874b05152215b7a
	ARCHIVE_ROOT=helium-0.20.3.1-r6-linux-aarch64-musl
	MUSL_APK_SHA256=2e17864102278d9049c3a56f1bceed9e8cf287065617e579325123443daed8a5
fi
MUSL_APK_URL=https://dl-cdn.alpinelinux.org/alpine/v3.24/main/$ARCH/musl-1.2.6-r3.apk

case $# in
0) ;;
3)
	ARCHIVE=$1 ARCHIVE_SHA256=$2 ARCHIVE_ROOT=$3
	case $ARCHIVE_SHA256 in '' | *[!0-9a-f]*) die "archive digest must be SHA-256" ;; esac
	[ "${#ARCHIVE_SHA256}" -eq 64 ] || die "archive digest must be SHA-256"
	case $ARCHIVE_ROOT in '' | *[!a-zA-Z0-9._-]*) die "invalid archive root" ;; esac
	[ "${ARCHIVE##*/}" = "$ARCHIVE_ROOT.tar.xz" ] || die "archive root must match the archive name"
	;;
*) die "usage: helium-rootfs.sh [ARCHIVE SHA256 ROOT]" ;;
esac

[ -n "$ARCHIVE" ] || die "set HELIUM_ARCHIVE to helium-0.20.3.1-r6-linux-aarch64-musl.tar.xz"

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

[ -f "$ARCHIVE" ] || die "missing Helium archive $ARCHIVE"
sha256_is "$ARCHIVE_SHA256" "$ARCHIVE" || die "Helium archive sha256 mismatch: $ARCHIVE"

musl_apk=$DL/${MUSL_APK_URL##*/}
if ! sha256_is "$MUSL_APK_SHA256" "$musl_apk"; then
	fetch "$MUSL_APK_URL" "$musl_apk"
	sha256_is "$MUSL_APK_SHA256" "$musl_apk" || { rm -f "$musl_apk"; die "$MUSL_APK_URL does not match its pinned sha256"; }
fi
[ "$ARCH" != x86_64 ] || apk verify "$musl_apk" >/dev/null 2>&1 || { rm -f "$musl_apk"; die "$musl_apk: signature or content check failed"; }

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
