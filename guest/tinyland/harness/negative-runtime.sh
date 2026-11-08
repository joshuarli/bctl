#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash harness/build-guest.sh 2>&1 | tail -n 5
for FAULT in no-input blank missing-marker frozen early-crash; do
  LOG=".work/smoke-$FAULT-proof.log"
  if bash harness/run-proof.sh smoke "$FAULT" > "$LOG" 2>&1; then
    echo "FAIL: live fault unexpectedly passed: $FAULT"; exit 1
  fi
  case "$FAULT" in
    no-input)
      rg -q 'FAIL: input not confirmed' "$LOG"
      if rg -q 'QMP_INPUT_OK|GUEST_TEXT_OK' "$LOG" ".work/smoke-$FAULT-console.log"; then echo 'FAIL: no-input fault delivered input'; exit 1; fi
      ;;
    blank) rg -q 'FAIL: nonblack' "$LOG" ;;
    missing-marker) rg -q 'FAIL: missing magenta marker' "$LOG" ;;
    frozen) rg -q 'FAIL: missing magenta marker' "$LOG" ;;
    early-crash)
      rg -q 'FAIL: guest exited before proof' "$LOG"
      rg -q 'TINYLAND_FAULT_INJECTED signal=KILL' ".work/smoke-$FAULT-console.log"
      ;;
  esac
  echo "LIVE_NEGATIVE_OK $FAULT"
done
