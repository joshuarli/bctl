# Hydrogen fork review

Reviewed at `883ff3f64830ddc44152242f48d64b2c61c18390`. The intent is to assess the resource-limit repair and identify supported risks in the fork delta. These findings are source analysis, not reproduced guest failures. BCTL-20 remains open, including its separate benchmark gate.

## Findings

### P1: A user namespace can raise inherited hard limits

`kernel/nanos/src/unix/syscall.c:1951` computes privilege in the target's user namespace and uses that same result to authorize raising a hard limit at `syscall.c:1992-1994`. `kernel/nanos/src/unix/namespace.c:430-443` grants that capability in the caller's own namespace. `syscall.c:1376-1399` gives the caller all capabilities in a newly unshared user namespace. Consequently a process can lower its hard limit, enter a new user namespace, and raise its own hard limit again without initial-namespace `CAP_SYS_RESOURCE`. This defeats a hard resource ceiling inherited from a supervisor. Linux distinguishes permission to access another process from permission to raise a hard limit: its [resource-limit implementation](https://github.com/torvalds/linux/blob/master/kernel/sys.c) checks the latter against the initial user namespace.

Regression: have an unprivileged process lower both `RLIMIT_NOFILE` limits, unshare a user namespace, and attempt to raise the hard limit. Compare with native Linux, expecting `EPERM`; also exercise a target in a descendant namespace and a caller with initial-namespace privilege. Keep target-access authorization and hard-limit authorization separate.

### P2: A zero descriptor limit permits descriptor allocation

`kernel/nanos/src/unix/syscall.c:1973-1975,2000-2002` accepts and stores a zero `RLIMIT_NOFILE` soft limit. Both allocation paths treat zero as disabling the check: `kernel/nanos/src/unix/unix.c:38-50` and `unix.c:63-70`. A process with a zero soft limit can therefore open or duplicate descriptors, including after a supervisor applies the limit with `prlimit64`. The limit field defaults to 65536 at `unix.c:822`, so zero is a valid configured value rather than an uninitialized sentinel. The allocator behavior predates this commit, but the commit's target-limit enforcement depends on it.

Regression: close a descriptor, set the soft limit to zero while retaining a nonzero hard limit, and check that `open` and `fcntl(F_DUPFD)` fail with `EMFILE` on Linux and Nanos. Repeat after a parent sets the child's limit via `prlimit64`.

### P2: Shared address spaces do not serialize break updates

`kernel/nanos/src/unix/unix.c:1328-1330` lets distinct processes share a `process_vm` through `CLONE_VM`, but each has its own process lock. `kernel/nanos/src/unix/syscall.c:2082-2109` holds only that process lock while reading and updating the shared `vm->brk`. `adjust_process_heap` acquires the shared vmap lock only for its own range change (`kernel/nanos/src/unix/mmap.c:962-975`), after `brk` has calculated its old and new ends. Two processes can therefore calculate from the same old break, serialize conflicting adjustments, and publish different break values in an order unrelated to the vmap changes. A shrink can leave `vm->brk` pointing into a removed range, or a stale growth can fail despite a valid current break. This matters to the new heap accounting because its checked delta is derived from that stale break.

Regression: create two processes with `CLONE_VM` and without `CLONE_THREAD`, synchronize concurrent `brk` growth and shrink around the old-break read, and verify the final break and mapped heap agree. Include a concurrent limit update and a deterministic interleaving hook. Serialize the complete break transition with the shared address space, while respecting lock order and TLB-flush progress.

## Resolved findings

`40e1b546` repairs the hard-limit namespace check and zero descriptor limit.
The hard-limit check now requires initial-user-namespace `CAP_SYS_RESOURCE`.
The descriptor allocators enforce zero and `F_DUPFD` validates its minimum.
Both regressions pass natively and fail on the preceding Nanos kernel.
After the repair, all seven resource-limit checks pass on both architectures
at 1, 2 and 4 vCPUs. Receipts are linked from [resource limits](resource-limits.md).
The shared-break finding remains open and has not been reproduced.

## Coverage and limits

The resource-limit review inspected `process_rlimit`, `prlimit64`, their user-copy and credential paths, namespace capability checks, limit initialization and fork inheritance, descriptor allocation, `brk`, and the mapping checks for `mmap`, `mprotect`, and `mremap`. The shared vmap lock serializes mapping accounting, but the process lock used for limit writes is distinct from that lock and from the descriptor-table lock. Concurrent limit updates, fork snapshots, and their precise observable interleavings need dedicated tests. The zero soft `RLIMIT_DATA` fallback is explicitly exercised by the existing test and was not classified as a defect here. The fixed `mremap` replacement calculation was inspected, but mixed mappings and failure paths remain untested.

`kernel/nanos/test/runtime/resource_limits.c` covers five cases: target limits, data mappings, stack and errors, mapping capacity, and rejection of unmodeled limits. The supplied qualification reports those five passing on Linux and macOS native and Nanos at 1, 2, and 4 vCPUs. This review did not rerun them. They do not exercise the namespace, zero-descriptor, or shared-break cases above.

The upstream-root-to-subtree comparison is `git diff e4b9c9e65cd51ef29e5021835419a1d9a7733f20 HEAD:kernel/nanos`: 148 files, 14,907 insertions and 1,835 deletions. Beyond resource limits, this review inspected the storage mount and detach changes in `kernel/nanos/src/kernel/storage.c:56-79,106-210,438-464` and the locked MMIO allocation in `kernel/nanos/src/virtio/virtio_mmio.c:29-48`, against `docs/kernel/macos-qualification.md`. No additional finding is established for those repairs. Pending-init detach, asynchronous completion ordering, allocation failure, and MMIO discovery concurrency remain unverified; the qualification itself says hot-unplug during initialization was not exercised.

The existing [9p ownership review](9p-ownership-review.md) records separate open findings and was read, not independently re-proved here. Most of the fork delta, including seccomp, namespaces outside the capability path, process lifecycle, exec, futexes, sockets, page cache, virtio-fs, architecture code, and their tests, was not reviewed in depth. No whole-fork correctness or security verdict follows from this partial review. No runtime tests or benchmarks were run for this record.
