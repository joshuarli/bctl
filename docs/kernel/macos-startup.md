# macOS startup instrumentation

`HYDROGEN_BOOT_PROFILE` enables serial timestamps in kernel initialization.
The build flag is `KERNEL_TEST_CFLAGS=-DHYDROGEN_BOOT_PROFILE` on the Nanos
`make kernel` command. A normal build compiles out the calls.
The flag applies to kernel objects, including `init.c` and `stage3.c`.

Each record has the form
`hydrogen-startup STAGE monotonic_us=VALUE`.
`VALUE` comes from `CLOCK_ID_MONOTONIC_RAW`, in microseconds.
The first record follows platform clock registration.
It excludes firmware and kernel work before that registration.

| Stage | Boundary |
| --- | --- |
| `clock_ready` | Platform clock registration completes |
| `secondary_cores_ready` | Secondary core startup returns |
| `devices_discovered` | Platform and PCI device discovery return |
| `root_filesystem` | Root filesystem initialization invokes its completion |
| `unix_init_begin`, `unix_init_ready` | Unix initialization begins and returns |
| `init_exec_begin` | Initial executable loading begins |
| `init_loaded` | Initial executable loading invokes its completion |

Filesystem completion and device discovery can overlap across CPUs.
Records describe observed boundaries, rather than a serial list of phases.

The appliance owns guest-supervisor and Chromium launch timestamps.
The host runtime owns disk preparation, libkrun configuration, and readiness
probe timestamps. Their instrumentation and receipts live in
[bctl's macOS workflow reference](https://github.com/joshuarli/bctl/blob/main/docs/macos-workflows.md).
Host wall timestamps and guest monotonic timestamps have different origins.
They cannot be subtracted from each other.

Five isolated diagnostic boots on the M5 Max place `clock_ready` to guest
supervisor entry at a median 10.965 ms, range 10.301–13.091 ms.
Chromium execution to the successful guest CDP probe takes a median
548.681 ms, range 490.935–695.132 ms.
That interval includes readiness polling and the successful HTTP request.
It does not isolate Chromium CPU work from kernel services or scheduling.
These instrumented boots do not establish a release performance improvement.

Both kernel configurations build on Linux ARM64 in the macOS Docker builder.
The instrumented image completes five boots with browser and sandbox readiness.
