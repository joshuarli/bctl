#!/bin/sh
set -eu

# Builds the SmolVM guest inputs for linux/arm64: the kernel bundle, the agent
# rootfs, and the pre-formatted disk templates. The host is macOS, where none
# of the guest toolchain (a musl C compiler, apk, mke2fs) exists, so the work
# runs in a pinned Debian container.

image=debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2
root=$(cd "$(dirname "$0")/.." && pwd)
source_dir="$root/third_party/smolvm"
out="$root/.work/runtime/guest"
rust_toolchain=stable

fail() { echo "runtime/guest.sh: $*" >&2; exit 1; }

[ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || fail "only Apple Silicon macOS hosts are supported"
[ -f "$source_dir/Cargo.toml" ] || fail "vendored SmolVM source is missing; run scripts/update-smolvm.sh"
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
    -v "$source_dir:/src:ro" \
    -v "$out/work:/work" \
    -v "$out/work/inputs:/inputs:ro" \
    -e RUST_TOOLCHAIN="$rust_toolchain" \
    "$image" bash -eu -c '
        export DEBIAN_FRONTEND=noninteractive
        apt-get update >/dev/null
        apt-get install -y --no-install-recommends \
            bc bison build-essential ca-certificates curl e2fsprogs flex file \
            libelf-dev libncurses-dev libssl-dev musl-tools python3 \
            python3-pyelftools rsync xz-utils zstd >/dev/null

        if [ ! -x /work/cargo/bin/cargo ] || [ ! -d "/work/rustup/toolchains/${RUST_TOOLCHAIN}-aarch64-unknown-linux-gnu" ]; then
            curl -fsSL https://sh.rustup.rs -o /work/rustup-init.sh
            RUSTUP_HOME=/work/rustup CARGO_HOME=/work/cargo \
                sh /work/rustup-init.sh -y --profile minimal --default-toolchain "$RUST_TOOLCHAIN" \
                --target aarch64-unknown-linux-musl >/dev/null
            rm -f /work/rustup-init.sh
        fi
        export RUSTUP_HOME=/work/rustup CARGO_HOME=/work/cargo
        export PATH="/work/cargo/bin:$PATH"
        rustup default "$RUST_TOOLCHAIN" >/dev/null
        rustup target add --toolchain "$RUST_TOOLCHAIN" aarch64-unknown-linux-musl >/dev/null
        rustup target list --installed --toolchain "$RUST_TOOLCHAIN" | grep -qx aarch64-unknown-linux-musl
        RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN"
        export RUSTUP_TOOLCHAIN

        rm -rf /work/libkrunfw
        cp -a /src/libkrunfw /work/libkrunfw
        mkdir -p /work/libkrunfw/tarballs
        cp /inputs/linux-*.tar.xz /work/libkrunfw/tarballs/
        make -C /work/libkrunfw OS=Linux ARCH=arm64 -j"$(nproc)" kernel.c >/work/kernel.log 2>&1
        test -s /work/libkrunfw/kernel.c
        mv /work/libkrunfw/kernel.c /work/kernel.c
        rm -rf /work/libkrunfw

        export CC_aarch64_unknown_linux_musl=aarch64-linux-musl-gcc
        export AR_aarch64_unknown_linux_musl=ar
        cargo build --locked --profile release-small -p smolvm-agent \
            --manifest-path /src/Cargo.toml --target aarch64-unknown-linux-musl \
            --target-dir /work/target >/work/agent.log 2>&1
        cargo build --locked --release --manifest-path /src/libkrun/init/Cargo.toml \
            --target aarch64-unknown-linux-musl \
            --target-dir /work/target >/work/krun-init.log 2>&1
        agent=/work/target/aarch64-unknown-linux-musl/release-small/smolvm-agent
        krun_init=/work/target/aarch64-unknown-linux-musl/release/krun-init
        for binary in "$agent" "$krun_init"; do
            test -s "$binary"
            readelf -h "$binary" | grep -Eq "^[[:space:]]*Machine:[[:space:]]+AArch64[[:space:]]*$"
            if readelf -l "$binary" | grep -q INTERP; then
                echo "guest binary has a dynamic interpreter: $binary" >&2
                exit 1
            fi
            if readelf -d "$binary" | grep -q NEEDED; then
                echo "guest binary has dynamic dependencies: $binary" >&2
                exit 1
            fi
        done
        mv "$krun_init" /work/krun-init
        mv "$agent" /work/smolvm-agent
        rm -rf /work/target

        AGENT_BINARY=/work/smolvm-agent /src/scripts/build-agent-rootfs.sh --crane-release --no-build-agent \
            /work/rootfs >/work/rootfs.log 2>&1
        test -x /work/rootfs/usr/local/bin/smolvm-agent
        test -L /work/rootfs/sbin/init

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
    ' || fail "guest build failed (see .work/runtime/guest/work/{kernel,rootfs}.log)"

for name in storage overlay; do
    zstd -19 -T0 -q --rm -f "$out/work/$name-template.ext4" -o "$out/work/$name-template.ext4.zst"
done

rm -rf "$out/rootfs"
mv "$out/work/rootfs" "$out/rootfs"
mv "$out/work/kernel.c" "$out/kernel.c"
mv "$out/work/krun-init" "$out/krun-init"
for name in storage overlay; do
    mv "$out/work/$name-template.ext4.zst" "$out/$name-template.ext4.zst"
done
echo "$out/rootfs, $out/kernel.c, $out/krun-init, $out/templates"
