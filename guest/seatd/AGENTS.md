# seatd-rs

Linux Rust implementation of the seatd foreground daemon (one seat, `seat0`). Deny nothing silently: every error path fails closed or is tested.

## Commands

```sh
cargo test --offline --locked
cargo clippy --offline --locked --all-targets -- -D warnings
bash harness/vm-qualification.sh transition|shutdown <guest-root> <kernel> <initramfs> <seatd-bin>
```

Musl targets use the `kache` wrapper and link with `mold`; see [validation](doc/validation.md).

## Invariants

- Exactly-once ownership of every FD; unexpected incoming FDs are closed.
- A logged revocation failure never clears the device's active flag.
- Second daemon never steals a live socket; stale sockets are reclaimed, non-sockets never unlinked.
- Linux only; no D-Bus, systemd, logind, or supervision logic.

## Map

- Socket admission and single-instance lock → `src/admit.rs`
- Frame buffers and descriptor transfer → `src/conn.rs`
- Session transitions → `src/session.rs`
- Devices, VT, effects → `src/device.rs`, `src/vt.rs`, `src/effect.rs`, `src/sys.rs`
- Wire protocol → `src/wire.rs`, golden transcript → `harness/protocol.golden`
- Guest qualification → `harness/VM.md`, raw guest client → `harness/vm-probe.c`
- Runtime contract, recovery, validation, behavior rules → `doc/runtime.md`, `doc/recovery.md`, `doc/validation.md`, `doc/behavior.md`
