# Shared break review

Reviewed `ea66aca4ddbcfa3a52e4b76e7cc3783c039d9202` against its parent. The intent is to serialize the entire break transition across separate processes sharing an address space.

## Finding

### P1: Shrinking a populated heap leaves its pages mapped

`process_set_brk` creates heap vmaps with `VMAP_FLAG_HEAP | VMAP_FLAG_READABLE | VMAP_FLAG_WRITABLE` (`kernel/nanos/src/unix/mmap.c:973`). None of those flags sets `VMAP_MMAP_TYPE_MASK` (`kernel/nanos/src/unix/unix_internal.h:10-27`). On shrink, the new path calls `process_remove_range_locked(..., true)` (`mmap.c:980-981`), which removes the vmap and calls `vmap_unmap_page_range` for the intersection (`mmap.c:1370-1412,1439-1452`). That function switches only on the mmap type and has no case for zero (`mmap.c:1418-1436`), so it neither clears heap PTEs nor frees their physical pages. The parent explicitly called `address_space_unmap_and_free_phys` after removing the range. Faulting in and writing a heap page, shrinking past it, and accessing it again therefore leaves the old mapping accessible above the reported break; regrowth can expose the old contents, and removed pages can leak even at address-space teardown because their vmap was removed. Heap demand allocation installs a PTE at `mmap.c:237-280`, while teardown only walks remaining vmaps (`mmap.c:2231-2253`).

The new regression does not expose this: it does not touch the heap pages, and checks only `mincore` return status (`kernel/nanos/test/runtime/shared_brk.c:73-94`). Nanos `mincore` returns `ENOMEM` for a vmap gap before inspecting PTEs (`mmap.c:1170-1182`), so it can report the range absent while a stale PTE remains. A regression should populate a full heap page, shrink below it, verify access faults in a child, and verify regrowth yields zero-filled memory; include repeated shrink and teardown for physical-page reclamation. This finding is established by source analysis, not a guest reproduction in this review.

## Evidence and limits

Parent follow-up: the populated-heap regression reproduced the finding on
the reviewed candidate at 1, 2 and 4 vCPUs. `f97b29a4` restores explicit
physical-page unmapping while retaining the shared vmap lock. The expanded
fixture verifies a fault after shrink and zero-filled regrowth, then runs
the competing-break test. See [qualification](shared-brk.md).

- The syscall is the sole caller of `process_set_brk` (`kernel/nanos/src/unix/syscall.c:2082-2085`). It takes the process lock and then the shared VM vmap lock around the read, range change, publication, and returned value (`mmap.c:962-988`). Separate `CLONE_VM` processes reserve the same VM (`kernel/nanos/src/unix/unix.c:1327-1333`); fork copies the break and vmaps under that lock (`mmap.c:2160-2171`).
- The VM lock masks interrupts and services pending flushes while spinning (`kernel/nanos/src/unix/unix_internal.h:1029-1032`, `kernel/nanos/src/kernel/kernel.h:193-209`). Process teardown runs after its last thread, clears the VM under the process lock and releases its reference (`unix.c:1498-1512`); shared VM references and final mapping teardown are at `unix_internal.h:731-738` and `mmap.c:1897-1900,2231-2256`. The newly introduced heap teardown defect above prevents a general flush or reclamation pass verdict; other lock-order and flush interleavings were not exhaustively proven.
- Null, below-base, and rounded-overflow requests return the current break; failed growth checks and allocation do not publish the request (`mmap.c:966-985`). Growth preserves the previous process-local `RLIMIT_DATA` and aggregate data checks (`mmap.c:941-959,971-979`); the process lock also guards limit writes (`syscall.c:1946-1950,1969-1971,2001-2006`). Same-page updates bypass growth checks as before. Concurrent limit changes across distinct shared-VM processes, allocation failure, overflow, mapping replacement, and mixed mapping shrink paths were not exercised here.
- The supplied context reports native Linux success over 2,000 coordinated rounds and focused shared-brk and resource-limits passes on native and Nanos at 1, 2, and 4 vCPUs on Linux x86_64 and macOS ARM64. Those results were not independently rerun or validated from receipts here. The shared-brk test coordinates competing requests but does not force a particular internal interleaving (`shared_brk.c:21-42,73-94`); resource-limit tests cover data mappings rather than break failure paths (`kernel/nanos/test/runtime/resource_limits.c:53-90,116-143`). No runtime tests were run for this review.
