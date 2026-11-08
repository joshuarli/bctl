#!/bin/bash
set -euo pipefail
MODE="$1"
CONSOLE="$2"
SCREENSHOT="$3"
if rg -qi 'TINYLAND_EXIT|TINYLAND_CHILD_EXIT|TINYLAND_SESSION_END|protocol.?error|foot spawn failed|GUEST_TEXT_MISMATCH|GUEST_READ_FAILED' "$CONSOLE"; then
  echo 'FAIL: guest or compositor exited or rejected protocol'
  exit 1
fi
rg -q 'TINYLAND_SURFACE_MAPPED' "$CONSOLE" || { echo 'FAIL: surface never mapped'; exit 1; }
rg -q 'TINYLAND_PRESENTED' "$CONSOLE" || { echo 'FAIL: no confirmed presentation'; exit 1; }
if [ "$MODE" = smoke ]; then
  rg -q '^GUEST_TEXT_OK' "$CONSOLE" || { echo 'FAIL: input not confirmed'; exit 1; }
fi
python3 harness/check-screenshot.py "$SCREENSHOT" 0.05 0.0001
