# Linux handoff, 2026-11-12

Linux qualification is incomplete. The kernel revision bctl pins is
`dd0ba2c7`. The [campaign report](campaign-report.md) records the repairs
and checks; [bctl compatibility](../../bctl/docs/compatibility.md) records
interop.

## Done

- **Quiet-host measurements:** the seven-round external-client measurement,
  and G4.2's 300-boot timing (median 0.275 s per boot).
- **vsock:** the receive-path TLB rendezvous deadlocks.
- **9p and mutex:** 9p dentry and fid ownership, and the mutex wakeup race.
- **tmpfs:** the page-cache `fs_reserve` use-after-free, with the
  `fsfile_lifetime` regression.
- **vmap lock:** the boot hang where `mmap` slept on a filesystem mutex while
  holding the vmap lock.
- **bctl `3b134a4`:** pins `dd0ba2c7`. Interop passes 113 of 113 in two of
  five runs.

## Remaining work

Repeated `bctl up`/`bctl down` on the copied failing profile still fails about
once per 170 cycles across four parallel loops. Fix these in order and rerun
the loops after each repair:

1. **Writes after free in early boot.** Evidence: a stale virtio-scsi `READ_16`
   completion closure, objcache free-list corruption, a run-queue task pointer
   off by one, and `table_find` faults. Find the writer before changing code.
2. **Epoll lock-order deadlock.** `release_epollfd` and
   `notify_dispatch_with_arg` deadlock during shutdown.
3. **Idle hang after `bctl-guest: ready`.** Both CPUs sleep while the host's
   check request times out. Suspect a lost vsock wakeup.

Then repeat interop twice and refresh final gate acceptance. macOS and AArch64
were not tested in this Linux session.

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
- **Debug probes:** the unpushed branch `debug/scsi-stale-completion`, in
  worktree `.work/wt/scsi-debug`, carries them, each disabled for release:
  - a stale virtio-scsi completion check;
  - recording of the vmap lock owner;
  - a report when a context suspends while holding the vmap lock;
  - SCSI probe logging;
  - `MEMDEBUG_MCACHE`, with a write-after-free check on reallocation.

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
