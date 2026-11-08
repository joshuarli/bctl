# tinyland agents

Minimal Rust Wayland kiosk: one fullscreen toplevel, software compose, DRM
scanout, foot client, proven in a QEMU/KVM guest.

Commands: `bash harness/build-guest.sh` on a fresh checkout, then
`cargo check --locked`, `cargo test --locked`, `make smoke`,
`make wayland-pixels`, `make drm-proof`, `make drm-parity`,
`make negative`, `bash harness/audit.sh`.

Invariants: Linux-only single-threaded poll loop with no async runtime.
Pure-Rust `wayland-server` backend only, never Smithay, wlroots, libwayland, libdrm, Mesa, EGL, GBM, Pixman, systemd, D-Bus, or XWayland.
Try atomic modeset then legacy fallback only on ENOSYS/EOPNOTSUPP (EINVAL also allows fallback at capability stage); both paths share page-flip presentation and must show identical pixels.
Client protocol violations disconnect with an error, internal invariant failures assert, never panic on client input.
Validate shm offsets, strides, sizes, and object lifetimes before use, copy shm at commit, and never let scanout reference client-writable memory.
Fire frame callbacks only on confirmed kernel flip and complete nothing on a failed flip.
Send QMP input as one 14-event transaction, exit 75 on an uncertain outcome, and never resend.
Exit 2 on bad CLI before opening devices, and shut down on HUP/INT/TERM/QUIT while reaping foot.
Keep `src/` free of `unsafe` except the single reviewed libseat FFI in
`src/seat.rs`; isolate the single `libxkbcommon` dependency to input.
Treat guest-only root on `/dev/dri` and evdev as test harness, not a production seat claim.

Map: proofs, guest image, thresholds, timeouts, modeset, and presentation in `docs/verification.md`.
Entry points are `src/main.rs`, `src/server.rs`, `src/output.rs`, `src/input.rs`, `harness/run-proof.sh`, `guest/init`, and `guest/test-app`.
