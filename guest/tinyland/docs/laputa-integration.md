# Laputa integration

Target: unprivileged tinyland session, seatd-mediated devices, Ghostty
`1.4.1` in the Laputa image. `qemu-tinyland-ghostty` is the only graphical
profile. No tiling, pointer, or desktop scope change; GPU use is limited to
virgl rendering with a 2D scanout.

## Boundaries

| Component | Responsibility |
|---|---|
| `mdevd-rs` | Apply device policy, complete coldplug, publish handled-device events. |
| `seatd-rs` | Mediate DRM and input descriptors, activation and deactivation. |
| Tinyland | Discover suitable devices, request access through broker, render, deliver input. |
| XSH/xinit | Boot ordering, supervision, user-session startup, recovery policy. |

Tinyland does not couple to `mdevd-rs` internals. Fixed QEMU profile uses
completed coldplug plus deterministic discovery. No libinput or libudev-zero
in tinyland for this contract.

## Seat client

Use libseat built seatd-only (`-Dlibseat-logind=disabled
-Dlibseat-seatd=enabled -Dlibseat-builtin=disabled`), `LIBSEAT_BACKEND=seatd`.
No automatic fallback when broker connection fails. Daemon loss never falls
back to direct open.

`src/seat.rs` is the single `unsafe` module: it links libseat at build
time (`#[link(name = "seat", kind = "dylib")]`) and declares the eight
used symbols against the pinned 0.9.3 header (all present in the guest's
0.8.0). The listener and its state are boxed because libseat keeps the
listener pointer past the open call; both stay on the owning thread.
`Session::open` enforces `LIBSEAT_BACKEND=seatd` before touching the
library. Dispatch runs every main-loop iteration with timeout 0; any
dispatch error is fatal. A zero-timeout `libseat_dispatch` reports a dead
daemon as an empty read, so dispatch polls the session fd for hangup first
and treats it as daemon loss. The enable path runs only from the suspended
state, so the initial activation event never double-opens devices.

Seams in tree:

- `src/output.rs`: `DrmOutput::open_from_file` takes broker `OwnedFd`.
  `Card::from_file` wraps without reopen. Caller keeps seat session alive
  while `Card` is held and drops scanout on disable.
- `src/input.rs`: `device_from_fd` validates keyboard, sets nonblocking,
  grabs. `open_keyboard` is explicit-path variant. `reset_stuck` clears
  modifiers on resume.
- Lifecycle: submit frame, return to shared poll, complete on matching DRM
  event. Disable, shutdown, and device failure stay responsive during flip
  wait. Resume reestablishes input without stuck modifiers and without
  reusing revoked descriptors.

## Session contract

Normal operation: explicit `-- COMMAND...`, validated user-owned
`XDG_RUNTIME_DIR`, readiness after socket, keyboard, discovery, and spawn,
exit `0` with `TINYLAND_SESSION_END` when primary client exits. Restart
policy belongs to xinit.

Proof operation: `foot /usr/local/bin/guest-test` fallback remains for
`harness/run-proof.sh` only. `TINYLAND_SPAWN_FOOT ... (legacy fallback)`
marks it.

Validation: `XDG_RUNTIME_DIR` must exist, be absolute directory, owned by
euid. Non-`0700` logs `TINYLAND_RUNTIME_DIR_PERMS` and continues for guest
`/tmp` compatibility; Laputa sessions use `0700` (`/run/user/0`).

Readiness: `TINYLAND_DRM_OK`, `TINYLAND_SOCKET`, keyboard logs,
`TINYLAND_SPAWN_COMMAND`, then `TINYLAND_COMPOSITOR_READY socket=...`.

## Presentation

`queue_present` writes back buffer, dirties, requests flip event, arms
two-second deadline. Main loop polls display, socket, DRM, and input
together. `poll_flip` completes on matching CRTC event, fires
`TINYLAND_PRESENTED`, clears pending, then fires frame callbacks. Timeout or
wrong CRTC is failure. `present_pixels` blocking wrapper remains for
`drm-proof` only.

## Devices and output

Default `--drm-device /dev/dri/card0`. Selection prefers connected
connector, logs `TINYLAND_CONNECTOR_FALLBACK` otherwise. CRTC prefers
current or compatible encoder, logs `TINYLAND_CRTC_FALLBACK` when using
first resource CRTC. Fixed VM requires explicitly selected device and
validated compatible output; multimonitor discovery is out of scope.

## Release packaging

Musl binary is static; `readelf -d` shows no `NEEDED` beyond `ld-musl`.
`libxkbcommon.a` via `-Lnative=.work/guest-root/usr/lib` and
`XKB_CONFIG_ROOT=.work/guest-root/usr/share/X11/xkb` are proof-only build
inputs. The seat build links libseat dynamically (static musl cannot
dlopen), so the binary carries `libseat.so.1`, `libelogind.so.0`,
`libcap.so.2`, and `libgcc_s.so.1` atop the loader; XKB stays statically
linked. Release uses the system `libseat` link path and installed XKB data
(`/usr/share/X11/xkb` from `xkeyboard-config`). `TINYLAND_XKB_ROOT` or
`XKB_CONFIG_ROOT` overrides include path at runtime. Installed artifact must
run without source checkout or `.work`. Host builds need the system
libseat dev package (`apk add libseat-dev` on Alpine) for
`.cargo/config.toml`'s `/usr/lib` search path.

## Gates for `qemu-tinyland-ghostty`

1. Unprivileged operation: tinyland and Ghostty as ordinary user, direct opens
   denied, broker-mediated succeeds, no direct fallback on broker loss.
2. Exact chain: Laputa kernel, tinyland artifact, Ghostty artifact, brokers
   produce real input to changed pixels to confirmed presentation with
   post-input marker.
3. Lifecycle: app exit, compositor crash, broker loss, shutdown, supported
   device loss have bounded observable outcomes. VT disable and resume
   qualified separately when VT-bound.
4. Measured dependencies: actual ELF closure shows which packages leave the
   image. Ghostty keeps libEGL, wayland client and cursor, libxkbcommon, and
   fonts; Mesa provides virgl. Remove only unreachable packages.

`make seat` proves gates 1 and the device half of 2 in tinyland's own
guest (Alpine foot 1.17.2, C seatd 0.8.0): denial marker, seat open,
brokered devices, post-input marker with confirmed presentation. The
`qemu-tinyland-ghostty` Laputa profile proves gates 1 and 2 against the
exact Laputa chain (Ghostty 1.4.1, Rust brokers, Laputa image):
unprivileged session, denial marker, brokered DRM/input, real input to
changed pixels to confirmed presentation, plus the measured closure for
gate 4. Lifecycle qualification covers compositor crash, broker loss,
single-device loss, application exit, and scanner shutdown.

## Sequencing

Qualify tinyland against the C seatd reference, substitute `seatd-rs`,
substitute `mdevd-rs`, then qualify the complete Rust combination. The dwl
reference and the foot profiles were removed after the Ghostty profile was
promoted. `doas-rs` does not block display devices.
