#!/bin/sh
set -eu

chromium_build_rev=4275db88a4b3883c0316d99e03968a7f9ad9701a
root=$(cd "$(dirname "$0")/../.." && pwd)
runtime=$root/.work/runtime/nanos
tree=$runtime/hydrogen
work=$root/.work/images/nanos-helium
out=$root/.work/images/nanos-helium.img
archives=${BCTL_BROWSER_ARCHIVE:?set BCTL_BROWSER_ARCHIVE to a directory holding one helium-*-linux-x86_64-musl.tar.xz with its sidecars}

fail() { echo "images/nanos-helium/build.sh: $*" >&2; exit 1; }

[ -s "$tree/COMMIT" ] && [ -x "$runtime/llvm/bin/clang" ] || fail "run runtime/build-nanos.sh first"

set -- "$archives"/*.tar.xz
[ $# = 1 ] && [ -f "$1" ] || fail "want exactly one *.tar.xz in $archives, found: $*"
archive=$(realpath "$1")
name=${archive##*/}
case $name in
    helium-*-linux-x86_64-musl.tar.xz) ;;
    *) fail "$name is not named as a Helium linux-x86_64-musl archive" ;;
esac

python3 -I - "$archive" <<'PY'
import hashlib, json, re, sys
from pathlib import Path

archive = Path(sys.argv[1])
def refuse(why):
    sys.exit(f"images/nanos-helium/build.sh: {archive.name}: {why}")

digest = hashlib.sha256()
with archive.open("rb") as stream:
    for chunk in iter(lambda: stream.read(1 << 20), b""):
        digest.update(chunk)
actual = digest.hexdigest()
fields = archive.with_name(archive.name + ".sha256").read_text().split()
if fields[:2] != [actual, archive.name]:
    refuse(f"the .sha256 sidecar {fields[:2]} does not bind digest {actual} and this archive name")
revision = re.fullmatch(r"helium-[0-9.]+-r([0-9]+)-linux-x86_64-musl\.tar\.xz", archive.name)
if revision is None or int(revision.group(1)) < 5:
    refuse("archives before r5 lack the pwritev2 and sched_getparam sandbox fixes musl needs")
report = json.loads(archive.with_name(archive.name + ".acceptance.json").read_text())
expected = {"product": "helium", "architecture": "amd64", "archive": archive.name, "archive_sha256": actual}
for key, value in expected.items():
    if report.get(key) != value:
        refuse(f"the acceptance sidecar has {key}={report.get(key)!r}, not {value!r}")
PY
sha256=$(cut -d' ' -f1 "$archive.sha256")

rm -rf "$work"
mkdir -p "$work/chromium-build"
for script in install-runtime.py runtime_install.py linux_runtime.py; do
    curl --proto '=https' --tlsv1.2 -fsSL -o "$work/chromium-build/$script" \
        "https://raw.githubusercontent.com/laputa-systems/chromium-build/$chromium_build_rev/scripts/$script" ||
        fail "cannot fetch chromium-build $chromium_build_rev scripts/$script"
done
python3 "$work/chromium-build/install-runtime.py" --archive "$archive" --verify-extract "$work/verified" >"$work/install.log" 2>&1 ||
    { tail -n 20 "$work/install.log" >&2; fail "chromium-build's installer refused $name"; }
rm -rf "$work/verified"

HELIUM_ARCHIVE=$archive sh "$tree/dev/helium-rootfs.sh" >"$work/rootfs.log" 2>&1 ||
    { tail -n 20 "$work/rootfs.log" >&2; fail "the Nanos rootfs build failed"; }
rootfs=$work/rootfs
mv "$tree/.work/helium/rootfs" "$rootfs"
mv "$rootfs/opt/helium" "$rootfs/opt/browser"
mkdir -p "$rootfs/etc/bctl" "$rootfs/home/browser" "$rootfs/run/bctl"
cp "$root/guest/appliance/rootfs/etc/passwd" "$root/guest/appliance/rootfs/etc/group" "$rootfs/etc/"

llvm=$runtime/llvm
(
    cd "$root/guest"
    CARGO_NET_GIT_FETCH_WITH_CLI=false \
    CC_x86_64_unknown_linux_musl=$llvm/bin/clang \
    CFLAGS_x86_64_unknown_linux_musl=--target=x86_64-linux-musl \
    RUSTFLAGS="-Clinker=$llvm/bin/clang -Clink-self-contained=no -Clink-arg=--target=x86_64-linux-musl -Clink-arg=-fuse-ld=mold" \
    CARGO_TARGET_DIR=$work/target \
        cargo build --quiet -Zbuild-std=std,panic_abort --locked --release --target x86_64-unknown-linux-musl -p bctl-guest
) || fail "bctl-guest build failed"
guest=$work/target/x86_64-unknown-linux-musl/release/bctl-guest
mkdir -p "$rootfs/usr/local/bin"
cp "$guest" "$rootfs/usr/local/bin/bctl-guest"

version=$(env -i HOME=/home/browser LD_LIBRARY_PATH="$rootfs/opt/browser/lib" "$rootfs/lib/ld-musl-x86_64.so.1" \
    "$rootfs/opt/browser/helium" --version) || fail "helium --version failed against the rootfs's musl"
case $version in
    "Helium "*) ;;
    *) fail "helium --version printed $version, not a Helium version" ;;
esac
python3 -I - "$name" "$sha256" "$version" "$(cat "$tree/COMMIT")" "$rootfs/etc/bctl/receipt.json" <<'PY'
import json, sys
from pathlib import Path

archive, sha256, version, hydrogen, out = sys.argv[1:6]
receipt = {"image": "nanos-helium", "browser_version": version, "archive": archive, "archive_sha256": sha256, "hydrogen": hydrogen}
Path(out).write_text(json.dumps(receipt, indent=2) + "\n")
PY

sh "$tree/dev/image.sh" -o "$out" --size 2g --klib tmpfs --klib shmem --root "$rootfs" --tree "$rootfs:/" \
    --link /usr/local/bin/browser:/opt/browser/helium \
    --dir /profile --dir /tmp --dir /dev/shm \
    --arg run-headless "$guest" >/dev/null || fail "mkfs failed"
rm -rf "$work/target"
echo "$out ($version, hydrogen $(cat "$tree/COMMIT"))"
