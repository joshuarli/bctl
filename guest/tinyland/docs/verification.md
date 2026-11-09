# Verification reference

Host is macOS Apple Silicon. The proof runs a Linux arm64 guest on SmolVM,
not on the x86_64 virtual-machine harness laputa keeps. SmolVM must be the
patched build that accepts classic 2D virtio-gpu resources; the stock
release does not, and tinyland's scanout needs them. The harness defaults
to the main checkout's `.work/runtime/smolvm/smolvm` and accepts
`SMOLVM=<path>`.

## Harness

`guest/harness/smolvm-proof.sh` builds `guest/harness/image/Dockerfile` for
`linux/arm64`, saves the image archive, creates a machine with `--gpu`,
starts it with `SMOLVM_DISPLAY` and `SMOLVM_VNC` on loopback, and always
stops and deletes it. The image's `/usr/local/bin/tinyland-session`
entrypoint:

- creates `/dev/input/event*` from `/sys/class/input/*/dev` with `mknod`;
  the container gets no input nodes otherwise;
- keeps `/dev/dri/card0` and the event nodes `root:root 0600`;
- starts the vendored `/usr/local/bin/seatd` as root with
  `SEATD_VTBOUND=0` and `SEATD_SOCK=/run/seatd.sock`, then waits for the
  socket and logs `TINYLAND_SEATD_BROKER`;
- runs `/usr/local/bin/tinyland --seat --smoke --width 1024 --height 768
  --socket wayland-0 -- foot /usr/local/bin/guest-test smoke` as the
  unprivileged `tiny` user (uid 1000, group `seat` only), with
  `LIBSEAT_BACKEND=seatd`, `XDG_RUNTIME_DIR=/run/user/1000`, and
  `TINYLAND_XKB_ROOT=/usr/share/X11/xkb`.

The connector's preferred mode from `SMOLVM_DISPLAY` controls scanout; the
requested 1024x768 is a lower bound. The image carries Alpine's foot,
libgcc, fonts, and XKB data, plus the vendored seatd binary. It installs
no Alpine `seatd` or `libseat` package. Tinyland's native seatd client
links no libseat, and its static musl binary needs no loader or shared
libraries in the guest.

`guest/harness/rfb.py` is a stdlib RFB 3.8 client. It negotiates security
type None, requests Raw encodings, accumulates a 1280x800 framebuffer,
types the laputa handshake word as discrete keysym press/release events,
and compares frames.

## Gates

The proof passes only when all of these hold:

- `TINYLAND_SEAT_OPEN`, `TINYLAND_DRM_ORIGIN seat:/dev/dri/card0`,
  `TINYLAND_DRM_OK`, `TINYLAND_COMPOSITOR_READY`, `TINYLAND_SURFACE_MAPPED`,
  and `TINYLAND_PRESENTED` appear in the compositor log, with no
  `TINYLAND_SESSION_END`, `TINYLAND_CHILD_EXIT`,
  `present failed`, `seat open failed`, `brokered display init failed`,
  `foot spawn failed`, or `seat dispatch failed`.
- The first captured frame has at least 0.5% non-black pixels
  (`R+G+B > 24`).
- Sending `laputa` plus Return over VNC changes at least 200 pixels and
  raises the magenta fraction (`R > 180, B > 180, G < 120`) by at least
  0.0002. The marker appears only after `guest/test-app` reads the exact
  `laputa` line, so this ties the pixel change to the handshake word.
- The running `tinyland` process has uid 1000.
- `su tiny -c 'exec 3<>/dev/dri/card0'` fails with `Permission denied`
  while tinyland holds the card through seatd.

Readiness allows 90 seconds, keyboard confirmation 20 seconds, and the VNC
connection 30 seconds. Frames and logs land under `.work/harness/`. The
last accepted run printed:

```text
VNC_FIRST_FRAME nonblack_frac=1.000000 avg_rgb=108.7 magenta_frac=0.000000
VNC_KEY_CHANGE changed_pixels=5228 magenta_frac=0.001174 magenta_delta=0.001174
SEAT_MEDIATED_DRM_OK TINYLAND_SEAT_OPEN TINYLAND_DRM_OK
TINYLAND_UNPRIVILEGED uid=1000
DIRECT_OPEN rc=1 output=sh: can't create /dev/dri/card0: Permission denied
SMOLVM_TINYLAND_PROOF_OK
```

## Cargo tests

The workspace crates test inside a linux/arm64 Alpine image with the pinned
`nightly-2026-09-15` toolchain and prefetched locked dependencies:

```sh
docker build --platform linux/arm64 --target tests \
  -f guest/harness/image/Dockerfile -t bctl/tinyland-smolvm-tests:local guest
docker run --rm --platform linux/arm64 --network none -w /src/seatd \
  bctl/tinyland-smolvm-tests:local cargo test --locked
docker run --rm --platform linux/arm64 --network none -w /src/tinyland \
  bctl/tinyland-smolvm-tests:local cargo test --locked
docker run --rm --platform linux/arm64 --network none -w /src \
  bctl/tinyland-smolvm-tests:local cargo test -p seatd-protocol --locked
```

The last run passed all seatd suites (45 library, 31 adversarial, 2
differential, 4 storm with the idle timing case ignored), the shared
protocol suite (13), and all tinyland suites (46 library, 1 CLI). The idle
timing case stays ignored; it needs a `perf-gate-ready` host measurement.

## WLCS

`harness/wlcs/run.sh` builds Wayland Conformance Test Suite and a test-only
cdylib adapter in an arm64 Alpine container and runs the suite against
tinyland's `headless` feature. Neither ships in the guest. Method and results
are in `../../../docs/research/tinyland-wlcs.md`.

## Presentation

Modeset tries atomic then legacy. Capability fallback allows ENOSYS,
EOPNOTSUPP, and EINVAL; commit fallback allows only ENOSYS and EOPNOTSUPP.
Both paths share the `page_flip` presentation in `src/output.rs`.

The compositor keeps two scanout buffers, writes the back buffer, requests
a flip event, and completes it in the shared poll loop within two seconds
for the matching kernel event before reusing the front buffer and firing
frame callbacks. Display, socket, DRM, and input are polled together, so
seat disable, shutdown, and device failure stay responsive during flip
wait. `TINYLAND_PRESENTED` marks completion. Client shm is copied at commit
in `src/server.rs`, then the buffer is released, so scanout never depends
on client-writable memory.

The seat has a keyboard with a US keymap and, when an evdev device has
`BTN_LEFT` with absolute or relative axes, a pointer (`src/input.rs`).
Absolute axes are scaled from the device range to the output, so the
SmolVM VNC tablet lands on the pixel the viewer clicked. Pointer focus
follows the topmost surface under the cursor except while a button is
held, a press outside the open popups sends them `popup_done`, and
`wl_pointer.set_cursor` surfaces are drawn at the hotspot for the focused
client. Every toplevel is configured maximized at the output size; the
newest mapped one is shown, activated, and holds keyboard focus, and
when it closes the previous one is reactivated. Popups are placed by
`src/positioner.rs` (flip, then slide, then resize) inside the output.
Globals and version handling are owned by `src/server.rs`. HUP, INT, TERM, and QUIT
shut down the loop and reap foot (`src/shutdown.rs`, `src/main.rs`).
Normal sessions validate user-owned `XDG_RUNTIME_DIR`, emit
`TINYLAND_COMPOSITOR_READY` after socket, keyboard, discovery, and spawn,
and end with `TINYLAND_SESSION_END` when the primary client exits. The
proof fallback without `-- COMMAND...` remains harness-only. Laputa
boundaries, seat seams, and replacement gates are in
`docs/laputa-integration.md`.

## Build and audit

`guest/` is one Cargo workspace. `guest/.cargo/config.toml` selects `mold`
for the musl targets; tinyland keeps the target's default `crt-static`, so
the aarch64 release has no interpreter and no `DT_NEEDED`, including no
libseat. `make audit` checks the workspace release binary path
(`../target/x86_64-unknown-linux-musl/release/tinyland`) and rejects
forbidden dependencies, any dynamic dependency, and any `unsafe` in
`src/`; build that binary first when running the audit.

`make test` runs `cargo test --locked`; coverage is owned by
`src/server/tests.rs`, `src/seat.rs`, and `tests/cli.rs`. Bad CLI exits 2
before opening devices.

## Laputa history

Laputa's x86_64 proof used its own QEMU/KVM and QMP harness, which lives in
laputa. It is not part of this repository and this repository does not run
it. `docs/laputa-integration.md`, `docs/size-perf.md`, and
`docs/opengl-probe.md` record that laputa-side profile.
