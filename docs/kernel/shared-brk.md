# Shared heap break qualification

The partial BCTL-20 review found that separate processes using `CLONE_VM`
could calculate break changes from the same old value and publish a break
inconsistent with the mapped heap. `ea66aca4` moves the complete transition
under the shared address space's vmap lock, after the calling process lock.
The syscall wrapper delegates to that single owner.

The regression at `kernel/nanos/test/runtime/shared_brk.c` coordinates two
separate processes sharing an address space. After each pair of competing
requests it checks the reported break against the mapped range. Native
Linux/musl passes 2,000 rounds; the preceding Nanos kernel fails at the first
round with a page still mapped above the break.

An [independent review](shared-brk-review.md) of the first repair found that
generic vmap removal does not unmap resident heap pages: heap vmaps have no
mmap type. `f97b29a4` restores explicit physical-page unmapping while holding
the shared lock. The expanded fixture writes a heap page, shrinks it, requires
an access fault in a child, and requires zero-filled regrowth before the race
rounds. That check fails the intermediate candidate at every tested CPU count.

## Results

Run `sh kernel/dev/test.sh shared-brk resource-limits`. On macOS, set
`HYDROGEN_KRUN` to an existing libkrun bundle. The native macOS-host oracle
runs Linux/musl in the ARM builder; it does not compare Darwin syscalls.

| Host | Native Linux/musl | Nanos 1/2/4 vCPUs |
| --- | --- | --- |
| macOS ARM64, HVF/libkrun | shared break and 7 resource checks pass | all pass |
| Linux x86_64, KVM | shared break and 7 resource checks pass | all pass |

[Raw receipts and serial output](verification/2026-11-14-shared-brk/)
record revision `fc033050`, toolchain, fixture, image and kernel digests.
The Linux checkout was clean. The ARM receipts mark the checkout dirty because
qualification documentation was being edited; `git diff HEAD -- kernel`
was empty. Both contain the committed `f97b29a4` implementation.

## Limits

The race fixture coordinates requests without forcing an internal instruction
interleaving. The resident-page regression directly proves stale mappings are
removed and regrowth is zero-filled; it does not measure physical-page leaks.
Allocation failure, mixed heap/mmap replacement, simultaneous per-process
limit changes and every possible TLB-flush interleaving remain unqualified.
This repairs the demonstrated shared-break defect, but does not close
BCTL-20's whole-fork review or benchmark requirements.
