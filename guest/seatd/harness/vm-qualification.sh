#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "$#" != 5 ]; then
  echo 'usage: harness/vm-qualification.sh transition|shutdown|compositor guest-root kernel initramfs seatd' >&2
  exit 2
fi
mode=$1
source_root=$(realpath "$2")
kernel=$(realpath "$3")
initramfs=$(realpath "$4")
daemon=$(realpath "$5")
case "$mode" in transition|shutdown|compositor) ;; *) exit 2 ;; esac
for path in "$source_root/init" "$kernel" "$initramfs" "$daemon"; do test -e "$path"; done
if [ "$mode" = compositor ]; then
  for path in usr/bin/dwl usr/bin/foot sbin/udevd bin/udevadm usr/lib/libwlroots-0.20.so; do test -e "$source_root/$path" || { echo "missing guest $path" >&2; exit 1; }; done
fi
exclude=$(git rev-parse --git-path info/exclude)
rg -qxF '.work/' "$exclude" || printf '\n.work/\n' >> "$exclude"
mkdir -p .work/tmp .work/vm-qualification
export TMPDIR="$PWD/.work/tmp"
work="$PWD/.work/vm-qualification/$mode"
rm -rf "$work/root"
mkdir -p "$work/root"
cp -a --reflink=auto "$source_root/." "$work/root/"
cp "$daemon" "$work/root/usr/local/bin/seatd"
cc -std=c11 -static -D_XOPEN_SOURCE=700 harness/vm-probe.c -o "$work/root/usr/local/bin/vm-probe"
cp harness/vm-guest-test.sh "$work/root/usr/local/bin/vm-guest-test"
cat > "$work/root/init" <<'GUEST'
#!/bin/sh
mount -o remount,rw /
mkdir -p /dev/pts /dev/shm /run /tmp/runtime
mount -t devpts none /dev/pts
chmod 700 /tmp/runtime
export PATH=/usr/local/bin:/usr/bin:/bin:/sbin
modprobe evdev
mdev -s
export LIBSEAT_BACKEND=seatd XDG_RUNTIME_DIR=/tmp/runtime WLR_RENDERER=pixman
if [ "$VM_MODE" = compositor ]; then
  /sbin/udevd --daemon
  udevadm trigger
  udevadm settle
fi
seatd -l debug &
pid=$!
sleep 1
case "$VM_MODE" in
  transition) vm-probe ;;
  shutdown) VM_SHUTDOWN_PID=$pid vm-probe ;;
  compositor) dwl -s 'foot /usr/local/bin/vm-guest-test' ;;
esac
rc=$?
echo VM_EXIT=$rc
kill "$pid" 2>/dev/null || true
wait "$pid"
poweroff -f
GUEST
chmod +x "$work/root/init"
truncate -s 0 "$work/guest.img"
truncate -s 1G "$work/guest.img"
mkfs.ext4 -q -d "$work/root" "$work/guest.img"
qmp="$work/qmp.sock"
log="$work/console.log"
rm -f "$qmp" "$log"
pid=
wait_for_exit() {
  for _ in $(seq 1 "$1"); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  ! kill -0 "$pid" 2>/dev/null
}
cleanup() {
  if [ -n "$pid" ]; then
    if kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null || true
      if ! wait_for_exit 5; then
        kill -KILL "$pid" 2>/dev/null || true
        if ! wait_for_exit 5; then
          echo "QEMU did not exit after SIGKILL: $pid" >&2
          return
        fi
      fi
    fi
    wait "$pid" 2>/dev/null || true
    pid=
  fi
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 131' QUIT
trap 'exit 143' TERM
qemu-system-x86_64 -M q35 -m 1024 -cpu host -enable-kvm -vga none -device virtio-gpu-pci -device virtio-keyboard-pci -kernel "$kernel" -initrd "$initramfs" -append "console=tty0 console=ttyS0 root=/dev/vda rootfstype=ext4 rw init=/init VM_MODE=$mode" -drive "file=$work/guest.img,format=raw,if=virtio" -display none -serial "file:$log" -qmp "unix:$qmp,server=on,wait=off" -no-reboot > "$work/qemu.log" 2>&1 &
pid=$!
if [ "$mode" = compositor ]; then
  for _ in $(seq 1 60); do
    if rg -q '^VM_PROMPT_READY' "$log" 2>/dev/null; then break; fi
    kill -0 "$pid" 2>/dev/null || { tail -40 "$log"; exit 1; }
    sleep 1
  done
  rg -q '^VM_PROMPT_READY' "$log"
  python3 harness/vm-qmp.py "$qmp" input
  for _ in $(seq 1 20); do
    if rg -q '^VM_INPUT_OK' "$log"; then break; fi
    kill -0 "$pid" 2>/dev/null || { tail -40 "$log"; exit 1; }
    sleep 1
  done
  rg -q '^VM_INPUT_OK' "$log"
  python3 harness/vm-qmp.py "$qmp" screenshot "$work/screen.ppm"
  python3 harness/vm-check-image.py "$work/screen.ppm"
  python3 harness/vm-qmp.py "$qmp" quit
  if ! wait_for_exit 15; then echo 'QEMU did not exit after QMP quit' >&2; exit 1; fi
  wait "$pid"
  pid=
else
  for _ in $(seq 1 60); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 1
  done
  if kill -0 "$pid" 2>/dev/null; then echo 'guest timed out' >&2; exit 1; fi
  wait "$pid"
  pid=
  rg -q '^VM_EXIT=0' "$log"
  if [ "$mode" = transition ]; then
    for marker in VM_RELEASE_REVOKE_HANDOFF_OK VM_ACQUIRE_REOPEN_OK VM_RESTORE_OK; do rg -q "^$marker" "$log"; done
  else
    rg -q '^VM_SHUTDOWN_RESTORE_OK' "$log"
  fi
fi
echo "VM_${mode^^}_OK $log"
