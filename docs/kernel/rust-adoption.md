# Campaign: incremental Rust adoption in Hydrogen

Status: active scope, authorized 2026-11-13; implementation not started by this change.

Hydrogen will adopt Rust incrementally, starting with the ownership-sensitive
repairs in BCTL-15 and BCTL-17. Rust is the intended implementation of those
ownership domains, not an optional translation after another round of C-only
repairs. Small C containment fixes remain allowed when needed to stop a known
failure; they do not complete the migration.

This is the active kernel companion to [the bctl campaign](../CAMPAIGN-ROADMAP.md).
It supersedes the former ban on kernel language rewrites in
[development](development.md), not the syscall contract, sandbox requirements,
existing qualification gates, or [macOS workflow objectives](../macos-workflows.md).
The root TODO retains the defect IDs and their closure state. Do not duplicate
those tickets as Rust work items or claim a defect fixed because it was ported.

## Starting evidence and scope

The planning baseline is bctl `b58345e17108817dd47970d14e6c4cba78f0534f`.
Reconcile newer fixes before implementing; this is not a request to roll back
other work.

| Consumer | Evidence | What the Rust slice must own |
| --- | --- | --- |
| BCTL-15 | `kernel/nanos/src/kernel/schedule.c`, `src/runtime/pqueue.c`, and the locked-heap follow-up recorded in the root TODO and `campaign-report.md` | Queue backing storage, membership transitions, and a valid lifetime lease for every queued task |
| BCTL-17 | `kernel/nanos/src/fs/9p.c`: `p9_dentry_delete`, `p9_fsfile_delete`, `p9_fsf_free`, `p9fsf_sync_complete`; missing review detail recorded in `campaign-report.md` | Dentry/fid/metadata ownership, cache membership, open-file references, and in-flight completion lifetimes |

Paths beginning `src/` in this table are relative to `kernel/nanos/`.
The BCTL-15 corrupting write is still unidentified. Its clean 2,400-cycle
follow-up is evidence, not a causal proof. BCTL-17's suspected free-path defect
has not been demonstrated at runtime, and its missing review must be redone.
Keep these distinctions in results rather than inheriting a success claim from
an old campaign gate.

Port coherent ownership domains, not arbitrary files. Do not begin by porting
bitmaps or ring buffers merely to raise a Rust percentage. A minimal bring-up
probe is useful; it must lead directly to the two repairs. Do not replace the
bootloader, scheduler policy, process manager, Linux ABI, networking stack, or
all of virtio in this campaign. Narrowing unrelated device support is not a
prerequisite. Existing C and assembly stay where they are not part of the
selected ownership boundary.

## HR0: mixed-language foundation

Add a small, separate workspace under `kernel/rust/`, built into the existing
kernel through a freestanding static library. This path and the interfaces
below are planned, not currently shipped. Keep the host/guest workspaces and
their `unsafe_code = "forbid"` policy unchanged. Preserve the existing GCC,
binutils, NASM, boot, image, and linker flow; introducing Rust does not require
converting the C build to LLVM or introducing another build system.

Use `#![no_std]` and the repository's already pinned Rust compiler. Do not
upgrade or replace any pin as part of bring-up. Record compiler, target and
code-generation flags, dependencies, and lockfile in the kernel build receipt
and cache inputs; make `doctor.sh` and the builders verify them. If the current
pin cannot meet a necessary ABI requirement, document the exact blocker rather
than silently changing compilers. Cargo builds must be locked and support
cached/offline rebuilds after dependency provisioning. Avoid new production
dependencies unless they remove more code than they add.

The foundation must establish all of the following before a migrated subsystem
can become the default:

- A reviewed C ABI with opaque handles and explicit retain/release or ownership
  transfer. Check layouts, alignment, integer widths and status conventions on
  both architectures. No Rust layout-dependent types cross it. A raw pointer
  or `NonNull` alone is not a lifetime proof; `Pin` alone is not ownership.
- Correct code generation for the actual kernel ABI: x86-64 red-zone and
  FP/SIMD restrictions; AArch64 FP/SIMD restrictions, reserved x18, and atomic
  helper requirements; compatible relocation/code model and linker retention.
  Start from the existing kernel flags, not a userspace target or an assumption
  that a stock bare-metal target is sufficient. Check disassembly and all
  linked Rust dependencies, not only wrapper objects.
- A kernel allocator adapter with explicit alignment, zero-size, exhaustion,
  realloc, concurrency and execution-context rules. Use the locked allocator
  where allocation is allowed after SMP startup; do not expose the unlocked
  general heap as a globally usable Rust allocator. Preallocate or use fallible
  reservation for queue transitions that cannot fail. Define an explicit
  fallback for unavoidable exhaustion; never silently drop a runnable task.
- Kernel-specific lock/IRQ guards preserving interrupt state, lock ordering,
  CPU affinity of guards, and `spin_lock_flush` behavior where required.
  Audit every `Send`/`Sync` implementation. A generic spinlock or blanket
  `unsafe impl` is not an acceptable substitute for those rules.
- No unwinding across the ABI, and a panic path that emits the existing fatal
  diagnostics and halts safely without allocating or acquiring unsafe locks.
  Allocation failure follows its declared contract rather than unexpectedly
  reaching a panic in a normal fallible syscall path. Audit unresolved symbols
  and compiler-builtins: no libc, libgcc, hosted runtime or surprise TLS runtime
  dependency may enter the kernel.

Keep unsafe operations in small, reviewed boundary modules with documented
preconditions and `unsafe_op_in_unsafe_fn` denied. Safe core modules must not
contain unsafe operations. C must not keep access that violates a live Rust
borrow; neither language may free an object behind the other's lease. Callbacks
must have explicit ownership, cancellation and completion rules. Synchronous
callback re-entry, delayed callbacks and destruction under locks require tests.
Do not introduce an async executor solely to replace Nanos closures. Remaining
C still shares kernel memory and can corrupt Rust-owned storage; safety claims
must name the reviewed boundary, not imply whole-kernel memory safety.

**HR0 acceptance:** link and boot both supported architectures; exercise a real
C-to-Rust-to-C allocation/callback path; test allocation failures and concurrency;
check symbols and generated code; prove clean and incremental/cache rebuilds
include changes to Rust source, lockfile and target flags; retain reproducible
kernel images. Run the existing applicable baseline suites and report known
failures explicitly. Keep the changes opt-in until these checks pass.

## HR1: run-queue ownership (BCTL-15)

Retain `0523dbbc`'s locked-heap containment. Instrument enqueue, dequeue,
remove, steal, wakeup and task destruction to capture the first invalid state,
with task identity, queue, CPU and allocation provenance. Preserve a minimized
reproducer when found; never discard a failure capture because a later run
passed. Baseline capture and host-model work may proceed alongside HR0.

Replace the scheduler's untyped priority-queue ownership with a typed Rust core.
The core owns its storage and membership bookkeeping. Each queued entry must
hold a real task-lifetime lease: an audited C task reference, or an opaque
identity resolved through a lifetime-owning registry. Null checks or a
`Vec<*mut sched_task>` wrapped in safe-looking methods do not satisfy this.

Specify the transition model before coding. It must cover a task moving between
running, queued, migrating, blocked and exiting states, including wakeups racing
with exit. Enqueue transfers or acquires a lease; dequeue transfers it; removal
and destruction release it exactly once. A task must not be concurrently
scheduled twice. If a generation-based registry is used, test stale identities,
slot reuse and wraparound handling. Do not require that design when a simpler
valid lease fits the existing scheduler.

Keep queue storage and ordering keys inaccessible to unsynchronized C writes.
Do not retain an exclusive Rust reference while C callbacks can alias its
object. Moving a task between CPUs must not create a lease gap or duplicate
membership. Preserve scheduler ordering, affinity and flush progress. Review
all call sites, not only `pqueue_insert`; unrelated general-purpose C pqueue
users need not be ported.

Required focused tests include empty and single-element queues, growth and
allocation failure, duplicate enqueue, removal, ordering-key changes, repeated
steal/dequeue, task-exit versus wakeup, callback re-entry and allocation churn
under multiple CPUs. Test the same safe core on the host and in the kernel;
add deterministic scheduling/fault hooks rather than relying only on random
soaks. Model interleavings where practical. A test that intentionally calls
an unsafe ABI with an invalid pointer is not evidence that valid callers are
safe; test the boundary's stated contract and invalid opaque IDs instead.

**HR1 acceptance:** the typed core is used by the real scheduler, all lifetime
and allocator paths are reviewed, targeted tests pass, and at least 5,000
additional recorded parallel browser up/down cycles complete on the original
Linux failure configuration at the exact candidate revision. Also run SMP
coverage on 1, 2 and 4 vCPUs on both supported architectures. Preserve counts,
seeds and failure receipts. Clean stress alone does not establish that the
historical writer was fixed. If its cause remains unproven, record the migration
and invariant-test results but leave BCTL-15 open with that explicit gap.

## HR2: 9p lifetimes (BCTL-17)

Redo the missing independent ownership review against the current tree and
commit its findings and their resolution. Distinguish confirmed defects from
suspicions already repaired by `aacbd732` or later commits. Build a regression
for every supported risk, especially releasing a regular file and looking it
up again while its parent still caches metadata. The native Linux/musl result
is the behavioral oracle; a known-bad C build is a regression control, not the
source of truth for expected correctness.

Move the dentry/fid, metadata/cache membership and open-file lifetime machinery
to Rust. Keep the existing wire transport, filesystem ABI and page-cache
mechanisms behind small C adapters. Rust must control the leases that make those
C objects valid; do not duplicate the current refcounts in an unrelated side
table while C can still free them independently.

The cache's ownership of metadata must be independent of the final open-file
handle. Every asynchronous read, write, sync or teardown operation retains its
required file, dentry, buffers and completion context through completion.
Unlink/rename detaches namespace membership without prematurely invalidating an
open handle. Final teardown must invalidate remaining cache references before
freeing metadata, and clunk/release a fid exactly once under the transport's
actual contract. Model live, detached, closing and released states explicitly;
a zero-ref object cannot be revived through an unguarded lookup.

Review page-cache callbacks, `p9_fsf_free` and `p9fsf_sync_complete` together.
Destructors must not unexpectedly block, re-enter the filesystem or take a
conflicting lock. Separate preparation under the lock from asynchronous effects
and completion as needed, without changing externally observable filesystem
semantics. Cancellation or failure cannot abandon an owned operation or allow
a late callback to access released memory. Do not add unsupported transport
reconnection or unmount behavior just to expand the state machine.

Test last-close/relookup, open-unlink-I/O-close, rename with open handles,
parent-cache eviction, fid exhaustion/reuse, overlapping handles, delayed and
reordered completions, allocation/transport errors, partial failure cleanup,
and exactly-once callback/release behavior. Track live-object counts so replacing
a use-after-free with a leak cannot pass. Native semantic tests and host state
machine tests complement real guest 9p tests; neither replaces them.

**HR2 acceptance:** every supported review finding has a recorded disposition,
the ownership slice and adapters have independent review, and host tests plus
`sh kernel/dev/test.sh 9p` pass on Linux x86_64 with SMP coverage. Requalify
macOS ARM storage/virtio-fs when shared adapters change, but do not report that
as macOS 9p qualification. Close BCTL-17 only after the missing review, supported
repairs and their regressions are complete.

## HR3: integration and promotion

The integrator owns the shared ABI, allocator/lock adapters, build glue and root
TODO edits. After HR0's interface is stable, HR1 and HR2 can proceed in separate
lanes; review/test development need not wait for full bring-up. Land one
qualified subsystem at a time. Production must have one authoritative owner
per object, never two active C/Rust implementations with independent lifetimes.

A temporary build-time C control is allowed for differential tests. Select and
record the implementation explicitly; never fall back silently on a failing
Rust path. Remove superseded production C ownership logic after qualification;
retain comparison revisions and receipts rather than maintaining two permanent
subsystems. Preserve Nanos attribution and Apache-2.0 licensing for derived
kernel code, including the new kernel-local Rust code.

Keep these gates in addition to the focused tests:

- Existing `g0`, native-versus-guest process probes, repeated/stress probes,
  Helium, Linux 9p/vsock and bctl interop, on their supported platforms. Pin and
  explain pre-existing failures; do not turn them into new passes or broaden
  an old waiver. Commit kernel changes before the release builders consume them.
- Host safety tests for the actual Rust core, Miri for supported pure-core
  cases with its toolchain recorded separately, and targeted C/FFI checks where
  available. State what those tools cannot execute; neither proves DMA, IRQ,
  guest page tables or arbitrary remaining C code safe. Independent review of
  unsafe boundaries, locking, callback lifetimes and teardown is mandatory.
- Compare the same workload, browser, VMM, toolchains and host against the
  pinned C baseline: kernel image size, allocations/footprint, queue operations,
  boot/shutdown, 9p latency/throughput and existing browser workflows. Report
  median/tail behavior and sample counts. Fix material regressions before
  promotion or obtain an explicit scoped waiver. Do not require solving the
  separate native-performance objective before this safety work can land.

Keep BCTL-16's shutdown investigation, BCTL-18's Linux qualification,
BCTL-20's review/benchmark work and BCTL-21's performance work active. Reuse
receipts where the exact revision and platform match; these lanes must not
create duplicate campaigns or imply those defects are closed. A missing KVM
or Apple Silicon host blocks the affected qualification, not independent
implementation or host tests. Report the block and do not promote an untested
architecture by assumption.

## Further adoption

After these slices are qualified, prefer Rust for new ownership-sensitive
kernel logic and for bounded repairs where moving an ownership domain removes
the risk. Select the next slice by a concrete consumer and tests: seccomp
validation/interpreter state, descriptor/IPC ownership, or virtio request
lifetimes are candidates, not pre-authorized wholesale rewrites. C/assembly
remain legitimate low-level boundaries. Do not use percent-Rust, zero C, or
performance improvement from language choice as acceptance criteria.

## References and reporting

The existing implementation, [kernel rules](development.md), root TODO and
[Linux report](campaign-report.md) supply the project-specific evidence. Rust's
[undefined-behavior rules](https://doc.rust-lang.org/reference/behavior-considered-undefined.html)
apply across C/Rust boundaries. Target starting points are the
[x86-64 bare-metal target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html)
and [AArch64 bare-metal targets](https://doc.rust-lang.org/rustc/platform-support/aarch64-unknown-none.html);
check the pinned compiler's behavior against Hydrogen's ABI before adopting
one. These references are not a claim that a stock target has been qualified.

For each slice, report its commit, replaced C ownership paths, remaining unsafe
assumptions, tests and commands, source/toolchain/image digests, stress counts,
performance deltas, and unresolved findings. Keep milestone implementation and
defect closure separate. This scope change implements no Rust kernel code and
closes no ticket.
