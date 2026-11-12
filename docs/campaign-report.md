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
