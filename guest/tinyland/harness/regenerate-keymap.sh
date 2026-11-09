#!/bin/sh
set -eu
xkeyboard_config=2.43-r0
root=$(cd "$(dirname "$0")/../../.." && pwd)
out="$root/.work/tmp/keymap"
mkdir -p "$out"
docker build -q --platform linux/arm64 --target tests \
  -f "$root/guest/harness/image/Dockerfile" -t bctl/tinyland-smolvm-tests:local "$root/guest" >/dev/null
docker run --rm --platform linux/arm64 --network none -w /src/tinyland \
  -v "$out:/out" -e TINYLAND_KEYMAP_OUT=/out/us.xkb -e PINNED="$xkeyboard_config" \
  bctl/tinyland-smolvm-tests:local sh -c '
    have=$(apk list -I xkeyboard-config | sed "s/ .*//; s/^xkeyboard-config-//")
    [ "$have" = "$PINNED" ] || { echo "xkeyboard-config $have, pinned $PINNED" >&2; exit 1; }
    cargo test --locked --bin tinyland -- --ignored regenerate_embedded_keymap'
cp "$out/us.xkb" "$root/guest/tinyland/data/us.xkb"
