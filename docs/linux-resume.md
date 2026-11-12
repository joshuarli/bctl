# Linux handoff, 2026-11-12

Linux qualification is incomplete. bctl `6f27d89` pins Hydrogen `47bc0255`.
The [campaign report](campaign-report.md) records the repairs and checks;
[bctl compatibility](../../bctl/docs/compatibility.md) records interop.

## Done

- Quiet-host measurements: the external-client run, and G4.2's 300-boot
  timing.
- vsock, 9p, mutex, tmpfs page-cache and vmap lock repairs.
- Since `dd0ba2c7`:
  - the procfs `fstat` halt;
  - both klib dependency races;
  - page-cache commits under spinlocks;
  - the unlocked symbol table;
  - the futex requeue reference race.
- Interop at `47bc0255` passes 113 of 113 in the last three runs.

## Remaining work

Four parallel `bctl up`/`bctl down` loops fail about once per 500 cycles on
the copied profile and once per 300 on fresh profiles (`FRESH=1`). Fix in
this order and rerun both loop kinds after each repair:

1. **Null scheduler task.** `sched_enqueue` finds a null element in a run
   queue. Find what writes into the queue's vector before changing code.
2. **Idle hang after `bctl-guest: ready`.** Both CPUs sleep while the host's
   check request times out. Suspect a lost vsock wakeup.

Then repeat interop twice and refresh final gate acceptance. macOS and
AArch64 were not tested in this Linux session.

## Tools

- **Loop driver:** `../bctl/.work/tmp/parallel-shutdown.sh REV LANES` pins bctl
  to a Hydrogen revision and rebuilds the runtime and image. It then runs
  `linux-shutdown-capture.sh` in each lane and stops every lane at the first
  failure.
- **Lane evidence:** each lane copies the guest console to
  `diagnostics/live-*` once a second. On a stall it captures every CPU stack
  through QMP and gdb (`capture-stacks.py`), and it leaves the failed lane's
  home in place.
- **Pin after a loop run:** the driver rewrites `runtime/build-nanos.sh`.
  Restore the committed pin when you finish.
- **Debug probes:** the unpushed branches `debug/scsi-stale-completion`
  (`.work/wt/scsi-debug`) and `debug/null-get` (`.work/wt/get-debug`) carry
  them. Neither is for release:
  - a stale virtio-scsi completion check;
  - recording of the vmap lock owner;
  - a report when a context suspends while holding the vmap lock;
  - SCSI probe logging;
  - `MEMDEBUG_MCACHE`, with a write-after-free check on reallocation;
  - on `debug/null-get`: a report when a context suspends holding the vmap
    or page-cache global lock, and a double-completion check for
    virtio-scsi requests.

  The write-after-free check produced false positives on 160-byte chunks;
  review it before relying on it.
- **Original profile:** `../bctl/.work/tmp/linux-shutdown-5994/profiles/shutdown-proof/nanos-profile.img`.
  Copy it into each new test home and keep the original unchanged.

## Build constraints

Use `PATH="$HOME/.cargo/bin:$HOME/usr/bin:$PATH"` for bctl builds.
Set `GIT_EXEC_PATH="$HOME/.local/share/git/current"` for submodule operations,
and keep `TMPDIR` under the repository's `.work/tmp`.
`VIRTIOFSD=/home/josh/d/bctl/.work/runtime/nanos/bin/virtiofsd` and
`VHOST_DEVICE_VSOCK=/home/josh/d/bctl/.work/runtime/nanos/bin/vhost-device-vsock`
enable the filesystem and vsock suites. The Helium archive directory is
`/home/josh/d/chromium-build/.work/artifacts/helium-0.20.3.1-r5`.

Run builds and VM suites sequentially when they share artifacts. Do not edit a
script while it runs, and do not rebuild a kernel or image while a VM uses it.
