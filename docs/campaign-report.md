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
in [bctl performance](../../bctl/docs/performance.md#linux-x86_64-2026-11-12).
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
