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
mesa_gl_closure="libEGL.so.1 libGLESv2.so.2 libgallium-26.1.6.so libLLVM.so.22.1 libSPIRV-Tools.so libdrm_amdgpu.so.1
libelf.so.1 libwayland-client.so.0 libwayland-egl.so.1 libxcb-present.so.0 libxcb-randr.so.0 libxcb-shm.so.0
libxcb-sync.so.1 libxcb-xfixes.so.0 libxshmfence.so.1 libzstd.so.1"
for library in $mesa_gl_closure; do
    [ -f "$browser/lib/$library" ] || fail "$name has no lib/$library; recheck which libraries only Mesa's GL stack loads"
    rm "$browser/lib/$library"
done
mkdir -p "$rootfs/lib" "$rootfs/usr/local/lib" "$rootfs/usr/local/bin" "$rootfs/home/browser" "$rootfs/etc/bctl"
cp "$xkbcommon" "$rootfs/usr/local/lib/libxkbcommon.so.0"
cp /lib/ld-musl-aarch64.so.1 "$rootfs/lib/"
ln -s ld-musl-aarch64.so.1 "$rootfs/lib/libc.musl-aarch64.so.1"
ln -s /opt/browser/helium "$rootfs/usr/local/bin/browser"

version=$(env -i HOME=/home/browser LD_LIBRARY_PATH=/opt/browser/lib /usr/sbin/chroot "$rootfs" /opt/browser/helium --version) \
    || fail "helium --version failed in the assembled rootfs"
case $version in
    "Helium "*) ;;
    *) fail "helium --version printed $version, not a Helium version" ;;
esac

python3 -I - "$archive" "$version" "$rootfs/etc/bctl/receipt.json" $mesa_gl_closure <<'PY'
import json, sys
from pathlib import Path

archive, version, out, removed = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3]), sys.argv[4:]
report = json.loads(archive.with_name(archive.name + ".acceptance.json").read_text())
receipt = {
    "image": "helium",
    "browser_version": version,
    "archive": archive.name,
    "archive_sha256": report["archive_sha256"],
    "replaced": {"lib/libxkbcommon.so.0": "/usr/local/lib/libxkbcommon.so.0"},
    "removed": [f"lib/{name}" for name in removed],
}
out.write_text(json.dumps(receipt, indent=2) + "\n")
PY
