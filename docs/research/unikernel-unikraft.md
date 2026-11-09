# Unikraft as a host for bctl's browser appliance

**Verdict up front: not viable.** Two independent hard blockers, each proven
by a live probe or by direct evidence:

1. **The only demonstrated Chromium-on-Unikraft stack is not self-hostable.**
   Every public Chromium example runs on `runtime: base-compat:latest` for
   `kraftcloud/x86_64`. That kernel has no public source, no public config and
   no anonymous registry access (HTTP 401), and the open `lib-ukp-bin` guest
   additions it appears to depend on are a proprietary, license-restricted
   binary blob. Under the owner's rule (public source, rebuildable, runnable
   locally) that alone ends the evaluation.
2. **Stock open-source Unikraft cannot satisfy the sandbox rule.** I built
   upstream `staging` plus `app-elfloader` with multiprocess enabled and ran a
   probe as a real Linux ELF. `vfork`+`execve`+`wait` work. Everything the
   Chromium sandbox needs does not: no `fork` (no second address space), no
   user/pid/net namespaces, no seccomp, `PR_SET_NO_NEW_PRIVS` silently
   "succeeds" without effect, no `/proc/self/status`, `setuid` fails, no
   `memfd_create`, and `SCM_RIGHTS` is silently dropped. There is also no
   virtio-gpu, DRM/KMS, virtio-input or evdev anywhere in the tree.

Good news recorded for completeness: an arm64 Unikraft kernel boots under
**QEMU with HVF** on Apple Silicon (GICv3), unlike Nanos. It does not
currently run a Linux app on arm64 from upstream (see 1).

| # | Question | Verdict | Decisive evidence |
|---|---|---|---|
| 0 | Self-hostable (owner rule) | **No** | `base-compat` unpublished; registry 401; `lib-ukp-bin` proprietary |
| 1 | arm64 on Apple Silicon, HVF; libkrun | **HVF yes (kernel boots); libkrun unverified** | HVF run reaches the loader; needs 2 upstream arm64 patches and still crashes loading an app |
| 2 | fork/clone/execve/wait | **vfork+execve only; no fork** | probe: `fork` ENOTSUP, `vfork`+`execve`+`wait` OK, `clone(CLONE_VM)` OK |
| 3 | Sandbox prerequisites | **No** | probe: namespaces ENOTSUP, `seccomp` ENOSYS, NNP stubbed, no `/proc/self/status` |
| 4 | virtio-gpu/DRM, input/evdev, SCM_RIGHTS, memfd | **No** | no drivers in tree; probe: memfd ENOSYS, `SCM_RIGHTS` dropped |
| 5 | Chromium (sandbox on / diagnostic) | **Not run** | stopped at hard blockers; the one public example is `--no-sandbox` |
| 6 | Boot time and memory vs SmolVM | **Not comparable** | kernel boot is milliseconds; no browser to measure |
| 7 | Overall | **Not viable** | See sections 2 and 8 |

Scratch and sources lived under the worktree's git-ignored `.work/`.

---

## 1. Self-hostability (the owner's central question)

**Verdict: no. `base-compat` cannot be rebuilt from public source.**

- All public Chromium examples use it. `unikraft-cloud/examples`
  (`476b59c`) `chromium-cdp/Kraftfile` and `novnc-browser/Kraftfile`:
  `runtime: base-compat:latest`, `targets: [kraftcloud/x86_64]`, erofs rootfs
  from a Debian Bookworm glibc image. `chromium-cdp/proxy.js` drives it
  through Playwright's `chromium.launch`, and Playwright adds `--no-sandbox`
  unless `chromiumSandbox: true` is passed
  (`packages/playwright-core/src/server/chromium/chromium.ts:386-387`, main).
  The example does not pass it. The demonstrated configuration is sandbox off.
- I searched `unikraft/catalog` (`476b59c`), `unikraft/kraftkit`,
  `unikraft/unikraft`, `unikraft/app-elfloader` and the whole
  `unikraft-cloud` and `unikraft` GitHub orgs (`gh repo list`) for the source
  or config of `base-compat`. The only hits are Kraftfiles and READMEs that
  consume it, and a unit-test string in KraftKit
  (`internal/cli/kraft/pkg/packager_kraftfile_runtime_test.go:28`). The docs
  describe it only as "the generic compatibility runtime"
  (https://unikraft.com/docs/platform/images).
- The registry is closed: `curl https://index.unikraft.io/v2/` returns 401, and
  `kraft pkg pull index.unikraft.io/official/base-compat:latest` reports
  `could not find`. Not even the binary is obtainable anonymously.
- Closest public analogue: `unikraft/catalog/library/base/Kraftfile` builds
  `unikraft.org/base` from open staging plus app-elfloader. It sets
  `CONFIG_LIBPOSIX_PROCESS_MULTITHREADING=y` and **no**
  `LIBPOSIX_PROCESS_MULTIPROCESS`, and targets `fc/x86_64` and `qemu/x86_64`
  only (no arm64). It is not a multi-process kernel.
- Cloud-only parts: `unikraft-cloud/lib-ukp-bin` ("Guest additions for
  Unikraft Cloud") ships prebuilt `libukp-{x86_64,arm64}-*.o` objects under a
  license that forbids reproduction, modification and use without written
  permission (`LICENSE.md`). `Config.uk` makes it
  `depends on KVM_VMM_FIRECRACKER`. Whether `base-compat` links it is not
  stated anywhere public; that it is undisclosed is itself the finding.
- `onkernel/kernel-images` (`6d4d1a6`, Apache-2.0) is a usable reference for
  the userland (Chromium flags, CDP proxy, live view) but relies on
  Cloud-only pieces: the Unikraft deploy scripts use `--plat kraftcloud
  --arch x86_64 --push` to a Unikraft Cloud index; the wrapper writes
  `/uk/libukp/scale_to_zero_disable`
  (`server/cmd/wrapper/system.go:8`); the Kraftfiles use `base-compat` and
  `cloud.unikraft.v1.instances/scale_to_zero.*` labels. Its wrapper hard-codes
  `--no-sandbox` and `--ozone-platform=headless`
  (`server/cmd/wrapper/chromium.go:57,67`). The headful variant uses Xvfb and
  WebRTC, not a scanout device.

KraftKit (BSD-3) and the Unikraft core, `lib/posix-process` and
`app-elfloader` are public and I built them. That is what section 2 tests.

## 2. Processes: vfork and execve, nothing more

**Verdict: no `fork`; single address space by design.**

Source, `lib/posix-process/README.md` (staging `a4c494e`): "Being a
single-address-space operating system ... Unikraft implements multiprocess
with `vfork()` only", and `execve()` requires PIE. `clone()` enforces it:
`if (!(flags & CLONE_VM)) ... "Multiple address spaces are not supported"
return -ENOTSUP` (`lib/posix-process/clone.c`, near line 216).

Live probe (x86_64 build, see 8 for why x86): a glibc static-PIE C program
doing `fork`, `vfork`+`execv("/probe")`+`waitpid`, `clone`.

```
fork pid=-1 errno=95
CHILD_RAN pid=3 argv1=child
vfork pid=3 errno=0
vfork_wait ret=3 errno=0
vfork_exit=3
clone_vm_sigchld ret=4 errno=0
```

So Chromium's zygote (which uses `fork`/`clone` without `CLONE_VM`) cannot
start unmodified, and "processes" share one address space, so a compromised
renderer is not MMU-isolated from the browser process. That undermines the
sandbox's premise regardless of whether flags exist.

## 3. Sandbox prerequisites: absent

**Verdict: no.** Same probe:

```
clone_NEWUSER_NEWPID_NEWNET ret=-1 errno=95   (Unsupported clone flags 0x70000100)
unshare_NEWUSER ret=-1 errno=38
unshare_NEWNET ret=-1 errno=38
prctl_SET_NO_NEW_PRIVS ret=0
prctl_GET_NO_NEW_PRIVS ret=0
prctl_GET_SECCOMP ret=0
seccomp_SET_MODE_FILTER_probe ret=-1 errno=38
setuid_1000 ret=-1 errno=22
open_proc_self_status fd=-1 errno=2
```

- `NNP` reads back 0 after being set to 1: `prctl` is a stub that returns 0
  (`lib/posix-process/deprecated.c`, `UK_WARN_STUBBED(); return 0;`). Worse
  than an error, as with Nanos: Chromium would believe it had applied it.
- The `CLONE_NEW*` flags are only printed in debug output
  (`lib/posix-process/clone.c:178-191`); seccomp has no handler.
- There is no `/proc/self/status`, so Chromium's `Seccomp` and `NoNewPrivs`
  checks have nothing to read. `/proc/self/maps` and `/proc/cpuinfo` are also
  absent.

## 4. Devices and fd passing

**Verdict: no.**

- Drivers present: `drivers/virtio/{9p,blk,bus,console,fs,mmio,net,pci,ring,vsock}`
  and `drivers/input/ukps2` (PS/2 only). There is no virtio-gpu and no
  virtio-input. A tree search for `virtio-gpu`, `drm`, `evdev` finds only the
  Xen `displif.h` header and a virtio device-id table entry.
- Probe: `/dev/dri/card0`, `/dev/fb0`, `/dev/input/event0`, `/sys/class/drm`,
  `/dev/shm`, `/dev/tty` all `ENOENT`; `/dev/null` and `/dev/urandom` exist.
- `memfd_create` returns `ENOSYS` (wl_shm needs it).
- `SCM_RIGHTS` is silently lost: `sendmsg` returns 1, `recvmsg` returns 1,
  and the received control message carries no SCM_RIGHTS fd
  (`recv_cmsg_scm_rights=NO fd=-1`). `lib/posix-unixsocket` has no control
  message handling; `SO_PASSCRED` returns `ENOPROTOOPT` (92). Unix
  `socketpair` for stream and seqpacket works. So seatd and Wayland fd
  passing cannot work even with a process model.
- Display path: the roadmap needs virtio-gpu scanout to RFB plus
  virtio-input. Neither driver exists; this is the same gap as Nanos.

## 5. Chromium

**Not run, by decision.** Both blockers above were proven first, and the brief
said to stop at a proven hard blocker. What exists:

- The sandboxed multi-process Chromium requires `fork`/namespaces/seccomp
  (sections 2 and 3). It cannot start with the sandbox on.
- Unikraft's own Chromium example is sandbox off. Running it needs
  `base-compat`, which I cannot obtain (section 1). A `--no-sandbox` run on
  stock Unikraft would be diagnostic only and could never be the product; I
  did not do it because the result cannot change the verdict.
- Headed Wayland is not conceivable: no DRM/KMS, no evdev, no `memfd`, no
  `SCM_RIGHTS`.

## 6. Host: arm64, HVF, libkrun

**Verdict: HVF works for the kernel; arm64 app execution is broken upstream;
libkrun unverified.**

- With `CONFIG_LIBUKINTCTLR_GICV3=y`, `qemu-system-aarch64 -machine
  virt,gic-version=3 -accel hvf -cpu host` boots the kernel (banner,
  `Ijiraq 0.21.0~a4c494e-custom`). Without GICv3, HVF fails at boot with
  `Could not initialize the IRQ controller: -1` (setup.c:102), because HVF has
  no GICv2. Unikraft ships a GICv3 driver (`drivers/ukintctlr/gic/gic-v3.c`),
  which is the opposite of Nanos' GICv2-only port.
- TCG with `-cpu max` fails: `4KiB granule not supported`
  (`plat/native/arch/arm64/paging.c:68`): the check requires TGRAN4 == 0 and
  newer QEMU `max` reports 1 (inference). `-cpu cortex-a72` gets past it.
- Upstream arm64 does not build or run an ELF as-is at `staging` `a4c494e`.
  The x86_64 build of the same config is clean. For arm64 I had to apply two
  local fixes, neither upstreamed:
  1. `lib/posix-process/Makefile.uk`: drop the `signalfd-3` provided syscall
     (arm64 has no legacy `signalfd`; error `Failed to map system call
     'signalfd'`). Same failure at tag `RELEASE-0.21.0`.
  2. `lib/syscall_shim/arch/arm64/syscall_handler.c`: add
     `#include <uk/arch/ctx.h>` (`invalid use of undefined type 'struct
     ukarch_execenv'`).
  The shipped `app-elfloader/defconfigs/qemu-aarch64-9pfs` also references
  rootfs options (`LIBVFSCORE_ROOTFS_*`) that no longer exist; I replaced them
  with `LIBVFSCORE_AUTOMOUNT_CI_INITRD`.
- After that the HVF guest loads the program and **kernel-crashes** in
  `ukarch_auxsp_get_cb` (`include/uk/arch/ctx.h:434`) with `Cannot deliver
  SIGSEGV for pf at 0x0`; the first fault is hidden by the signal path. I did
  not root-cause it. It may be my config, but x86_64 with equivalent options
  works, so arm64 is clearly less exercised upstream. The catalog `base`
  kernel does not target arm64 at all, and Unikraft keeps a separate
  `unikraft/unikraft-arm-bincompat` fork "for ARM64 binary compatibility
  work" (last push 2026-01-08), which supports that reading.
- libkrun/SmolVM VMM: **not tested** (`krunkit`, `krun`, `smolvm` are not
  installed). The built arm64 image is a "Linux kernel ARM64 boot executable
  Image" and Unikraft has virtio-mmio plus FDT boot, so a libkrun boot may
  be possible; nothing here proves it. Unikraft's supported VMMs are QEMU,
  Firecracker (x86), Xen.

## 7. Boot time and memory

Not comparable, so no SmolVM claim. Measured for completeness:

- arm64 HVF: the kernel reached the loader in about 2 ms of guest time
  (`[0.002036]` at the crash), then crashed.
- x86_64 under TCG (`qemu-system-x86_64 -machine pc -cpu max`), kernel plus
  the full probe, to the last line: **0.34 s** wall. This is a tiny static
  binary, not a browser. Kernel image: 792 KB (arm64).

The unikernel boot-time advantage is real, but there is no booting browser to
compare against SmolVM's ~4 s `m1`.

## 8. Verdict, work required, and what would change it

**Not viable.** There is no bounded-work path, because the blockers are
architectural and one of them is not ours to fix.

| Requirement | Stock open Unikraft | Work if fixed in-house |
|---|---|---|
| Rebuildable Chromium-capable kernel | `base-compat` closed | Re-derive it from `unikraft/base` + multiprocess; unknown how far it differs |
| `fork` with separate address spaces | absent; vfork/CLONE_VM only | A new MMU-isolated process model; open-ended kernel work |
| Namespaces, seccomp-bpf, real NNP, `/proc/self/status` | absent | Open-ended; seccomp-bpf alone is a BPF interpreter plus syscall hooks |
| virtio-gpu scanout and DRM/KMS | absent | New driver plus a DRM/KMS personality, or a custom tinyland backend |
| virtio-input/evdev | absent | New driver plus evdev semantics |
| memfd, `SCM_RIGHTS`, `SO_PASSCRED` | absent/stubbed | Moderate each, but only useful with a real process model |
| arm64 Linux ELF execution | broken at staging | Two small patches found, plus an unexplained auxsp crash; small to medium |

A sandbox-on Chromium plus tinyland plus seatd stack is not reachable on
Unikraft without changing the kernel's process and isolation model. That is
the same conclusion as Nanos (`docs/research/unikernel-nanos.md`) but for a
different reason: Unikraft's process model exists and is deliberately
single-address-space.

What would need to change upstream for this to be reopened: a published
multiprocess Linux-compat kernel with arm64 support; per-process address
spaces; seccomp and namespaces; virtio-gpu/DRM and virtio-input/evdev. None
is on a visible roadmap in `staging`.

Recommendation: close X2 for unikernels (Nanos and Unikraft). The remaining
X2 candidates are Linux-kernel VMMs (Firecracker, Cloud Hypervisor on Linux
hosts).

## Appendix A. Environment

| Item | Value |
|---|---|
| Host | macOS 27, arm64 (`sysctl kern.hv_support` = 1) |
| QEMU | Homebrew `qemu-system-aarch64` and `qemu-system-x86_64` 11.1.2 |
| KraftKit | `kraft` 0.12.16 (Homebrew), used for catalog listing and pull attempts only |
| Unikraft | `staging` `a4c494e` (reports 0.21.0), plus two arm64 local patches (section 6) |
| app-elfloader | `staging`; lib-libelf `staging` |
| Build | Docker `debian:trixie-slim` arm64, gcc 14.2; x86_64 via `x86_64-linux-gnu-` cross gcc; `WITH_LWIP=n` |
| Probe | `probe.c`: glibc static-PIE, built in the same container; delivered as a cpio initrd |
| Reference repos | `unikraft-cloud/examples` `476b59c`, `unikraft/catalog` `476b59c`, `onkernel/kernel-images` `6d4d1a6` |

Deviation to disclose: KraftKit ignores `HOME` and creates
`~/.local/share/kraftkit` and `~/.config/kraftkit`, even with `--config-dir`,
`--manifests-dir` and `--sources-dir` pointing under `.work/`. These are small
cache and runtime directories.

## Appendix B. Commands and decisive output

```
$ curl -s -o /dev/null -w '%{http_code}\n' https://index.unikraft.io/v2/        # 401
$ kraft pkg pull index.unikraft.io/official/base-compat:latest --arch x86_64 --plat qemu
level=error msg="could not find index.unikraft.io/official/base-compat:latest"
$ kraft pkg pull unikraft.org/base:latest --arch arm64 --plat qemu
level=error msg="could not find unikraft.org/base:latest"

# arm64, HVF, GICv3, cortex-a72 not needed (host cpu)
$ qemu-system-aarch64 -machine virt,gic-version=3 -accel hvf -cpu host -m 1G \
    -nographic -kernel elfloader_qemu-arm64 -initrd initrd.cpio -append "-- /probe"
... CRIT: [libposix_process] <deliver.c @ 428> Cannot deliver SIGSEGV for pf at 0x0
... ELR_EL1: 0x000000004013003c   -> ukarch_auxsp_get_cb (include/uk/arch/ctx.h:434)

# HVF without GICv3 in the kernel config
CRIT: [libkvmplat] <setup.c @ 102> Could not initialize the IRQ controller: -1

# x86_64, TCG: full probe (this is the decisive evidence for sections 2-4)
$ qemu-system-x86_64 -machine pc -cpu max -m 1G -nographic -accel tcg \
    -kernel elfloader_qemu-x86_64 -initrd initrd86.cpio -append "/probe"
PROBE_START pid=2
ERR: [libposix_process] <clone.c @ 216> CLONE_VM not set: Multiple address spaces are not supported
fork pid=-1 errno=95
CHILD_RAN pid=3 argv1=child
vfork pid=3 errno=0
vfork_wait ret=3 errno=0
vfork_exit=3
clone_vm_sigchld ret=4 errno=0
clone_NEWUSER_NEWPID_NEWNET ret=-1 errno=95
unshare_NEWUSER ret=-1 errno=38
unshare_NEWNET ret=-1 errno=38
prctl_SET_NO_NEW_PRIVS ret=0
prctl_GET_NO_NEW_PRIVS ret=0
seccomp_SET_MODE_FILTER_probe ret=-1 errno=38
setuid_1000 ret=-1 errno=22
open_proc_self_status fd=-1 errno=2
open /dev/dri/card0 fd=-1 errno=2   (also /dev/fb0, /dev/input/event0, /sys/class/drm, /dev/shm)
memfd_create ret=-1 errno=38
socketpair_unix ret=0
sendmsg_SCM_RIGHTS ret=1
recvmsg ret=1
recv_cmsg_scm_rights=NO fd=-1 fd_valid=0
setsockopt_SO_PASSCRED ret=-1 errno=92
PROBE_DONE
```

## Appendix C. Not verified

- Chromium was not run on Unikraft in any mode (headless, headed, or
  `--no-sandbox` diagnostic).
- The probe's process results are from **x86_64 under TCG**, because the arm64
  guest crashed before running it. The upstream sources are architecture
  neutral for `clone.c` and `deprecated.c`, but the arm64 probe is
  unobserved. Namespaces, seccomp and device gaps are source-confirmed on all
  architectures.
- The arm64 crash root cause. It may stem from my configuration.
- libkrun/SmolVM as a Unikraft VMM; Firecracker; Xen.
- Whether `base-compat` links `lib-ukp-bin`, and whether it carries
  compatibility work absent from staging. Both are undisclosed rather than
  disproved; the verdict depends only on the fact that it cannot be rebuilt.
- Boot time and memory against SmolVM; no browser ran.
- The `-cpu max` TGRAN4 explanation is inferred from the check, not probed.
