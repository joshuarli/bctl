#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD/.work/guest-root"
bash harness/prepare-guest.sh "$ROOT"
export TMPDIR="$PWD/.work/tmp"
echo 'building tinyland (musl)'
cargo build --locked --release --target x86_64-unknown-linux-musl 2>&1 | tail -n 3
mkdir -p "$ROOT/usr/local/bin"
cp target/x86_64-unknown-linux-musl/release/tinyland "$ROOT/usr/local/bin/tinyland"
cp guest/init "$ROOT/init"
cp guest/test-app "$ROOT/usr/local/bin/guest-test"
cp guest/seat-proof "$ROOT/usr/local/bin/seat-proof"
chmod +x "$ROOT/init" "$ROOT/usr/local/bin/guest-test" "$ROOT/usr/local/bin/seat-proof"
cp "$ROOT/boot/vmlinuz-virt" .work/vmlinuz-virt
cp "$ROOT/boot/initramfs-virt" .work/initramfs-virt
echo 'building disk image'
truncate -s 0 .work/guest.img
truncate -s 1G .work/guest.img
mkfs.ext4 -q -d "$ROOT" .work/guest.img
echo BUILD_GUEST_OK
