# Linux handoff, 2026-11-12

This is the historical qualification checkpoint at Hydrogen `47bc0255` and
bctl `6f27d89`. Later kernel repairs in `5ece982a` and `e4aad9cc` address
page-cache destination faults and locked-heap management allocation. Recheck
the current code before resuming the investigations below; these later
commits do not constitute a new full qualification run.

The [campaign report](campaign-report.md) retains the measured results.
[bctl compatibility](https://github.com/joshuarli/bctl/blob/main/docs/compatibility.md)
records interop, and [TODO.md](../../TODO.md) holds the review findings.

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
- Since `47bc0255`:
  - the vsock idle hang (bctl `2792fcc`, a vhost-device-vsock patch);
  - the empty guest stop reply (bctl `a098bbb`);
  - copy-on-write faults under the page-cache locks (`5ece982a`);
  - unlocked allocation of locked-heap management (`e4aad9cc`), the
    source of the stale virtio-scsi completions;
  - every other unlocked general-heap allocation after the secondary
    CPUs start, including CPU 0's run queue (`0523dbbc`).
- Interop at `44f4b61f` (bctl `cbf338b`) passes 113 of 113 in 2 of 2 runs.

## Remaining work

Four parallel `bctl up`/`bctl down` loops at `e4aad9cc` ran about 2,300
cycles with one failure. At `0523dbbc` they ran 2,400 cycles with none.
Two faults are not yet closed:

1. **Null scheduler task.** `sync_complete` reaches `sched_enqueue`, and
   `pqueue_insert` compares a null element in a run queue. `0523dbbc`
   moved CPU 0's run queue off the unlocked general heap, the likely
   cause; close it after 5,000 more cycles without a failure.
2. **Browser ignores `SIGKILL` at shutdown.** Seen twice in about 1,500
   cycles, once on a control kernel without `5ece982a`. The guest logs that
   the browser ignored `Browser.close` and `SIGTERM`, then both CPUs idle
   and the host's 18 s stop read times out. The `debug/kill-watch` branch
   prints each thread's syscall and wait 3 s after a `SIGKILL`; it did not
   recur in 2,300 cycles with that probe.

Interop at `44f4b61f` passes 113 of 113 in 2 of 2 runs, and final gate
acceptance at `409d61e1` passes G0 through G5 (see `plan.md`). The long
loops were relaxed, so both faults stay open on that evidence and close if
they do not recur in use; HYD-3 and HYD-4 in `TODO.md` track them. The P
gate needs a quiet host. bctl now runs headed sessions on Linux through
SmolVM and keeps Nanos as its headless runtime; macOS and AArch64 work
continues from bctl's `docs/handoffs/2026-11-12-macos.md`.

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
- **Debug probes:** the unpushed branch `debug/kill-watch`
  (`.work/wt/kill-watch`) reports the threads of a process that outlives
  `SIGKILL`. The older unpushed branches `debug/scsi-stale-completion`
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
