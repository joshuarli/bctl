#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
scratch=$root/.work/tmp/helium-rootfs-inputs
mkdir -p "$scratch/kernel/dev" "$scratch/kernel/.work/helium"
for file in helium-rootfs.sh common.sh toolchain.lock; do
    cp "$root/kernel/dev/$file" "$scratch/kernel/dev/$file"
done
script=$scratch/kernel/dev/helium-rootfs.sh
archive=$scratch/helium-fixture.tar.xz
digest=0000000000000000000000000000000000000000000000000000000000000000
printf 'changed archive\n' >"$archive"
printf '%s\n' "$digest" >"$scratch/kernel/.work/helium/archive.ok"

refuses() {
    message=$1
    shift
    if output=$(sh "$script" "$@" 2>&1); then
        printf 'FAIL: accepted invalid archive inputs\n' >&2
        exit 1
    fi
    case $output in
        *"$message"*) ;;
        *) printf 'FAIL: expected %s, got %s\n' "$message" "$output" >&2; exit 1 ;;
    esac
}

refuses 'archive digest must be SHA-256' "$archive" invalid helium-fixture
refuses 'invalid archive root' "$archive" "$digest" ../helium-fixture
refuses 'archive root must match the archive name' "$archive" "$digest" another-root
refuses 'Helium archive sha256 mismatch' "$archive" "$digest" helium-fixture
printf 'helium rootfs input checks passed\n'
