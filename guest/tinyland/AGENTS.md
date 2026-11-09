# tinyland agents

Minimal Rust Wayland kiosk: one fullscreen toplevel, software compose, DRM
scanout, foot client, proven on SmolVM arm64.

Commands: `cargo check --locked`, `cargo test --locked`, `make audit`,
and `sh ../harness/smolvm-proof.sh` (also `make proof`).

Invariants: Linux-only single-threaded poll loop with no async runtime.
Pure-Rust `wayland-server` backend only, never Smithay, wlroots, libwayland, libdrm, Mesa, EGL, GBM, Pixman, systemd, D-Bus, or XWayland.
Try atomic modeset then legacy fallback only on ENOSYS/EOPNOTSUPP (EINVAL also allows fallback at capability stage); both paths share page-flip presentation and must show identical pixels.
Client protocol violations disconnect with an error, internal invariant failures assert, never panic on client input.
Validate shm offsets, strides, sizes, and object lifetimes before use, copy shm at commit, and never let scanout reference client-writable memory.
Fire frame callbacks only on confirmed kernel flip and complete nothing on a failed flip.
The SmolVM harness sends discrete VNC keysym press/release events and exits non-zero on any failed marker, pixel, denial, or timeout.
Exit 2 on bad CLI before opening devices, and shut down on HUP/INT/TERM/QUIT while reaping foot.
Keep `src/` free of `unsafe` except the single reviewed libseat FFI in
`src/seat.rs`; isolate the single `libxkbcommon` dependency to input.
Treat guest-only root on `/dev/dri` and evdev as test harness, not a production seat claim.

Map: proofs, guest image, thresholds, timeouts, modeset, and presentation in `docs/verification.md`.
Entry points are `src/main.rs`, `src/server.rs`, `src/output.rs`, `src/input.rs`,
`../harness/smolvm-proof.sh`, `../harness/image/tinyland-session`, and
`guest/test-app`.
