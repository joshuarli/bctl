# macOS ARM qualification

Functional verification on 2026-11-12 uses macOS 27.0.1, Apple M5 Max,
Hypervisor.framework, and bctl's rebuilt libkrun `9e6cdcab` bundle.
The browser is the pinned arm64 Helium 0.20.3.1-r6 archive.

## Storage mount deadlock

A two-vCPU virtio-fs boot could stop before the first fixture record.
The host processed FUSE `INIT`, but no subsequent request arrived.
Register captures place CPU 0 in `kernel_sleep` and CPU 1 in
`storage_set_procfs_mount`, waiting for `storage.lock` with interrupts disabled.
The mount initializer had suspended for the FUSE reply while holding that lock.

Storage now records the pending mount under its spinlock and queues its
initializer. Filesystem lookup, initialization, and mounting run outside the
lock. The completion publishes the result under the lock and checks readiness
after releasing it. A detach keeps the pending volume alive until initialization
finishes. Detach completion then waits for any resulting filesystem unmount.

The failure reproduced with the rebuilt host runtime and a newly packed image.
Those observations ruled out the runtime update and image damage as fixes.
Full kernel tracing hid the failure in 20 boots. Host-only FUSE logging still
reproduced it after 49 successful boots. Forced HVF exits then captured the
blocked guest registers without changing the kernel.

Evidence is retained under `.work/`:

- Original failure: `runs/20261112T221956Z-virtiofs_files-smp2-30439/`.
- Host sample: `tmp/virtiofs-stalled.sample`.
- Register capture: `runs/20261112T223515Z-virtiofs-register-2-57772/qemu.log`.
- Regression: `tmp/macos-storage-repeat.log`.
- Final ARM suites: `tmp/macos-kernel-final.log`.
- Invalid mount: `runs/20261112T224849Z-mount-invalid-22427/`.

## Virtio-MMIO heap race

The rebuilt bctl appliance panicked on its sixth persistent boot. Further
loops reproduced three more failures. Faults included corrupted tuple entries,
a function pointer into a symbol allocation, and page-cache cleanup failures.
One captured caller is `fsstarted`, before mounting the additional volumes.

`vtmmio_new_dev` allocated devices and their queue-handler vectors from the
unlocked general heap. Device discovery runs after secondary CPUs start.
Later queue-handler vector growth can overlap filesystem allocation on another
CPU. The Linux allocator repair had covered PCI discovery but missed MMIO.

Virtio-MMIO devices and their vectors now use `heap_locked(kh)`. A temporary
detector checked every general-heap allocation and free after SMP startup.
Before the change, one appliance boot reported 22 calls outside the wrapper:
18 device/vector allocations and four calls from two vector resizes.
After the change, the same boot reported none. The detector was removed from
the kernel used for the persistent-boot regression.

The original profile and failing machine disks remain in bctl's `.work/tmp/`.
The reproducer clones that profile before each loop and boots a new root disk
on every iteration. Each successful iteration checks CDP, requests guest
shutdown, and waits for the VMM to exit.

Evidence in the sibling bctl checkout includes these files:

- First failure: `.work/tmp/nanos-lifecycle/machines/lifecycle-5/console.log`.
- Captured failures: `.work/tmp/nbp-72892/29/` and `.work/tmp/nbp-74564/40/`.
- Unlocked calls: `.work/tmp/nbp-79377/0/console.log`.
- Detector after the fix: `.work/tmp/nbp-83220/0/console.log`.
- Uninstrumented loops: `.work/tmp/nanos-mmio-fixed-loop-1.log` and `-2.log`.

## Verification

| Check | Result |
| --- | --- |
| `dev/test.sh virtiofs --repeat 100` on macOS | 200 fresh boots pass, 100 each on 1 and 2 vCPUs |
| `dev/test.sh harness probe helium` on macOS | Pass, including virtio-fs and all harness failure classifications |
| Process probe on macOS | 55/55 and raw 6/6, native and on 1, 2, and 4 vCPUs |
| Helium on macOS | 10/10 records, including renderer sandbox and clean exit |
| Persistent bctl appliance after the MMIO heap repair | 200 boots pass in two loops, with CDP readiness and guest shutdown checked each time |
| Invalid virtio-fs mount point on macOS | Named error, then `hello_static` passes and exits |
| Linux virtio-fs on threadripper | 20 fresh boots pass, 10 each on 1 and 2 vCPUs |
| Linux 9p on threadripper | Close, retained-descriptor, and host-mutation checks pass on 1 and 2 vCPUs |
| Linux process probe on threadripper | 55/55 and raw 6/6, native and on 1, 2, and 4 vCPUs |

The Linux checks use an isolated checkout. Its first run lacked `jaq` on
SSH's PATH. A subsequent virtio-fs launch exceeded the Unix socket path limit.
The final run uses the required PATH and the shorter `.work/wt/ms` checkout.
The earlier successful 9p checks remain valid for the unchanged kernel source.

## Verification limits

The pinned Carbon independent reviewer was unavailable. No independent review
is claimed. Pending-init detach handling was checked by source review; device
hot-unplug during initialization was not exercised.

This qualification does not close HYD-3 or HYD-4. Performance gates remain
waived by the owner. No new performance result is claimed.
