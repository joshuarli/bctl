# Behavior rules

These rules are deliberate and tested. They define what the daemon
guarantees beyond the wire protocol.

- Socket location: `SEATD_SOCK` overrides `/run/seatd.sock`. The override
  supports isolated daemon instances for tests and guests.
- Opened device identity: a path whose device and inode differ from the
  opened descriptor fails with `EACCES`. The check runs after opening
  with `O_NOFOLLOW` and closes replacement races between path lookup
  and use.
- Opened device type: a descriptor that is not a character device
  fails with `ENODEV`. Classification checks the opened descriptor,
  never the path string alone.
- Symlinks: resolve the path first, classify the canonical prefix, then
  open with `O_NOFOLLOW`. Only paths whose opened descriptors fail the
  identity or type checks are rejected.
- Platform: Linux only. No BSD, macOS, or wscons support.
- Distribution: the foreground Rust daemon is the whole product. There
  is no seatd launcher, no embedded daemon, and no login, power,
  supervision, authentication, D-Bus, systemd, logind, or polkit
  integration.
- VT qualification: switching and graceful restoration passed an x86_64
  KVM guest proof. That proof covers Alpine Linux 6.6.142-0-virt with
  virtio devices, not every kernel or physical device.
- Shutdown socket cleanup: remove the path only if its device and inode
  match the socket captured at startup, so a replacement daemon's
  socket survives. The directory must remain administrator-controlled.
- VT setup failure: capture prior mode values and attempt rollback on
  setup failure, then disconnect the opening client. Rollback is best
  effort and does not prove restoration of kernel VT ownership. The
  guest proof covers successful switching and graceful restoration.
- Logging: Rust includes level, source file, and line. Logs are not
  wire-protocol goldens.

The corpus in `harness/protocol.golden` pins the exact exchange bytes
for ping, open and close, invalid device requests, stale IDs, invalid
switches, disable acknowledgement, switch event ordering, malformed
frames, and repeated opens. Transport tests cover fragmented and
coalesced frames and descriptor ownership. Successful device transfer,
kernel revocation, real VT operation, and compositor behavior require
the guest proofs in [validation](validation.md). Corpus checks alone do
not establish those hardware contracts.

One historical test failure remains unclassified: a single-test failure
during contaminated reruns. No fix is claimed for that report.
