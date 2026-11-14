# Resource limit qualification

BCTL-19 is repaired by `883ff3f6` and `40e1b546`.
`prlimit64` reads and updates the namespace-visible target under its process lock.
Target access checks credentials or the target namespace's `CAP_SYS_RESOURCE`.
Raising a hard limit separately requires initial-user-namespace `CAP_SYS_RESOURCE`.

Stack limits retain both their soft and hard values across fork.
Unmodeled limits return `EINVAL` instead of accepting an ineffective limit.
`RLIMIT_CORE` remains readable as zero because core dumps are unsupported.
Zero `RLIMIT_NOFILE` forbids new descriptors, and `F_DUPFD` validates its minimum.

`RLIMIT_DATA` counts private writable non-stack mappings, including heap growth.
`mmap`, `mprotect`, and `mremap` check projected usage under the shared vmap lock.
Replacing a mapping credits its existing usage. Unmapping or removing write access releases capacity.
Like Linux, a zero soft data limit uses the hard limit for mapping expansion.
The existing byte-level `brk` check still enforces the soft limit.

## Regression results

Run the focused suite with `sh kernel/dev/test.sh resource-limits`.
On macOS, set `HYDROGEN_KRUN` to the existing libkrun bundle.
The fixture source is `kernel/nanos/test/runtime/resource_limits.c`.
Both build systems compile that same source.

All seven checks pass natively and on Nanos at 1, 2 and 4 vCPUs:

| Platform | Native oracle | Nanos | Candidate |
| --- | --- | --- | --- |
| Linux x86_64, Threadripper, KVM | 7/7 | 7/7 on each CPU count | `40e1b546`, clean tree |
| macOS ARM64, HVF/libkrun | 7/7 in native ARM Linux builder | 7/7 on each CPU count | `40e1b546`, clean tree |

[Raw receipts and serial records](verification/2026-11-14-resource-limits/)
contain kernel, fixture and image digests, compiler versions, and reproduction commands.
The native oracle on macOS is Linux/musl inside the ARM builder, not Darwin syscalls.

The checks cover foreign-pid updates without changing the caller, exited targets,
mapping exhaustion, shared/read-only exemptions, capacity reclamation, fixed replacement,
stack hard limits, invalid input, unmodeled limits, namespace hard ceilings, and zero descriptors.
At `a21c8882`, the first three checks fail on Nanos and pass on Linux.
At `883ff3f6`, the added namespace and zero-descriptor checks fail on Nanos and pass on Linux.

## Remaining limits

The [independent review](fork-review.md) identified a separate shared-`brk` race.
It is reproduced and repaired by `ea66aca4` and `f97b29a4`; see
[shared break qualification](shared-brk.md).
These results do not close BCTL-20's broader review or benchmark gates.
