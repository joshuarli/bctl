#!/bin/sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
guest=$(cd "$here/../../.." && pwd)
out=${WLCS_OUT:-$guest/../.work/build/wlcs}
mkdir -p "$out/target"
docker build --platform linux/arm64 -t bctl/tinyland-wlcs:local "$here"
exec docker run --rm --platform linux/arm64 \
    -v "$guest:/src" -v "$out/target:/target" \
    -e CARGO_TARGET_DIR=/target -w /src/tinyland/harness/wlcs/adapter \
    bctl/tinyland-wlcs:local sh -c '
        cargo build --release &&
        if [ "${1:-}" = --sweep ]; then
            exec sh /src/tinyland/harness/wlcs/sweep.sh /target/release/libtinyland_wlcs.so /target/sweep
        fi &&
        exec /wlcs-build/wlcs /target/release/libtinyland_wlcs.so "$@"
    ' wlcs "$@"
