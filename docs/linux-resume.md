# Linux handoff, 2026-11-12

Work stopped at the owner's request. Linux qualification is incomplete.
The latest kernel revision is `0bb4a9d974723b00a8dd2b7fdc91395c765d1f10`.
The [verification report](campaign-report.md) records accepted changes and checks.
Previous bctl context: [Linux results](../../bctl/docs/handoffs/2026-11-11-linux-x86-64-results.md).

## Remaining work

### Capture the intermittent bctl shutdown failure

The refreshed kernel passes bctl interop once, but repeated shutdown still fails.
No shutdown fix has been established. Preserve these bctl directories:

| Evidence under `../bctl/.work/tmp/` | Result |
| --- | --- |
| `linux-shutdown-5994` | 48 successful cycles, then shutdown request or reply timed out |
| `shutdown-original-profile` | Copy of the failing profile: 12 successes, then timeout |
| `shutdown-original-debug2` | Copy of the failing profile: 7 successes, then empty reply and kernel fault |
| `shutdown-breakpoint` | First shutdown timed out with a debugger attached; no useful stack captured |

The original persistent profile is
`linux-shutdown-5994/profiles/shutdown-proof/nanos-profile.img`.
Copy it into a new test home before each reproduction. Keep the original unchanged.
The fault console is
`shutdown-original-debug2/machines/bctl-shutdown-proof-8/console.log`:

```text
bctl-guest: ready
cannot get from unknown value (e 0x0000000000000000, a 0xffffbc8000007aa0)
```

These scratch helpers are in `../bctl/.work/tmp/`:

- `linux-shutdown-cycles.sh`: repeats up/down 100 times. `BCTL_HOME` selects
  an existing test home. `BCTL_DEBUG_WATCH=1` attaches a debugger before shutdown.
- `attach-shutdown-debug.py`: starts QEMU's GDB server through QMP and sets
  a hardware breakpoint on `halt_with_code`. It relocates symbols using IDT
  vector zero and the pinned ELF's `interrupt_vectors` address, `0x201263`.
  It currently misses silent deadlocks because it never interrupts `continue`.
- `watch-shutdown.py`: older delayed capture helper. Early attempts used the
  wrong GDB server mode or attached after QEMU exited. Those attempts prove nothing.
- `fast-shutdown.c` and `fast-shutdown.py`: minimal accept/reply/exit guest,
  100/100 passes. Immediate VM exit alone did not reproduce the browser failure.
- `vsock-cow.c` and `vsock-cow/`: compiled but **not run**. The proposed test
  receives into a forked COW buffer while another thread polls the same socket.

The next debugger attempt needs a bounded interrupt before the host's five-second
connect timeout or eighteen-second reply timeout. Record every CPU stack.
Use a process identity check when interrupting GDB. The current helper leaves
GDB running until the VM exits.

One untested hypothesis is a TLB rendezvous deadlock in `vsock.c`:
`vsock_lock` uses `spin_lock`, and the read path copies into user pages under
that lock. Another CPU polling the socket could wait with interrupts disabled.
The page-cache repair fixed a proven version of this pattern elsewhere.
Do not treat that analogy as the shutdown diagnosis. Run the focused COW fixture
and capture a stack before changing the lock.

Temporary guest shutdown logging produced 100 successful cycles, but changed
its timing. The instrumentation was removed and the image rebuilt. The failure
then returned with the copied profile. The current guest source is unchanged.

### Finish 9p ownership

`aacbd732` fixes the demonstrated close/stat/reopen stale-metadata failure.
An independent read-only review found two remaining source-level risks.
Neither risk has a runtime reproduction yet:

1. `p9_fsf_free` retains its dentry during asynchronous page-cache sync.
   A host delete followed by directory refresh can make `p9_dir_cleanup`
   delete that dentry before `p9fsf_sync_complete` dereferences it.
2. `p9_fsfile_new` sets `dentry->pinned` permanently. The retained closed
   dentry keeps its metadata and server fid. Distinct closed files can
   accumulate when their parent directory is not refreshed.

The existing regression repeatedly uses one file. It does not cover either case.
Add host-mutation coverage with an open or syncing file, plus a distinct-file
close workload that checks server-fid reclamation.

A possible repair separates fsfile references from temporary operation pins.
Directory refresh would detach referenced dentries and defer destruction.
The final fsfile release would clunk an unused fid while retaining metadata
still owned by a parent tuple. This is a design candidate, not implemented code.
Check create-error cleanup, unlink, rename replacement, fid eviction, and the
embedded root dentry before adopting it.

### Complete qualification and measurements

- Repeat bctl interop after a demonstrated shutdown repair. One 113/113 run
  does not close the intermittent shutdown or earlier browser-use navigation issue.
- Review and commit the three pending bctl changes listed below.
- Keep the 14 declared upstream baseline failures explicit. The regression run
  did not add failures, but does not make upstream fully green.
- Collect Linux external-client measurements on a quiet host.
  **Perf gate deferred: host busy.** No performance improvement is claimed.
- Record G4.2's required wall time and per-boot median. The functional checks
  pass, but their timing report is unfinished.
- Refresh final gate acceptance after the ownership and shutdown work.
  macOS and AArch64 were not tested in this Linux session.

## Repository state

Hydrogen's kernel and fixture changes are committed and pushed on `main`.
This handoff replaces the earlier, stale Linux resume report.

bctl remains at `5ac7b9e` with exactly three uncommitted implementation files:

- `runtime/build-nanos.sh`: Linux pin advanced from `6dc7d43e` to `0bb4a9d9`.
  The AArch64 pin is unchanged. Treat the Linux pin as a qualification candidate.
- `tests/interop/checks.py`: copies machine console and supervisor logs before down.
- `tests/interop/run.sh`: preserves failed homes and logs under `.work/interop/failures/`.

The interop changes pass the normal suite and an intentional failure-retention
check. Their diagnostic filesystem operations can still mask a failure if a copy
or directory creation fails. Review that cleanup behavior before committing.
An exact backup is at `../bctl/.work/tmp/linux-wrapup-wip.patch`.
No bctl implementation change was committed or pushed during wrap-up.

The bctl runtime source copy is pinned independently under
`.work/runtime/nanos/hydrogen`. Rebuilding Hydrogen's root checkout does not
update that copy. The accepted interop image was later overwritten during
diagnosis. Its recorded hash identifies the tested image, not today's artifact.

No session-owned test VM, shutdown watcher, or GDB process was running at wrap-up.
The read-only `linux-review` worktree was retired after its report was preserved.
Other worktrees and all failing profiles remain in place.

## Build constraints

Use `PATH="$HOME/.cargo/bin:$HOME/usr/bin:$PATH"` for bctl builds.
Set `GIT_EXEC_PATH="$HOME/.local/share/git/current"` for submodule operations.
Keep `TMPDIR` under the repository's `.work/tmp`.

Hydrogen's pinned compiler is
`.work/toolchain/clang+llvm-23.1.5-x86_64-linux-musl/bin/clang`,
with sysroot `.work/sysroot`.
Set `VIRTIOFSD=/home/josh/d/bctl/.work/runtime/nanos/bin/virtiofsd` for filesystem tests.
The Helium archive directory is
`/home/josh/d/chromium-build/.work/artifacts/helium-0.20.3.1-r5`.

Run source edits, builds, and VM suites sequentially when they share artifacts.
Do not edit a shell script while it is running. Do not rebuild a kernel or image
while a VM runner uses it. Both mistakes caused invalid evidence during diagnosis.
