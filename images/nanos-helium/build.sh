#!/bin/sh
set -eu

chromium_build_rev=4275db88a4b3883c0316d99e03968a7f9ad9701a
root=$(cd "$(dirname "$0")/../.." && pwd)
runtime=$root/.work/runtime/nanos
tree=$runtime/hydrogen
images=$root/.work/images
work=$images/nanos-helium
out=$images/nanos-helium.img
kernel_out=$images/nanos-helium.kernel.img
profile_out=$images/nanos-helium.profile.img
dl=$images/dl/chromium-build/$chromium_build_rev
chromium_scripts="install-runtime.py runtime_install.py linux_runtime.py"
archives=${BCTL_BROWSER_ARCHIVE:?set BCTL_BROWSER_ARCHIVE to a directory holding one helium-*-linux-x86_64-musl.tar.xz with its sidecars}
force=${FORCE:-0}

fail() { echo "images/nanos-helium/build.sh: $*" >&2; exit 1; }

sha() { sha256sum "$1" | cut -d' ' -f1; }
digest() { sha256sum | cut -d' ' -f1; }

nano_out=$tree/nanos/output
kernel=$nano_out/platform/pc/bin/kernel.img
boot=$nano_out/platform/pc/boot/boot.img
mkfs=$nano_out/tools/bin/mkfs
kernel_stamp=$nano_out/platform/pc/bin/.build-nanos-kernel.stamp
rootfs=$work/rootfs
target=$work/target
guest=$target/x86_64-unknown-linux-musl/release/bctl-guest
self=$(realpath "$0")

stamp_valid() {
    stamp=$1 identity=$2
    shift 2
    [ -f "$stamp" ] || return 1
    [ "$(sed -n '1p' "$stamp")" = "$identity" ] || return 1
    for path in "$@"; do
        [ -e "$path" ] || return 1
        want=$(awk -v path="$path" '$2 == path { print $1; exit }' "$stamp")
        [ -n "$want" ] || return 1
        [ "$(sha "$path")" = "$want" ] || return 1
    done
}

write_stamp() {
    stamp=$1 identity=$2
    shift 2
    {
        printf '%s\n' "$identity"
        for path in "$@"; do
            printf '%s  %s\n' "$(sha "$path")" "$path"
        done
    } >"$stamp.new"
    mv "$stamp.new" "$stamp"
}

rootfs_digest() {
    find "$1" -mindepth 1 -exec stat -c '%F %a %s %N' {} + | LC_ALL=C sort | digest
}

rootfs_identity() {
    printf 'archive %s\n' "$sha256"
    printf 'chromium-build %s\n' "$chromium_build_rev"
    printf 'hydrogen %s\n' "$(cat "$tree/COMMIT")"
    printf 'kernel-stamp %s\n' "$(sha "$kernel_stamp")"
    printf 'passwd %s\n' "$(sha "$root/guest/appliance/rootfs/etc/passwd")"
    printf 'group %s\n' "$(sha "$root/guest/appliance/rootfs/etc/group")"
}

installer_identity() {
    printf 'archive %s\n' "$sha256"
    printf 'chromium-build %s\n' "$chromium_build_rev"
    for script in $chromium_scripts; do printf 'script %s %s\n' "$script" "$(sha "$dl/$script")"; done
}

scripts_present() {
    for script in $chromium_scripts; do [ -s "$dl/$script" ] || return 1; done
}

pack_identity() {
    printf 'archive %s\n' "$sha256"
    printf 'chromium-build %s\n' "$chromium_build_rev"
    printf 'hydrogen %s\n' "$(cat "$tree/COMMIT")"
    printf 'kernel-stamp %s\n' "$(sha "$kernel_stamp")"
    printf 'kernel %s\n' "$(sha "$kernel")"
    printf 'boot %s\n' "$(sha "$boot")"
    printf 'mkfs %s\n' "$(sha "$mkfs")"
    printf 'rootfs %s\n' "$(rootfs_digest "$rootfs")"
    printf 'guest %s\n' "$(sha "$guest")"
    printf 'script %s\n' "$(sha "$self")"
}

verify_nanos_output() {
    rel=$1 path=$2
    want=$(awk -v path="$rel" '$2 == path { print $1; exit }' "$kernel_stamp")
    [ -n "$want" ] && [ "$(sha "$path")" = "$want" ] ||
        fail "$path does not match build-nanos.sh's stamp; run runtime/build-nanos.sh"
}

[ -s "$tree/COMMIT" ] && [ -x "$runtime/llvm/bin/clang" ] || fail "run runtime/build-nanos.sh first"
[ -s "$kernel_stamp" ] || fail "no kernel stamp in $tree; run runtime/build-nanos.sh first"
[ -s "$kernel" ] && [ -s "$boot" ] && [ -x "$mkfs" ] ||
    fail "the hydrogen tree has no kernel.img, boot.img or mkfs; run runtime/build-nanos.sh"
verify_nanos_output hydrogen/nanos/output/platform/pc/bin/kernel.img "$kernel"
verify_nanos_output hydrogen/nanos/output/platform/pc/boot/boot.img "$boot"
verify_nanos_output hydrogen/nanos/output/tools/bin/mkfs "$mkfs"

set -- "$archives"/*.tar.xz
[ $# = 1 ] && [ -f "$1" ] || fail "want exactly one *.tar.xz in $archives, found: $*"
archive=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
name=${archive##*/}
case $name in
    helium-*-linux-x86_64-musl.tar.xz) ;;
    *) fail "$name is not named as a Helium linux-x86_64-musl archive" ;;
esac

sha256=$(python3 -I - "$archive" <<'PY'
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
print(actual)
PY
)

mkdir -p "$images" "$work"

installer_stamp=$work/installer.stamp
if [ "$force" != 1 ] && scripts_present && stamp_valid "$installer_stamp" "$(installer_identity | digest)"; then
    echo "installer: reused"
else
    mkdir -p "$dl"
    for script in $chromium_scripts; do
        [ -s "$dl/$script" ] && continue
        curl --proto '=https' --tlsv1.2 -fsSL -o "$dl/$script.part" \
            "https://raw.githubusercontent.com/laputa-systems/chromium-build/$chromium_build_rev/scripts/$script" ||
            fail "cannot fetch chromium-build $chromium_build_rev scripts/$script"
        mv "$dl/$script.part" "$dl/$script"
    done
    rm -rf "$work/verified"
    python3 "$dl/install-runtime.py" --archive "$archive" --verify-extract "$work/verified" >"$work/install.log" 2>&1 ||
        { tail -n 20 "$work/install.log" >&2; fail "chromium-build's installer refused $name"; }
    rm -rf "$work/verified"
    write_stamp "$installer_stamp" "$(installer_identity | digest)"
    echo "installer: built"
fi

rootfs_stamp=$work/rootfs.stamp
rootfs_valid=0
if [ "$force" != 1 ] && [ -d "$rootfs" ] && [ -f "$rootfs_stamp" ]; then
    rootfs_id=$(rootfs_identity | digest)
    if [ "$(sed -n '1p' "$rootfs_stamp")" = "$rootfs_id" ] &&
        [ "$(sed -n '2p' "$rootfs_stamp")" = "rootfs $(rootfs_digest "$rootfs")" ]; then
        rootfs_valid=1
    fi
fi
if [ "$rootfs_valid" = 1 ]; then
    echo "rootfs: reused"
else
    HELIUM_ARCHIVE=$archive sh "$tree/dev/helium-rootfs.sh" >"$work/rootfs.log" 2>&1 ||
        { tail -n 20 "$work/rootfs.log" >&2; fail "the Nanos rootfs build failed"; }
    [ -d "$tree/.work/helium/rootfs" ] || fail "the Nanos rootfs build produced no rootfs"
    rm -rf "$rootfs"
    mv "$tree/.work/helium/rootfs" "$rootfs"
    mv "$rootfs/opt/helium" "$rootfs/opt/browser"
    mkdir -p "$rootfs/etc/bctl" "$rootfs/home/browser" "$rootfs/run/bctl"
    cp "$root/guest/appliance/rootfs/etc/passwd" "$root/guest/appliance/rootfs/etc/group" "$rootfs/etc/"
    echo "rootfs: built"
fi

llvm=$runtime/llvm
if ! cargo_output=$(
    cd "$root/guest"
    CARGO_NET_GIT_FETCH_WITH_CLI=false \
    CC_x86_64_unknown_linux_musl=$llvm/bin/clang \
    CFLAGS_x86_64_unknown_linux_musl=--target=x86_64-linux-musl \
    RUSTFLAGS="-Clinker=$llvm/bin/clang -Clink-self-contained=no -Clink-arg=--target=x86_64-linux-musl -Clink-arg=-fuse-ld=mold" \
    CARGO_TARGET_DIR=$target \
        cargo build -Zbuild-std=std,panic_abort --locked --release --target x86_64-unknown-linux-musl -p bctl-guest 2>&1
); then
    printf '%s\n' "$cargo_output" >&2
    fail "bctl-guest build failed"
fi
[ -s "$guest" ] || fail "bctl-guest built no $guest"
rootfs_guest=$rootfs/usr/local/bin/bctl-guest
mkdir -p "$rootfs/usr/local/bin"
if [ ! -f "$rootfs_guest" ] || [ "$(sha "$rootfs_guest")" != "$(sha "$guest")" ]; then
    cp "$guest" "$rootfs_guest"
fi
if printf '%s\n' "$cargo_output" | grep -q 'Compiling bctl-guest '; then
    echo "guest: built"
else
    echo "guest: reused"
fi

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

{
    printf '%s\n' "$(rootfs_identity | digest)"
    printf 'rootfs %s\n' "$(rootfs_digest "$rootfs")"
} >"$rootfs_stamp.new"
mv "$rootfs_stamp.new" "$rootfs_stamp"

image_stamp=$work/image.stamp
pack_id=$(pack_identity | digest)
if [ "$force" != 1 ] && stamp_valid "$image_stamp" "$pack_id" "$out" "$kernel_out" "$profile_out"; then
    echo "image: reused"
else
    cp "$kernel" "$kernel_out.new"
    sh "$tree/dev/image.sh" -o "$out" --size 2g --klib tmpfs --klib shmem --root "$rootfs" --tree "$rootfs:/" \
        --link /usr/local/bin/browser:/opt/browser/helium \
        --dir /profile --dir /tmp --dir /dev/shm --dir /home/browser/Downloads \
        --arg run-headless-persistent "$guest" >/dev/null || { rm -f "$kernel_out.new" "$out"; fail "mkfs failed"; }
    "$mkfs" -s 2g -k "$kernel" -b "$boot" -t '(mounts:(profile:"/profile" files:"/home/browser/Downloads"))' "$out.new" <"$out.manifest" >/dev/null &&
        "$mkfs" -e -l profile -s 2g "$profile_out.new" >/dev/null ||
        { rm -f "$kernel_out.new" "$out" "$out.new" "$profile_out.new"; fail "cannot create the persistent profile volumes"; }
    mv "$out.new" "$out"
    mv "$profile_out.new" "$profile_out"
    mv "$kernel_out.new" "$kernel_out"
    write_stamp "$image_stamp" "$pack_id" "$out" "$kernel_out" "$profile_out"
    echo "image: built"
fi

echo "$out ($version, hydrogen $(cat "$tree/COMMIT")), kernel $kernel_out, profile template $profile_out"
