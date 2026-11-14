#!/bin/sh
set -eu
archives=${1:?usage: stage-helium.sh ARCHIVE_DIR ROOTFS CHROMIUM_BUILD_SCRIPTS XKBCOMMON ARCH}
rootfs=${2:?usage: stage-helium.sh ARCHIVE_DIR ROOTFS CHROMIUM_BUILD_SCRIPTS XKBCOMMON ARCH}
scripts=${3:?usage: stage-helium.sh ARCHIVE_DIR ROOTFS CHROMIUM_BUILD_SCRIPTS XKBCOMMON ARCH}
xkbcommon=${4:?usage: stage-helium.sh ARCHIVE_DIR ROOTFS CHROMIUM_BUILD_SCRIPTS XKBCOMMON ARCH}
arch=${5:?usage: stage-helium.sh ARCHIVE_DIR ROOTFS CHROMIUM_BUILD_SCRIPTS XKBCOMMON ARCH}

fail() { echo "stage-helium.sh: $*" >&2; exit 1; }

set -- "$archives"/*.tar.xz
[ $# = 1 ] && [ -f "$1" ] || fail "want exactly one *.tar.xz in $archives, found: $*"
archive=$1
name=${archive##*/}
case $arch in
    aarch64) report_arch=arm64 min_revision=6 ;;
    x86_64) report_arch=amd64 min_revision=5 ;;
    *) fail "unsupported architecture $arch" ;;
esac
case $name in
    helium-*-linux-"$arch"-musl.tar.xz) ;;
    *) fail "$name is not named as a Helium linux-$arch-musl archive" ;;
esac

python3 -I - "$archive" "$arch" "$report_arch" "$min_revision" <<'PY'
import hashlib, json, sys
from pathlib import Path

archive = Path(sys.argv[1])
arch, report_arch, min_revision = sys.argv[2], sys.argv[3], int(sys.argv[4])
def refuse(why):
    sys.exit(f"stage-helium.sh: {archive.name}: {why}")

digest = hashlib.sha256()
with archive.open("rb") as stream:
    for chunk in iter(lambda: stream.read(1 << 20), b""):
        digest.update(chunk)
actual = digest.hexdigest()
fields = archive.with_name(archive.name + ".sha256").read_text().split()
if fields[:2] != [actual, archive.name]:
    refuse(f"the .sha256 sidecar {fields[:2]} does not bind digest {actual} and this archive name")
import re
revision = re.fullmatch(rf"helium-[0-9.]+-r([0-9]+)-linux-{arch}-musl\.tar\.xz", archive.name)
if revision is None or int(revision.group(1)) < min_revision:
    refuse(f"archives before r{min_revision} are refused: arm64 r5 reports Chrome/ as its DevTools product and crashes at start when $HOME cannot be created, and amd64 r4 lacks the pwritev2 and sched_getparam sandbox fixes")
report = json.loads(archive.with_name(archive.name + ".acceptance.json").read_text())
expected = {"product": "helium", "architecture": report_arch, "archive": archive.name, "archive_sha256": actual}
for key, value in expected.items():
    if report.get(key) != value:
        refuse(f"the acceptance sidecar has {key}={report.get(key)!r}, not {value!r}")
PY

mkdir -p "$rootfs/opt"
python3 "$scripts/install-runtime.py" --archive "$archive" --verify-extract "$rootfs/opt/browser"

browser=$rootfs/opt/browser
[ -f "$browser/lib/libxkbcommon.so.0" ] || fail "$name ships no lib/libxkbcommon.so.0 to replace"
rm "$browser/lib/libxkbcommon.so.0"
mkdir -p "$rootfs/lib" "$rootfs/usr/local/lib" "$rootfs/usr/local/bin" "$rootfs/home/browser" "$rootfs/etc/bctl"
cp "$xkbcommon" "$rootfs/usr/local/lib/libxkbcommon.so.0"
cp "/lib/ld-musl-$arch.so.1" "$rootfs/lib/"
ln -s "ld-musl-$arch.so.1" "$rootfs/lib/libc.musl-$arch.so.1"
ln -s /opt/browser/helium "$rootfs/usr/local/bin/browser"

mkdir -p "$browser/fontconfig/cache"
cat > "$browser/fontconfig/fonts.conf" <<'EOF'
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir prefix="relative">../fonts</dir>
  <cachedir>/opt/browser/fontconfig/cache</cachedir>
  <cachedir prefix="xdg">fontconfig</cachedir>
  <include ignore_missing="yes">conf.d</include>
</fontconfig>
EOF
find "$browser/fonts" -type d -exec touch -d "@0" {} +
[ ! -e /opt/browser ] || fail "/opt/browser unexpectedly exists in builder"
fc_tmp=$(mktemp -d)
mkdir -p /opt
mv "$browser" /opt/browser
env -i PATH=/usr/bin:/bin HOME="$fc_tmp" LD_LIBRARY_PATH="/opt/browser/lib:/opt/browser:$rootfs/usr/local/lib" \
    FONTCONFIG_FILE=/opt/browser/fontconfig/fonts.conf FONTCONFIG_PATH=/opt/browser/fontconfig \
    /opt/browser/helium --headless --no-sandbox --use-gl=disabled --user-data-dir="$fc_tmp/profile" --dump-dom about:blank >/dev/null 2>&1 \
    || { mv /opt/browser "$browser"; rm -rf "$fc_tmp"; fail "helium failed to warm fontconfig cache"; }
mv /opt/browser "$browser"
rm -rf "$fc_tmp"
[ -n "$(ls -A "$browser/fontconfig/cache" 2>/dev/null)" ] || fail "helium wrote no fontconfig cache"
chmod 0755 "$browser/fontconfig/cache"
chmod 0644 "$browser/fontconfig/cache/"*
find "$browser/fonts" -type d -exec touch -d "@0" {} +

version=$(env -i HOME=/home/browser LD_LIBRARY_PATH=/opt/browser/lib:/opt/browser /usr/sbin/chroot "$rootfs" /opt/browser/helium --version) \
    || fail "helium --version failed in the assembled rootfs"
case $version in
    "Helium "*) ;;
    *) fail "helium --version printed $version, not a Helium version" ;;
esac

python3 -I - "$archive" "$version" "$rootfs/etc/bctl/receipt.json" <<'PY'
import json, sys
from pathlib import Path

archive, version, out = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
report = json.loads(archive.with_name(archive.name + ".acceptance.json").read_text())
receipt = {
    "image": "helium",
    "browser_version": version,
    "archive": archive.name,
    "archive_sha256": report["archive_sha256"],
    "replaced": {"lib/libxkbcommon.so.0": "/usr/local/lib/libxkbcommon.so.0"},
}
out.write_text(json.dumps(receipt, indent=2) + "\n")
PY
