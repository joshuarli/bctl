# Linux x86_64 results, 2026-11-11

From threadripper (AMD Ryzen 9 9950X, Alpine Linux edge 7.2.9), in reply to
[the Linux handoff](2026-11-11-linux-x86-64.md). Pull `main` at `c9db778` or
later.

## Done

- **Interop on Linux Nanos.** `docs/compatibility.md` has a `linux-nanos`
  tuple. It passed 105 of 105 in 2 of 6 runs; the other four hit the
  intermittent guest stall described below.
- **Roadmap.** M8C's status records the Linux leg.
- **arm64 pin (`6308222`).** `runtime/build-nanos-arm64.sh` now pins Hydrogen
  `9119ba3e`. It merges `bbeea356` and `df949087`, and its only change over
  `bbeea356` is the FIONREAD fix. It has not been built.
- **Orphaned VMs on remove (`c9db778`).** Each Nanos supervisor locks
  `supervisor.lock` in its machine directory and writes its process group
  there. When the control socket is missing or refuses connections,
  `remove()` signals that group and waits for the lock to be released before
  deleting anything. One gap remains: the lock proves only that the
  supervisor is alive. A supervisor killed with SIGKILL can still leave its
  VMM behind, because std cannot pass the lock to the VMM.
- **Interop script fixes.** A half-built browser-use venv no longer blocks
  later runs (`5428dbe`), and Linux receipts now print the image hash
  (`3b29402`).

## Verify on the Mac

- Build the arm64 kernel at the new pin. Rerun the mac-nanos tuple and
  update its row.
- `supervisor_macos.rs` now takes the lock. Run
  `cargo test -p bctl-runtime-nanos` there. That includes
  `remove_stops_a_supervisor_whose_control_socket_is_gone`, which only
  type-checks from Linux.

## Open

- **Intermittent guest browser stall.** Helium in the guest stops answering
  CDP while the VM stays up. The guest's `Browser.close` fails with `EAGAIN`,
  and Helium ignores SIGTERM until it is killed. Callers see a gateway 502
  and a 120 s `bctl run` hang. This is likely the same stall as the Mac's
  owner-death relaunch timeout.
  - It did not reproduce in 8 idle up/probe/down cycles or in 8 provider
    open/close cycles.
  - It hit 3 of 6 full suite runs at host load averages between 4 and 25.
  - Hydrogen `bea1e222` passed its one run, which is too few to call a
    regression.

  Kernels for every x86-affecting commit from `bea1e222` to `df949087` are
  built under `.work/tmp/k-<commit>` on threadripper, ready for a bisect.
  The next step is a reproduction that fails reliably, and the full suite
  is the only one found so far.
- **Owned provider session outlives `close`.** This happened in 2 of 6 runs,
  once without the stall. agent-browser still prints "Browser closed". It
  did not reproduce in isolation, and no failing run kept its daemon log.
- **Linux measurement** (`docs/performance.md#external-clients`). This is
  waiting for a quiet host. An xsh campaign shares threadripper and cannot be
  paused, and `perf-gate-ready` passed even at a load average of 21.
- **Files directory on Nanos.** The proposal is virtiofs: libkrun already
  supports it, and QEMU supports it through `virtiofsd`. It needs a new
  guest driver in Hydrogen. No decision has been made yet.
- **Host tooling on threadripper.** The git build in
  `~/.local/share/git/current` has its exec path set to `/usr/bin`, so
  `git submodule` fails and browser-use cannot install. These runs set
  `GIT_EXEC_PATH=$HOME/.local/share/git/current`.

## Carried over

- Existing Linux Nanos homes still need `<image>.kernel.img`.
