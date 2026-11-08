# Verification reference

Host is Linux x86_64 with KVM; `harness/run-proof.sh` passes `-cpu host
-enable-kvm`. Guest-only root on `/dev/dri` and evdev is test harness, not
a production seat claim.

Guest is Alpine 3.20.3. `guest/packages.sha256` pins 49 APKs including
foot 1.17.2, strace 6.9, seatd 0.8.0, and libseat 0.8.0. `harness/prepare-guest.sh` checks the rootfs
hash, each APK hash, and signatures, then installs with `--no-network`.
`harness/test-guest-lock.py` rejects a corrupt cached APK.

`make drm-proof` proves real scanout through `src/output.rs`.
`make wayland-pixels` proves real foot shm pixels through `src/server.rs`.
`make smoke` additionally injects `laputa` plus Enter through QMP and
requires `GUEST_TEXT_OK` before the magenta `POST_INPUT_MARKER`, so the
screenshot proves input to foot to changed pixels to presentation; see
`guest/test-app`.
`make drm-parity` decodes the atomic and legacy screenshots byte for byte
and reruns the full input smoke through legacy fallback; see
`harness/drm-parity.sh`. `make negative` boots the five live faults in
`harness/negative-runtime.sh`.

Modeset tries atomic then legacy. Capability fallback allows ENOSYS,
EOPNOTSUPP, and EINVAL; commit fallback allows only ENOSYS and EOPNOTSUPP.
Both paths share the `page_flip` presentation in `src/output.rs`.
`guest/init` forces the legacy path with strace ENOSYS injection.

The compositor keeps two scanout buffers, writes the back buffer, requests
a flip event, and completes it in the shared poll loop within two seconds
for the matching kernel event before reusing the front buffer and firing
frame callbacks. Display, socket, DRM, and input are polled together, so
seat disable, shutdown, and device failure stay responsive during flip
wait. `TINYLAND_PRESENTED` marks completion. Client shm is copied at commit in
`src/server.rs`, then the buffer is released, so scanout never depends on
client-writable memory.

QMP input in `harness/qmp.py` sends 7 keys as one 14-event transaction.
An uncertain send exits 75 and is never retried; an explicit rejection is
a separate failure.

`harness/check-proof.sh` requires mapping, confirmed presentation, input
confirmation for smoke, and no protocol error, child exit, session end, or
early exit.
`harness/check-screenshot.py` requires 5% nonblack (`R+G+B > 24`) and
0.01% magenta (`R > 180, B > 180, G < 120`) with no crop; `drm-proof`
requires 30% nonblack instead. Proofs write console, compositor, QEMU,
and PNG artifacts under `.work/`. Readiness is 90 seconds, input
confirmation 20 seconds, quit 15 seconds, and each QMP operation 20
seconds.

`make test` runs `cargo test --locked` plus `harness/test-qmp.py` and
`harness/test-guest-lock.py`; coverage is owned by `src/server/tests.rs`
and `tests/cli.rs`. Bad CLI exits 2 before opening devices.
`make audit` in `harness/audit.sh` rejects forbidden features, forbidden
linked libraries, and any `unsafe` in `src/`.

Seat is keyboard-only with a US keymap (`src/input.rs`); globals and
version handling are owned by `src/server.rs`. HUP, INT, TERM, and QUIT
shut down the loop and reap foot (`src/shutdown.rs`, `src/main.rs`).
Normal sessions validate user-owned `XDG_RUNTIME_DIR`, emit
`TINYLAND_COMPOSITOR_READY` after socket, keyboard, discovery, and spawn,
and end with `TINYLAND_SESSION_END` when the primary client exits. Proof
fallback without `-- COMMAND...` remains harness-only. Laputa boundaries,
seat seams, and replacement gates are in `docs/laputa-integration.md`.

On a fresh checkout run `bash harness/build-guest.sh` first: it prepares
the native library and XKB data that `.cargo/config.toml` points at.

Host builds link libseat (`apk add libseat-dev` on Alpine); the musl
binary links it dynamically with pure-Rust KBVM for XKB and no
libxkbcommon link, so the guest carries libseat, libelogind, libcap,
and libgcc plus XKB data lookups. `make audit` allows
`unsafe` only in `src/seat.rs`, the reviewed libseat FFI surface,
and rejects any libxkbcommon link or runtime xkbcommon crate.

`make seat` runs the brokered proof in `harness/seat-proof.sh`: seatd
mediates as root while tinyland and foot run as the unprivileged `tiny`
user. The guest asserts direct DRM open fails for that user
(`TINYLAND_DIRECT_DENIED`), opens the seat through libseat
(`TINYLAND_SEAT_OPEN`), then reuses the smoke input marker and screenshot
gates. Disable and resume paths are implemented in `src/server.rs` but
qualified separately from daemon loss, which is a hard error here.
