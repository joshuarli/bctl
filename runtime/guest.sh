#!/bin/sh
set -eu

image=alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
root=$(cd "$(dirname "$0")/.." && pwd)
source_dir="$root/third_party/smolvm"
out="$root/.work/runtime/guest"
rust_toolchain=nightly-2026-09-15

fail() { echo "runtime/guest.sh: $*" >&2; exit 1; }

[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || fail "only Apple Silicon macOS hosts are supported"
[ -f "$source_dir/Cargo.toml" ] || fail "vendored SmolVM source is missing; run scripts/update-smolvm.sh"
[ -f "$root/third_party/UPSTREAM.lock" ] || fail "third_party/UPSTREAM.lock is missing; run scripts/update-smolvm.sh"
locked_toolchain=$(sed -n 's/^init = "\(.*\)"$/\1/p' "$root/third_party/UPSTREAM.lock")
[ "$rust_toolchain" = "$locked_toolchain" ] || fail "guest toolchain $rust_toolchain differs from UPSTREAM.lock ($locked_toolchain)"
if [ "${FORCE:-0}" != 1 ] &&
    [ -s "$out/kernel.c" ] && [ -x "$out/krun-init" ] && [ -d "$out/rootfs" ] &&
    [ -s "$out/storage-template.ext4.zst" ] && [ -s "$out/overlay-template.ext4.zst" ]; then
    echo "$out is current; set FORCE=1 to rebuild the guest inputs"
    exit 0
fi
python3 -B "$root/scripts/update_smolvm.py" --repo-root "$root" --fetch-kernel "$out/work/inputs" >/dev/null ||
    fail "cannot fetch the pinned kernel archive"

mkdir -p "$out/work"
docker run --rm --platform linux/arm64 \
    -v "$root:/repo:ro" \
    -v "$out/work:/work" \
    -v "$out/work/inputs:/inputs:ro" \
    -e RUST_TOOLCHAIN="$rust_toolchain" \
    "$image" sh -eu -c '
        apk add --no-cache bash bc bison build-base bzip2 ca-certificates cmake curl \
            e2fsprogs e2fsprogs-extra elfutils-dev file flex gawk gperf linux-headers m4 \
            make meson mtd-utils-dev ncurses-dev openssl-dev patch perl pkgconf python3 \
            py3-elftools tar texinfo xz zstd >/dev/null

        rm -rf /work/libkrunfw
        cp -a /repo/third_party/smolvm/libkrunfw /work/libkrunfw
        mkdir -p /work/libkrunfw/tarballs
        cp /inputs/linux-*.tar.xz /work/libkrunfw/tarballs/
        make -C /work/libkrunfw OS=Linux ARCH=arm64 -j"$(nproc)" kernel.c >/work/kernel.log 2>&1
        test -s /work/libkrunfw/kernel.c
        mv /work/libkrunfw/kernel.c /work/kernel.c
        rm -rf /work/libkrunfw

        bash /repo/runtime/guest/rootfs.sh

        for spec in "storage:smolvm:20" "overlay:smolvm-overlay:10"; do
            IFS=: read -r name label gib <<EOF
$spec
EOF
            template="/work/$name-template.ext4"
            rm -f "$template"
            dd if=/dev/zero of="$template" bs=1 count=0 seek=$((512 * 1024 * 1024)) 2>/dev/null
            mke2fs -F -q -m 0 -L "$label" "$template"
            truncate -s $((gib * 1024 * 1024 * 1024)) "$template"
        done

        chmod -R u+w /work/gopath /work/gomodcache 2>/dev/null || true
        rm -rf /work/target /work/target-seatd /work/build /work/gopath /work/gomodcache
    ' || fail "guest build failed (see .work/runtime/guest/work/kernel.log and work/build/*.log)"

for name in storage overlay; do
    zstd -19 -T0 -q --rm -f "$out/work/$name-template.ext4" -o "$out/work/$name-template.ext4.zst"
done

rm -rf "$out/rootfs"
mv "$out/work/rootfs" "$out/rootfs"
mv "$out/work/kernel.c" "$out/kernel.c"
mv "$out/work/bin/krun-init" "$out/krun-init"
for name in storage overlay; do
    mv "$out/work/$name-template.ext4.zst" "$out/$name-template.ext4.zst"
done
rm -rf "$out/work/bin"
echo "$out/rootfs, $out/kernel.c, $out/krun-init, $out/templates"
