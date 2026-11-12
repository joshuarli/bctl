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
after releasing it. A detach during initialization retains the volume until
the initializer finishes and any resulting filesystem is unmounted.

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

## Verification

| Check | Result |
| --- | --- |
| `dev/test.sh virtiofs --repeat 100` on macOS | 200 fresh boots pass, 100 each on 1 and 2 vCPUs |
| `dev/test.sh harness probe helium` on macOS | Pass, including virtio-fs and all harness failure classifications |
| Process probe on macOS | 55/55 and raw 6/6, native and on 1, 2, and 4 vCPUs |
| Helium on macOS | 10/10 records, including renderer sandbox and clean exit |
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
