#!/bin/bash
# Seat-mediated proof: unprivileged tinyland+foot through seatd.
#
# Asserts the denial gate (direct opens refused for the session user) and
# the broker path (seat open, mediated devices, post-input marker with
# confirmed presentation), reusing the smoke input and screenshot checks.
set -euo pipefail
cd "$(dirname "$0")/.."
FAULT="${1:-none}"
case "$FAULT" in
  none) ;;
  *) echo "unsupported seat proof fault: $FAULT" >&2; exit 2 ;;
esac
MODE="seat"
mkdir -p .work/tmp
export TMPDIR="$PWD/.work/tmp"
QMP_SOCK="$PWD/.work/qmp-$MODE.sock"
CONSOLE="$PWD/.work/$MODE-console.log"
COMPOSITOR="$PWD/.work/$MODE-compositor.log"
SCREENSHOT="$PWD/.work/$MODE.png"
rm -f "$QMP_SOCK" "$CONSOLE" "$COMPOSITOR" "$SCREENSHOT"
cleanup() {
  local result=$?
  trap - EXIT
  if [ -n "${QEMU_PID:-}" ]; then
    if [ "$result" != 0 ] && [ -S "$QMP_SOCK" ] && kill -0 "$QEMU_PID" 2>/dev/null; then
      python3 harness/qmp.py "$QMP_SOCK" screendump "$SCREENSHOT" >/dev/null 2>&1 || true
    fi
    if kill -0 "$QEMU_PID" 2>/dev/null; then
      python3 harness/qmp.py "$QMP_SOCK" quit >/dev/null 2>&1 || true
      for _ in $(seq 1 5); do
        kill -0 "$QEMU_PID" 2>/dev/null || break
        sleep 1
      done
      if kill -0 "$QEMU_PID" 2>/dev/null; then kill -KILL "$QEMU_PID"; fi
    fi
    wait "$QEMU_PID" 2>/dev/null || true
  fi
  rm -f "$QMP_SOCK"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
"${QEMU:-qemu-system-x86_64}" -M q35 -m 1024 -cpu host -enable-kvm \
  -vga none -device virtio-gpu-pci -device virtio-keyboard-pci \
  -kernel .work/vmlinuz-virt -initrd .work/initramfs-virt \
  -append "console=tty0 console=ttyS0 root=/dev/vda rootfstype=ext4 rw init=/init TINYLAND_MODE=$MODE" \
  -drive file=.work/guest.img,format=raw,if=virtio \
  -display none -serial file:"$CONSOLE" -serial file:"$COMPOSITOR" \
  -qmp unix:"$QMP_SOCK",server=on,wait=off \
  -no-reboot > ".work/qemu-$MODE.log" 2>&1 &
QEMU_PID=$!
fail_if_dead() {
  if ! kill -0 "$QEMU_PID" 2>/dev/null || rg -qi 'TINYLAND_EXIT|TINYLAND_CHILD_EXIT|TINYLAND_SESSION_END|protocol.?error|foot spawn failed|GUEST_TEXT_MISMATCH|GUEST_READ_FAILED|TINYLAND_SEAT_PROOF_FAILED|seat dispatch failed|seat open failed' "$CONSOLE" 2>/dev/null; then
    tail -n 30 "$CONSOLE" 2>/dev/null || true
    echo 'FAIL: guest exited before proof' >&2
    exit 1
  fi
}
READY=0
for _ in $(seq 1 90); do
  fail_if_dead
  if rg -q TINYLAND_SURFACE_MAPPED "$CONSOLE" 2>/dev/null && rg -q GUEST_PROMPT_READY "$CONSOLE"; then READY=1; break; fi
  sleep 1
done
if [ "$READY" != 1 ]; then echo 'FAIL: guest never became ready'; tail -n 30 "$CONSOLE"; exit 1; fi
rg -q 'TINYLAND_SEAT_OPEN' "$CONSOLE" || { echo 'FAIL: seat was never opened through the broker'; exit 1; }
rg -q 'TINYLAND_DIRECT_DENIED' "$CONSOLE" || { echo 'FAIL: direct-open denial was not demonstrated'; exit 1; }
python3 harness/qmp.py "$QMP_SOCK" input
TEXT_OK=0
for _ in $(seq 1 20); do
  fail_if_dead
  if rg -q '^GUEST_TEXT_OK' "$CONSOLE"; then TEXT_OK=1; break; fi
  sleep 1
done
if [ "$TEXT_OK" != 1 ]; then echo 'FAIL: input not confirmed'; tail -n 30 "$CONSOLE"; exit 1; fi
sleep 1
fail_if_dead
python3 harness/qmp.py "$QMP_SOCK" screendump "$SCREENSHOT"
bash harness/check-proof.sh smoke "$CONSOLE" "$SCREENSHOT"
python3 harness/qmp.py "$QMP_SOCK" quit
for _ in $(seq 1 15); do
  kill -0 "$QEMU_PID" 2>/dev/null || break
  sleep 1
done
if kill -0 "$QEMU_PID" 2>/dev/null; then echo 'FAIL: QEMU did not exit'; exit 1; fi
wait "$QEMU_PID"
QEMU_PID=
echo SEAT_PROOF_OK
