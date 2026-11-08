#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p .work/tmp
cargo tree --locked -e features > .work/tmp/features.txt
if rg -i 'smithay|wlroots|libdrm|mesa|gbm|egl|wayland-backend feature "server_system"|wayland-sys feature "server"' .work/tmp/features.txt; then
  echo 'FAIL: forbidden compositor dependency'; exit 1
fi
if rg -w 'xkbcommon' .work/tmp/features.txt | rg -v 'oracle'; then
  echo 'FAIL: runtime xkbcommon binding remains'; exit 1
fi
BINARY=target/x86_64-unknown-linux-musl/release/tinyland
readelf -d "$BINARY" > .work/tmp/linker.txt
if rg -i 'NEEDED.*(wayland|drm|mesa|gbm|egl|pixman|systemd)' .work/tmp/linker.txt; then
  echo 'FAIL: forbidden linked library'; exit 1
fi
if rg -i 'NEEDED.*libxkbcommon' .work/tmp/linker.txt; then
  echo 'FAIL: libxkbcommon remains linked'; exit 1
fi
cat .work/tmp/linker.txt
if rg -n '\bunsafe\b' src --glob '!seat.rs'; then
  echo 'FAIL: unexpected unsafe code requires inventory review'; exit 1
fi
echo '--- reviewed unsafe inventory: src/seat.rs only (libseat FFI) ---'
rg -n '\bunsafe\b' src/seat.rs
wc -l src/*.rs src/server/*.rs tests/*.rs
wc -c "$BINARY"
echo AUDIT_OK
