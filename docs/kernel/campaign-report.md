# Linux verification report, 2026-11-12

This report covers the resumed Linux x86-64 work through kernel `0bb4a9d9`.
The campaign remains incomplete. The [handoff](linux-resume.md) records open
failures, pending bctl changes, and reproduction instructions.

## Committed changes

| Revision | Change | Decisive evidence |
| --- | --- | --- |
| `bc164807` | Convert the nonnegative CPU_SET index to `size_t` for musl | CPU fixture builds and passes at 1 and 2 CPUs |
| `17481814` | Deliver completion when a TLB flush has no pages | 8192 untouched anonymous and memfd map/unmap cycles lose 14336 KiB before repair and 0 KiB afterward |
| `aacbd732` | Retain 9p metadata while its parent tuple lists it | 200 close/stat/reopen/list cycles pass at 1 and 2 CPUs; ownership review remains open |
| `0bb4a9d9` | Service TLB flushes while waiting on page-cache node locks | Captured four-CPU rendezvous deadlock; renderer stress passes after repair |
| `0bb4a9d9` | Save the next address before protection changes can merge and free a vmap | Four-thread split/merge protection regression passes natively and at 4 CPUs |
| `0bb4a9d9` | Integrate bounded renderer stress and repeated probe commands | 300 cold boots pass; stress restores process count and stays within the 1 MiB memory allowance |

The four-CPU stress run performs 3200 warm-up launches and 3200 measured
launches. Its post-warm-up free-memory loss is 4 KiB. These are resource
correctness checks, not performance measurements.

The new harness interfaces are `dev/test.sh probe --repeat N`,
`dev/test.sh probe --stress`, and Linux x86-64 `dev/run.sh --9p ID:DIR`.
The probe's stress role accepts `--cycles=N` from 1 through 10000.
The 9p device uses a numeric tag, such as `--9p 1:share`, mounted with
`--mount virtfs1:/shared`. No persisted format or wire protocol changes here.

## Verification at 0bb4a9d9

All logs below are local evidence retained under Hydrogen `.work/tmp/`.
Source and shared artifacts stayed fixed during these final runs.

| Command | Result | Log |
| --- | --- | --- |
| `dev/test.sh probe --repeat 50` | 300 guest cold boots PASS: main and raw, each at 1, 2, and 4 CPUs; native references pass | `probe-repeat-50.log` |
| `dev/test.sh probe --stress` | PASS at 1, 2, and 4 CPUs | `probe-stress-verified.log` |
| `VIRTIOFSD=… dev/test.sh harness helium helium-stress` | PASS: 95 harness PASS records, browser 10/10 records, ten parallel browser runs pass | `linux-final-regression.log` |
| `dev/test.sh upstream external debug` | PASS against the declared upstream baseline; all external cohorts pass; syscall breakpoint reached | `linux-upstream-external-debug.log` |

The harness intentionally runs crash, missing-record, duplicate-record, and
timeout fixtures. Their expected rejection appears as harness PASS records.
The TCP RTT assertions pass in the final harness run. An earlier two-CPU run
exceeded the unchanged 50 ms threshold on the busy host. That earlier failure
remains in `.work/runs/20261112T003157Z-tcp_loopback-smp2-18023`.

The upstream result is 64 PASS, 6 FAIL, 2 TIMEOUT, and 6 CRASH across 78 runs.
The 14 nonpassing runs match the declared baseline at 1 and 2 CPUs:
`filesystem` faccessat2, `io_uring`, `mmap`, `netsock`, `sigloop`, `signal`,
and `tun`. No new upstream failure was observed.
Eight external gVisor cohorts pass natively and at 1 and 2 guest CPUs.
The cmsg cohort passes 18 checks and peercred passes three.

Additional focused evidence under `.work/runs/` includes:

- `20261112T004615Z-unmap-empty-clean-before-497`: clean before-repair memory loss.
- `20261112T004643Z-unmap-empty-fixed-*`: zero loss at 1, 2, and 4 CPUs.
- `20261112T005556Z-stress-vmap-fixed-19741`: 6400-launch four-CPU stress.
- `20261112T010446Z-p9-files-*`: wider filesystem operations under 9p at 1 and 2 CPUs.
- `20261112T011240Z-debug-19768/gdb.log`: syscall breakpoint proof.

The original deadlock stacks remain in `.work/tmp/stress-debug-backtrace.log`
and `.work/tmp/stress-debug-lock.log`.

## bctl integration

bctl is `5ac7b9e` with the uncommitted Linux kernel pin and diagnostic changes
listed in the handoff. These results apply to that dirty tree:

- Interop passes 113/113 once: `../bctl/.work/tmp/linux-interop-refreshed.log`.
- The tested image SHA-256 is
  `4dad4914281abaef6c257a79b4a438b252a3c5deafa79e3bbdc72c8142f18f9f`.
- `cargo test -p bctl-runtime-nanos` passes 14 tests. Three platform or
  integration tests remain ignored. Log: `linux-runtime-tests.log` in bctl scratch.
- An invalid interop suite exits with failure and retains nonempty console
  snapshots at `../bctl/.work/interop/failures/20261112T010556Z-28232`.

Subsequent shutdown loops reproduce both a timeout and an empty reply with
a guest kernel tuple-access fault. The successful interop run does not close
that issue. No performance result or speedup is established.

## Review and acceptance limits

The independent reviewer found no defect in the empty-flush completion,
page-cache lock, or vmap traversal repairs. The review identified two supported
9p ownership risks that still need runtime regressions and a repair.
The handoff describes both risks. The terminal review is retained at
`.work/tmp/linux-review-terminal.json`.

G4's repetition and resource-stress checks now pass. Full campaign acceptance
remains open because 9p ownership, bctl shutdown, and external-client measurement
work remain. Perf gate deferred: host busy. The earlier browser-use navigation
timeout is also unclosed. No macOS or AArch64 qualification is claimed.
G4.2's required wall-time and per-boot median report remains outstanding.

## Quiet-host resumption, 2026-11-12

The seven-round Linux external-client measurement completed with both
`perf-gate-ready` checks passing. The samples and configuration are recorded
in [bctl performance](../performance.md#linux-x86_64-2026-11-12).
Raw evidence: `.work/tmp/resume-external-measure.log` and
`resume-measure-gate-{before,after}.log`. No improvement is claimed.

G4.2 repetition at Hydrogen `88561473` passes all 300 cold boots in 120.46 s
wall time, including setup and native references. The per-boot median is
0.275 s, with range 0.12–0.84 s. Each sample covers QEMU start through guest
exit. Both quiet-host checks pass. The main and raw probes each run 50 times
at 1, 2, and 4 CPUs. Evidence: `.work/tmp/resume-g4-repeat.log`,
`resume-g4-gate-{before,after}.log`, and `.work/runs/20261112T0336*` through
`20261112T0338*`. This closes the repetition timing report for that revision.

The vsock COW fixture reproduces a TLB rendezvous deadlock before repair.
One CPU spins in `vsock_events_internal` with interrupts disabled. The other
waits in `flush_entry_post`. Making socket-lock acquisition service TLB
flushes repairs that two-CPU case. Four CPUs expose a second waiter in
`virtio_sock_rx_complete`'s packet-ordering loop. That loop must also service
TLB flushes while waiting for an earlier receive to complete.

`VHOST_DEVICE_VSOCK=/path/to/vhost-device-vsock dev/test.sh vsock` now runs
100 connections at each of 1, 2, and 4 CPUs. Each connection forks a child,
receives 256 KiB into COW pages while another thread polls the socket,
validates every byte, and checks the parent's pages remain unchanged.
The original kernel passes at one CPU and times out at two. The socket-only
repair passes at two and times out at four. Both repairs together pass all
three configurations. Evidence: `.work/tmp/resume-vsock-before.log`,
`resume-vsock-after.log`, `resume-vsock-after2.log`,
`resume-vsock-cow/stacks.log`, and `resume-vsock-four-stacks.log`.
This proves the vsock deadlocks. Browser shutdown qualification remains open
until the refreshed image passes repeated shutdown and interop.

bctl `f17507d` commits failure retention and the Linux measurement report.
Three filesystem fault-injection tests prove that snapshot and retention
errors cannot prevent shutdown cleanup or replace the original failure code.

## 9p ownership and mutex wakeup repair

The distinct-file fixture retains 128 host server descriptors before repair.
Host deletion of an open guest file followed by directory refresh also crashes
before repair. Dentries now count live fsfiles independently of temporary pins.
Directory refresh, unlink, and rename replacement detach referenced dentries;
the final fsfile release destroys a detached dentry or clunks a cached fid.
Metadata still listed by its parent tuple remains cached.

The wider fixture exposed a mutex wakeup race during asynchronous close.
After a woken waiter loses acquisition to another context, its repeated
bottom-half rescheduling can starve interrupts on one CPU. The lock holder is
suspended in `v9p_request`; the waiter is in `p9fsf_sync_complete`, with 849
acquisition spinouts and an empty wait queue in the captured state. The
waiter now rejoins the mutex wait queue after its bounded acquisition spin.
This allows the interrupt and RPC completion to run.

`dev/test.sh 9p` passes at one and two CPUs, including 200 close/stat/reopen
cycles, zero retained descriptors after 128 distinct files, host deletion while
open, and 32 guest unlink and rename-replacement cycles with old and new data
checked. Before/after logs are `.work/tmp/resume-p9-before.log`,
`resume-p9-final2.log`, `resume-p9-mutex.log`, and `resume-p9-32.log`.
The mutex capture is `resume-p9-lock.log`; passing guest runs are
`.work/runs/20261112T040151Z-p9-mutation-*`. Their receipts identify the dirty
`4f508a09` parent and kernel image SHA-256
`08cf4f27b58a6d475b6d0d5796575361df2908dfddca0824cb5f040f2be0835c`.

The bctl image built from `4f508a09` completes 100/100 shutdown cycles using
a copy of the original failing persistent profile, with guest instrumentation
removed. Evidence: `.work/tmp/resume-shutdown-vsock.log`. The original profile
remains preserved. Full interop on the final ownership repair is still pending.

That 100/100 run did not repeat. The same profile and loop fail at
`4f508a09`, `534f1052`, and `eeda1d00` after 7 to 95 cycles, so the earlier
result does not qualify shutdown.

## Page-cache lifetime and vmap lock repairs

Ten parallel Helium guests crashed one guest in `pagecache_node_free`. It
read the `fs_reserve` closure of a tmpfs file that had already been freed.
Every filesystem except tfs embeds that closure in its file object and frees
the object right after `pagecache_deallocate_node`, while the node is freed
later. `eeda1d00` releases `fs_reserve` before the caller's file is freed.
The `fsfile_lifetime` fixture churns memfds while unix datagrams refill freed
heap objects. It crashed in 3 of 4 runs before the repair and passes 28 of 28
after. The harness, `helium`, `helium-stress` (10 of 10 parallel), `upstream`,
and `external` suites then pass; the 14 declared upstream failures are
unchanged. `2ecb3ae4` drops the per-run QMP socket. Its path exceeds the
108-byte `sun_path` limit in deep checkouts, and no committed tool reads it.

bctl attaches two disks to one virtio-scsi controller and boots Chromium from
a persistent profile. Four parallel `bctl up`/`bctl down` loops on copies of
the failing profile expose faults that the Hydrogen suites do not. A debug
build recording the vmap lock owner, plus a check for any context suspending
while it holds that lock, caught `mmap` suspending in `mutex_lock` through
`file_perms` → `filesystem_get_meta`. `mmap` read file metadata under the
irq-disabled vmap spinlock. When the filesystem mutex was contended, the
holder parked while every fault and user-memory check spun. `dd0ba2c7` reads
the permissions before taking the lock. It also defers a final file close
from `deallocate_vmap_locked`, which takes the same mutex. The hang appeared
in three runs before the repair and in none of about 400 lane-cycles after
it. No fixture reproduces it: two attempts passed on the unrepaired kernel,
so none was committed.

## Linux qualification at `dd0ba2c7`

bctl `3b134a4` pins `dd0ba2c7`. Interop passes 113 of 113 in two of five runs.
The failures are recorded in
[bctl compatibility](../compatibility.md#known-failing-or-unsupported-cases).
The parallel up/down loops fail about once per 170 cycles. Faults captured
there, all still open:

- Writes after free in early boot. They show up as a stale virtio-scsi
  `READ_16` completion closure, objcache free-list corruption
  (`f->head < o->objs_per_page`), a run-queue task pointer off by one, and
  faults in `table_find`. `MEMDEBUG_MCACHE` makes them more frequent; its
  redzones have not caught the writer.
- A lock-order deadlock between `release_epollfd` and
  `notify_dispatch_with_arg` during shutdown.
- An idle hang. The guest prints `bctl-guest: ready`, then both CPUs sleep
  in `kernel_sleep`, while the host's check request times out.

The debug probes are on the unpushed branch `debug/scsi-stale-completion`.
The loop drivers are `../bctl/.work/tmp/parallel-shutdown.sh` and
`linux-shutdown-capture.sh`. Shutdown qualification and final gate
acceptance remain open. macOS and AArch64 were not retested.

## Boot and shutdown faults after `dd0ba2c7`

Fresh bctl profiles failed within two up/down cycles, which made the
remaining faults quick to capture. Each repair below was checked with four
parallel loops.

- `89012978`: `fstat` on a `/proc/<pid>/…` descriptor whose process had
  exited halted the guest ("cannot get from unknown value"). This was the
  shutdown fault in the original handoff. The `proc_fstat_exited` fixture
  halts before the repair and passes after it.
- `66c581ca`, `3b321a83`: autoloaded klibs raced their own dependency check.
  shmem failed with "missing dependencies" when it finished loading before
  tmpfs was counted, or before tmpfs's retry pass. No fixture reproduces the
  race; 400 Hydrogen boots with both klibs passed before the repair.
- `1b378efd`, `aab4bd76`: fsync committed dirty page-cache nodes under the
  global lock, and called `fs_write` under the node lock. `tfs_write` can
  suspend on the filesystem mutex while holding either spinlock. The first
  commit alone made a copied profile fail sooner, after 275 cycles against
  600 clean at `66c581ca`. The second restored 577 cycles with one unrelated
  failure.
- `d771804c`: the kernel symbol table had no lock and allocated from the
  unlocked general heap. Kernel, klib and exec loads update it from different
  CPUs while klib dependency resolution reads it. The early-boot corruption
  (stale virtio-scsi completions, objcache free-list asserts) stopped in the
  next 1,078 cycles. Causation is inferred from that change in rate.
- `47bc0255`: `futex_requeue` moved its waiters' references after releasing
  the futex locks. A requeued waiter woken in that window freed the
  destination futex. `futex_requeue_race` crashed or hung 8 of 10 runs at
  four CPUs before the repair and passes 10 of 10 after it.

bctl `6f27d89` pins `47bc0255`. It also restarts the daemon when a dead
daemon's socket resets a connection; two interop runs had failed that way.
The next three interop runs pass 113 of 113.

The loops now fail about once per 500 cycles on the copied profile and once
per 300 on fresh profiles. Two faults remain open:

- A null task in a scheduler run queue, reached from `sched_enqueue`. The
  priority queue code checks out, so a write after free into the queue's
  vector is suspected.
- An idle hang. The guest prints `bctl-guest: ready`, both CPUs sleep in
  `kernel_sleep`, and the host's check times out.

## Remaining faults at `47bc0255`

A longer run, about 2,100 cycles alternating copied and fresh profiles,
corrects two conclusions above:

- The early-boot stale virtio-scsi closure fault still occurs. `d771804c`
  made it less frequent but did not remove it; its commit message overstates
  the evidence.
- The idle hang is not a lost guest interrupt. At four stalls, the guest's
  vsock virtqueues show the device consumed nothing: `used.idx` equals
  `last_used_idx`, rx has 64 buffers available, and tx has pending
  descriptors. Both vhost-device-vsock queue threads sleep in
  `futex_do_wait`. In copied-profile stalls rx `used.idx` is 0, so the
  backend never processed a queue after boot.
  Setting `DRIVER_OK` before the first rx kick, which the virtio spec
  requires, did not change the rate. It was not committed.

The loops fail about once per 420 cycles in that run. Next steps are the
vhost-user start sequence between QEMU and vhost-device-vsock for the idle
hang, and the write-after-free search for the boot fault.

## Repairs after `47bc0255`

- vhost-device-vsock ended its vring worker on any handler error, and a
  host connection that arrived after QEMU's `SET_MEM_TABLE` but before the
  rings were ready failed with `IterateQueue`. The device then consumed
  nothing: the idle hang. bctl `2792fcc` patches the backend to ignore host
  events until both rings are ready. Upstream rust-vmm has the same bug.
- The guest wrote its stop reply and powered off before the vsock backend
  consumed it, so the host read an empty reply. bctl `a098bbb` holds the
  connection until the host closes it.
- `5ece982a`: `sg_fault_in` only read-touched read destinations, so a
  copy-on-write buffer faulted during the copy under the page-cache node and
  state locks. That deadlocked against the vmap lock or a TLB rendezvous.
  `file_cow_read` hung 4 of 4 runs before the repair and passes 5 of 5
  after.
- `e4aad9cc`: `heaplock_management` allocated its closures from the
  unlocked general heap at stage-3 startup while other CPUs allocated from
  it through the locked heap. Both captured stale virtio-scsi completions
  called memory holding `heaplock_set` or `heaplock_iterate` closures.
  Causation rests on that evidence and on the rate: no heap or SCSI
  fault in the next 3,200 cycles.

With those repairs the loops ran about 2,300 cycles with one failure, a
null scheduler task. Interop at `e4aad9cc` passes 113 of 113 in 2 of 2
runs.

- `0523dbbc`: CPU 0's run queue was a priority queue whose vector grew
  from the unlocked general heap, while the other CPUs allocated from the
  same heap through its lock. A debug detector that reports any unlocked
  general-heap allocation after the secondary CPUs start found that queue
  and five more sites: kernel contexts, storage attach, the PCI driver
  vector, the x86 interrupt heap, the ACPI powerdown closures and vmxnet3.
  All six now allocate from the locked heap.

At `0523dbbc` with the detector armed, the loops ran 2,400 cycles with no
failure and no unlocked allocation reported. At the earlier rate that run
passes about one time in three without a repair, so the null task stays
open until 5,000 more cycles pass. bctl `cbf338b` pins `44f4b61f`, and
interop passes 113 of 113 in 2 of 2 runs.
