#!/bin/sh
set -eu
archives=${1:?usage: stage-helium.sh ARCHIVE_DIR ROOTFS CHROMIUM_BUILD_SCRIPTS XKBCOMMON}
rootfs=${2:?usage: stage-helium.sh ARCHIVE_DIR ROOTFS CHROMIUM_BUILD_SCRIPTS XKBCOMMON}
scripts=${3:?usage: stage-helium.sh ARCHIVE_DIR ROOTFS CHROMIUM_BUILD_SCRIPTS XKBCOMMON}
xkbcommon=${4:?usage: stage-helium.sh ARCHIVE_DIR ROOTFS CHROMIUM_BUILD_SCRIPTS XKBCOMMON}

fail() { echo "stage-helium.sh: $*" >&2; exit 1; }

set -- "$archives"/*.tar.xz
[ $# = 1 ] && [ -f "$1" ] || fail "want exactly one *.tar.xz in $archives, found: $*"
archive=$1
name=${archive##*/}
case $name in
    helium-*-linux-aarch64-musl.tar.xz) ;;
    *) fail "$name is not named as a Helium linux-aarch64-musl archive" ;;
esac

python3 -I - "$archive" <<'PY'
import hashlib, json, sys
from pathlib import Path

archive = Path(sys.argv[1])
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
revision = re.fullmatch(r"helium-[0-9.]+-r([0-9]+)-linux-aarch64-musl\.tar\.xz", archive.name)
if revision is None or int(revision.group(1)) < 6:
    refuse("archives before r6 report Chrome/ as their DevTools product and crash at start when $HOME cannot be created")
report = json.loads(archive.with_name(archive.name + ".acceptance.json").read_text())
expected = {"product": "helium", "architecture": "arm64", "archive": archive.name, "archive_sha256": actual}
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
cp /lib/ld-musl-aarch64.so.1 "$rootfs/lib/"
ln -s ld-musl-aarch64.so.1 "$rootfs/lib/libc.musl-aarch64.so.1"
ln -s /opt/browser/helium "$rootfs/usr/local/bin/browser"

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
