#!/bin/sh
# Prove tinyland on SmolVM: foot pixels, VNC keyboard input, brokered seat.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
guest_root=$(dirname -- "$here")
repo=$(dirname -- "$guest_root")
work=$repo/.work/harness
main_worktree=$(git -C "$repo" worktree list --porcelain 2>/dev/null | sed -n '1s/^worktree //p')
smolvm=${SMOLVM:-${main_worktree:-/Users/josh/d/bctl}/.work/runtime/smolvm/smolvm}
name=${SMOLVM_MACHINE_NAME:-tinyland-smolvm}
display=${SMOLVM_DISPLAY_SIZE:-1280x800}
image=$work/tinyland-smolvm.tar
tag=bctl/tinyland-smolvm:local

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

created=0

dump_logs() {
    echo '--- guest workload output' >&2
    "$smolvm" machine logs --name "$name" --tail 60 2>&1 || true
    for log in /run/tinyland/session.log /run/tinyland/seatd.log /run/tinyland/compositor.log; do
        echo "--- $log" >&2
        "$smolvm" machine exec --name "$name" -- cat "$log" 2>&1 || true
    done
}

cleanup() {
    result=$?
    trap - EXIT INT TERM
    if [ "$created" = 1 ]; then
        if [ "$result" != 0 ]; then dump_logs; fi
        "$smolvm" machine stop --name "$name" --error-format json > /dev/null 2>&1 || true
        "$smolvm" machine delete --name "$name" -f --error-format json > /dev/null 2>&1 || true
    fi
    exit "$result"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

[ -x "$smolvm" ] || fail "patched SmolVM runtime missing at $smolvm"
mkdir -p "$work"

vnc_port=${SMOLVM_VNC_PORT:-$(python3 -B -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()')}
case "$vnc_port" in
    ''|*[!0-9]*) fail "invalid VNC port: $vnc_port" ;;
esac

if [ "${TINYLAND_SKIP_IMAGE_BUILD:-0}" != 1 ]; then
    docker build --platform linux/arm64 --target image \
        -f "$here/image/Dockerfile" -t "$tag" "$guest_root" \
        || fail "arm64 image build failed"
    docker save "$tag" -o "$image.part" || fail "docker save failed"
    mv "$image.part" "$image"
fi
[ -f "$image" ] || fail "missing image archive $image"
[ ! -L "$image" ] || fail "image archive must not be a symlink: $image"
echo "IMAGE_READY $image"

"$smolvm" machine delete --name "$name" -f --error-format json > /dev/null 2>&1 || true
"$smolvm" machine create --name "$name" --gpu --mem 2048 --image "$image" \
    --error-format json > /dev/null || fail "machine create failed"
created=1
SMOLVM_DISPLAY=$display SMOLVM_VNC=127.0.0.1:$vnc_port \
    "$smolvm" machine start --name "$name" --error-format json > /dev/null \
    || fail "machine start failed"
echo "SMOLVM_STARTED name=$name display=$display vnc=127.0.0.1:$vnc_port"

python3 -B "$here/rfb.py" --port "$vnc_port" \
    --out-prefix "$work/vnc" \
    || fail "VNC framebuffer/input proof failed"

compositor=$("$smolvm" machine exec --name "$name" --error-format json \
    -- cat /run/tinyland/compositor.log 2>&1) || fail "cannot read compositor log"
printf '%s\n' "$compositor" > "$work/compositor.log"
for marker in TINYLAND_SEAT_OPEN 'TINYLAND_DRM_ORIGIN seat:/dev/dri/card0' \
              TINYLAND_DRM_OK TINYLAND_COMPOSITOR_READY \
              TINYLAND_SURFACE_MAPPED TINYLAND_PRESENTED; do
    printf '%s\n' "$compositor" | grep -q "$marker" \
        || fail "compositor log missing $marker"
done
if printf '%s\n' "$compositor" | grep -Eq \
    'TINYLAND_SESSION_END|TINYLAND_CHILD_EXIT|present failed|seat open failed|brokered display init failed|foot spawn failed|seat dispatch failed'; then
    fail "compositor reported a session failure"
fi
echo "SEAT_MEDIATED_DRM_OK TINYLAND_SEAT_OPEN TINYLAND_DRM_OK"

uid=$("$smolvm" machine exec --name "$name" --error-format json -- sh -c \
    'for c in /proc/[0-9]*/comm; do if [ "$(cat "$c" 2>/dev/null)" = tinyland ]; then p=${c%/comm}; awk "/^Uid:/{print \$2}" "$p/status"; fi; done' \
    2>&1) || fail "cannot inspect the tinyland process"
printf '%s\n' "$uid" | grep -qx 1000 || fail "tinyland is not running as uid 1000: $uid"
echo "TINYLAND_UNPRIVILEGED uid=1000"

set +e
denial=$("$smolvm" machine exec --name "$name" -- su -s /bin/sh tiny -c \
    'exec 3<>/dev/dri/card0' 2>&1)
denial_status=$?
set -e
printf 'DIRECT_OPEN rc=%s output=%s\n' "$denial_status" "$denial"
[ "$denial_status" -ne 0 ] || fail "unprivileged direct open of /dev/dri/card0 succeeded"
printf '%s\n' "$denial" | grep -qi 'permission denied' \
    || fail "direct open did not report permission denied: $denial"
echo "DIRECT_OPEN_DENIED"

"$smolvm" machine stop --name "$name" --error-format json > /dev/null \
    || fail "machine stop failed"
"$smolvm" machine delete --name "$name" -f --error-format json > /dev/null \
    || fail "machine delete failed"
created=0
echo "SMOLVM_TINYLAND_PROOF_OK"
