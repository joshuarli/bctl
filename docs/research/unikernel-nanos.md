# Nanos as a host for bctl's browser appliance

**Verdict up front: not possible today, and not possible with bounded work.**
Nanos 0.1.56 is a genuine single-process kernel. It has no
`fork`/`clone`-with-new-address-space, no `execve` syscall, no seccomp, no
namespaces, no DRM/KMS, no evdev, and no virtio-gpu or virtio-input driver.
Chromium does start executing on it — a real Alpine `chromium`
(142.0.7444.59, aarch64, musl) reaches `chrome_main.cc` and then dies on a
SIGTRAP inside early crashpad initialisation, with the sandbox on and off
alike. The current bctl appliance (tinyland + seatd + sandboxed multi-process
Chromium) has no path to running there without rewriting the kernel.

Measured on the way: an aarch64 Nanos guest boots and runs a static binary in
**0.23 s wall clock** on this Apple Silicon host, entirely under QEMU TCG. The
boot-time argument for unikernels is real; everything else is missing.

| # | Question | Verdict | Decisive evidence |
|---|---|---|---|
| 1 | Host support on Apple Silicon macOS | **Yes, but TCG only** | aarch64 guest boots (`RAW_HELLO_FROM_NANOS`); HVF refuses Nanos' GICv2 |
| 2 | fork/clone/execve/multiple address spaces | **No** | `src/unix/thread.c:57-60`; live probe returns `errno=38` |
| 3 | Sandbox prerequisites (userns, seccomp, setuid, NNP, /proc) | **No** | live probe: `unshare`/`seccomp` `errno=38`, `/proc/self/status` `ENOENT`, NNP is a silent no-op |
| 4 | virtio-gpu / DRM / KMS / virtio-input / evdev | **No** | no driver sources; probe with devices attached still `ENOENT` |
| 5 | tinyland + seatd fit | **No** | needs `/dev/dri/card0`, `/dev/input/event*`, seatd over SCM_RIGHTS; none exist |
| 6 | Chromium headless fallback | **No** | Chrome starts, then SIGTRAP; identical sandbox on/off |
| 7 | Alternatives | Unikraft only, and its published Chromium demo runs `--no-sandbox` | `kernel-images/server/cmd/wrapper/chromium.go:57` |
| 8 | Overall | **No** | See §8 |

---

## 1. Host support: yes, but without hardware acceleration

**Verdict: yes for running an arm64 Nanos guest; no for HVF acceleration.**

ops publishes no GitHub release assets (`api.github.com/repos/nanovms/ops/releases`
returns `"assets": []` for every release on the first page of results,
0.1.33–0.1.47), but the install script's binary URL serves a standalone
darwin/arm64 Mach-O. I downloaded it directly, without running the installer
and without touching the real `$HOME`:

- `https://storage.googleapis.com/cli/darwin/aarch64/ops` → 226 MB,
  `Mach-O 64-bit executable arm64`, `ops version` prints `Ops version: 0.1.47`.
- `OPS_HOME=<worktree>/.work/home` redirects ops' state: `GetOpsHome()` honours
  `OPS_HOME` and uses `$OPS_HOME/.ops` (`lepton/const.go:56-70` at tag 0.1.47).
  All images and kernels landed under `.work/home/.ops`.
- One deviation to disclose: `DownloadReleaseImages` hardcodes
  `path.Join("/tmp", …)` for the transient kernel tarball
  (`lepton/image.go:634`), so the kernel download briefly hit the real `/tmp`
  despite `TMPDIR` being redirected. It is deleted by a deferred `os.Remove`.

Guest arch selection is confusing and, on macOS, half-broken:

- `ops update` on darwin/arm64 downloads the **x86-64** kernel
  (`home/.ops/0.1.56/kernel.img: ELF 64-bit LSB executable, x86-64`).
- `ops update --arm` downloads the **aarch64** kernel
  (`home/.ops/0.1.56-arm/kernel.img: ELF 64-bit LSB executable, ARM aarch64`),
  matching `nanos-release-linux-0.1.56-virt.tar.gz`.
- `ops run … --arch arm64` boots the arm64 kernel but **silently disables
  acceleration**: `addAccel()` returns early whenever `AltGOARCH != ""`
  (`qemu/qemu_unix.go:444-446`), so QEMU runs TCG with `-cpu max`.
- `ops run` **without** `--arch` tries HVF and fails: ops hardcodes
  `-machine gic-version=2` for arm (`qemu/qemu_unix.go:529`), and QEMU replies
  `qemu-system-aarch64: HVF does not support GICv2 emulation`. Nanos' aarch64
  port is GICv2-only: its interrupt driver is GICD-register based with no GICv3
  redistributor or `GICR_*` support, and `platform/virt/Makefile:318` pins
  `-machine virt -machine gic-version=2`. I tried `-accel hvf` with
  `gic-version=3` directly; the guest produced no console output within 120 s
  and was killed. (Inference from the QEMU error plus the missing GICv3 code:
  the two are incompatible, not merely misconfigured.)

So today's Apple Silicon path is **aarch64 Nanos under TCG**. That is fast for
a tiny binary (0.23 s cold boot) but is pure emulation for a browser.

## 2. Process model: no processes at all

**Verdict: no.**

The documentation is unambiguous: Nanos is "a single process system with no
support for running multiple programs nor does it have the concept of users"
(`README.md:12`, tag 0.1.56).

The implementation matches. `clone_internal` accepts only thread creation:

```c
if (!(flags & CLONE_THREAD)) {
     msg_warn("clone: attempted to create new process");
     return -ENOSYS;
}
```
— `src/unix/thread.c:57-60`, tag 0.1.56.

`clone(2)` is registered (`src/unix/thread.c:148-149`), so the syscall is
reachable and returns failure rather than being absent. `execve` is never
registered at all: of 184 `register_syscall` entries across `src/unix` and
`src/aarch64` at 0.1.56, none is `execve`, `execveat`, `fork`, `vfork`,
`wait4` (beyond `syscall_ignore`) or `clone3`-as-process. The dispatcher
pre-sets `-ENOSYS` and leaves it there for unregistered calls
(`src/unix/syscall.c:2526`, `:2544`). `sysinfo` reports `procs = 1`
(`src/unix/syscall.c:2320`).

Live confirmation from a freestanding aarch64 probe run as the Nanos program
(`.work/hello/probe.c`, boot log in Appendix B):

```
clone_SIGCHLD_newproc ret=-38 errno=38
clone_NEWUSER_NEWPID_NEWNET ret=-38 errno=38
```

What fails, precisely, for Chromium: the browser process would call
`clone(SIGCHLD)` to create the zygote and get `-ENOSYS`; `execve` of the
zygote/renderer/GPU/utility binaries does not exist as a syscall; and there is
only one `process` structure, so there is nowhere to put a second address
space. tinyland and seatd could not be separate processes either.

Nanos does support **threads** (`CLONE_THREAD` works), which is why Chromium's
crashpad thread appears as tid 2 in the crash log. That does not help.

## 3. Sandbox prerequisites: none are present

**Verdict: no.**

Probe output on Nanos 0.1.56, aarch64:

```
unshare_NEWUSER ret=-38 errno=38
seccomp_SET_MODE_FILTER ret=-38 errno=38
prctl_SET_NO_NEW_PRIVS ret=0
getuid ret=0
setuid_1000 ret=0
getuid_after_setuid ret=0
open_proc_self_status ret=-2 errno=2
open_proc_self_maps ret=3
```

- **User/PID/net namespaces**: `clone` with `CLONE_NEWUSER|CLONE_NEWPID|CLONE_NEWNET`
  and `unshare(CLONE_NEWUSER)` both return `ENOSYS` (38). The `CLONE_NEW*`
  constants exist only as `#define`s in `src/unix/system_structs.h:641-663`;
  nothing consumes them.
- **seccomp-bpf**: `SYS_seccomp` exists as a number
  (`src/aarch64/unix_syscalls.h:264`) but has no handler, so it hits the
  `-ENOSYS` default at `src/unix/syscall.c:2526`. Chromium's renderer cannot
  install its BPF filter.
- **setuid / credentials**: `getuid`, `setuid` and `setgid` are all registered
  as `syscall_ignore` (`src/unix/syscall.c:2445`, `:2448-2449`), a stub that
  returns 0 unconditionally (`src/unix/syscall.c:2143-2146`). The probe shows
  the consequence: `setuid(1000)` "succeeds" and `getuid()` still returns 0.
  There are no users to drop to.
- **NoNewPrivs**: `prctl` handles only `PR_SET_NAME`/`PR_GET_NAME` and returns
  0 for everything else (`src/unix/syscall.c:2291-2305`). `PR_SET_NO_NEW_PRIVS`
  therefore reports success without setting any kernel state. That is worse
  than an error: Chromium would believe the flag was applied.
- **`/proc`**: the special-file table exposes `/proc/meminfo`, `/proc/cpuinfo`,
  `/proc/mounts`, `/proc/self/maps` and `/sys/devices/system/cpu/online`
  (`src/unix/special.c:245-256`). `/proc/self/status` — where Chromium reads
  `NoNewPrivs` and `Seccomp` — returns `ENOENT`.
- **`setsockopt`/SCM**: Chromium's crashpad logs
  `setsockopt: Not supported (95)` (`ENOTSUP`) on its credential socket, the
  first error before the SIGTRAP in every run (inference: that socket is
  crashpad's peer-credential channel).

**Inference:** even a hypothetical port that added processes would still need
seccomp-bpf, user namespaces, a working NNP, and `/proc/self/status` before the
sandbox contract in bctl's rules could be satisfied.

## 4. Graphics and input: nothing to draw to, nothing to read from

**Verdict: no.**

`src/virtio/` at tag 0.1.56 contains: scsi, 9p, balloon, mmio, net, pci, rng,
socket, storage, virtqueue. There is **no** virtio-gpu and **no** virtio-input
driver. A repository-wide search for `virtio-gpu`, `/dev/dri`, `drm`, `evdev`,
`framebuffer` or `fbdev` finds only two hits, both QEMU trace-flag comments in
platform Makefiles.

The only display code is `src/drivers/vga.c`, which is legacy x86 VGA text mode
(I/O ports, `0xb8000`) and is not built for `virt`. ops invokes QEMU with
`-display none` and no GPU device (`qemu/qemu_unix.go:209` and the arm branch
at `:526-529`).

Empirical confirmation: I booted the probe with
`-device virtio-gpu-pci -device virtio-keyboard-pci -device virtio-tablet-pci`
attached. The devices are present on the PCI bus, but the guest has no drivers
for them and still reports:

```
open_dev_dri_card0 ret=-2 errno=2
open_dev_fb0 ret=-2 errno=2
open_dev_input_event0 ret=-2 errno=2
open_sys_class_drm ret=-2 errno=2
```

`ioctl` has no DRM dispatch path: unknown requests return `-ENOSYS`
(`src/unix/syscall.c:2121-2122`).

## 5. tinyland + seatd fit: no, at three independent layers

**Verdict: no.**

What the compositor stack needs, from this repo:

- tinyland opens `/dev/dri/card0`, sets `UniversalPlanes` and `Atomic`
  client capabilities, enumerates connectors/CRTCs/planes, creates dumb buffers,
  adds framebuffers, and does an atomic modeset or a legacy `set_crtc`
  (`guest/tinyland/src/output.rs`, `DrmOutput::open`/`atomic_modeset`), then
  page-flips and waits for the flip event.
- tinyland reads `evdev` devices: `/dev/input/event*`, `supported_keys`, `grab`,
  `fetch_events` (`guest/tinyland/src/input.rs`), and compiles an XKB keymap via
  kbvm.
- tinyland requires libseat with `LIBSEAT_BACKEND=seatd` and refuses any other
  backend (`guest/tinyland/src/seat.rs`, `Session::open`). libseat talks to
  seatd over a Unix socket and receives device fds by SCM_RIGHTS.
- seatd itself is a separate daemon process.

On Nanos: (a) there is no `/dev/dri/card0` and no DRM ioctls; (b) there is no
`/dev/input/event*` and no evdev; (c) seatd cannot be a separate process, and
Nanos' Unix-socket `sendmsg` ignores `msg_control` entirely
(`src/unix/socket.c:927-938`, tag 0.1.56), so SCM_RIGHTS fd passing does not
exist even if a daemon could run.

What would have to change on Nanos, in order:

1. A virtio-gpu driver with a DRM/KMS personality: minor device, GEM/dumb
   buffers, connector/CRTC/plane objects, atomic properties, page-flip events.
2. A virtio-input driver with evdev semantics (`/dev/input/eventN`, EVIOC*
   ioctls, key/abs event streams).
3. A process model so seatd can exist, plus SCM_RIGHTS, or a Nanos-specific
   direct-open backend that bypasses libseat entirely.
4. An output backend in tinyland that talks to whatever (1) exposes.

That is a kernel port plus a small rewrite of the compositor's platform layer.
And it still leaves §2 and §3 unsolved, so the browser would not run afterwards.
The honest description is "write a different kernel", not "port the appliance".

## 6. Chromium fallbacks: it starts, then dies; the sandbox never gets a chance

**Verdict: no — not headless, not `--ozone-platform=headless`, not even with
`--no-sandbox`.**

Binary used: Alpine 3.22 `chromium-142.0.7444.59-r0` (aarch64, musl,
`/usr/lib/chromium/chromium`), extracted with its 183-package dependency set
into an Alpine minirootfs, and packaged by ops with
`-e LD_LIBRARY_PATH=/lib:/usr/lib:/usr/lib/chromium`. Image size 329 MB.

Results (full commands in Appendix B):

1. `--headless=new` (no `--user-data-dir`):
   `ERROR:chrome/app/chrome_main.cc:207] Failed to create a unique user data
   directory for headless.` — so the ELF loaded, the dynamic linker ran, and
   `chrome_main` executed.
2. `--headless=new --no-sandbox --user-data-dir=/`:
   ```
   [2:2:1109/003833.540407:ERROR:third_party/crashpad/crashpad/util/linux/socket.cc:45] setsockopt: Not supported (95)
   *** signal 5 received by tid 2, errno 0, code 1
        esr: 00000000f2000000 brk
   ...
   [0.558679] storage_shutdown error: no core generated: limit 0
   ```
   Nanos dumps the thread context and the guest shuts down. Signal 5 with
   `brk` is an executed breakpoint — i.e. a libc/Chromium trap, not a kernel
   signal (inference from the ESR value).
3. Sandbox **on** (no `--no-sandbox`): identical failure at the same point
   (`setsockopt` then SIGTRAP). The sandbox is never initialised, so
   "sandbox on/off" makes no difference here.
4. `--headless=new --ozone-platform=headless --disable-breakpad
   --disable-crash-reporter`: identical.

**Interpretation (inference):** the fatal path is early crashpad/credential
socket setup, which needs peer-credential passing Nanos does not implement.
Even if that were bypassed, the next requirement is `clone(SIGCHLD)` for the
zygote, which returns `ENOSYS` (§2). There is no configuration of Chromium
that avoids needing a second process for the renderer; `--single-process` is
non-compliant and I did not test it, because the crash occurs before any
process creation decision is reached.

The glibc `chrome-headless-shell` 157.0.8103.38 (`linux-arm64`) was downloaded
but never booted: ops on macOS cannot package it, failing with
`failed building manifest: file does not exist`, because its darwin library
resolver searches only `/lib64`, `/lib/x86_64-linux-gnu`, `/usr/lib`,
`/usr/lib64`, `/usr/lib/x86_64-linux-gnu` (`lepton/ldd_darwin.go:131`) and
cannot find `/lib/ld-linux-aarch64.so.1` on a macOS host. Packaging it would
require a full glibc arm64 target root.

## 7. Alternatives worth naming

**Verdict: one alternative exists (Unikraft), and its published headless
Chromium wrapper defaults to `--no-sandbox`; none is a ready substitute.**

**Unikraft.** The only unikernel I found with a published, production
multi-process Chromium. Its `lib/posix-process` documents three feature levels
and a `CONFIG_LIBPOSIX_PROCESS_MULTIPROCESS` tier in which `clone()`, `vfork()`,
`execve()` and `wait4()` all work
(https://github.com/unikraft/unikraft/blob/staging/lib/posix-process/README.md).
Two caveats matter for bctl. First, Unikraft runs everything in a **single
address space** — even its `execve` documentation requires PIE binaries because
of this — so "processes" are not MMU-isolated the way Chromium's renderer
sandbox assumes. Second, seccomp-bpf is not part of that story; I found no
evidence it is implemented. Kernel (kernel.sh) is the existence proof: their
engineering post says Chromium "is far from a single-process program" and that
they "put Chromium on a Unikernel" by stretching the definition, using a
Unikraft build that "lets you run multi-process general purpose workloads"
(https://www.kernel.sh/blog/scale). Their published headless wrapper, however,
defaults to `--no-sandbox`
(https://github.com/onkernel/kernel-images/blob/main/server/cmd/wrapper/chromium.go,
line 57) — i.e. the demonstrated configuration is exactly the one bctl forbids.
Their GPU-capable browsers later moved off Unikraft to a QEMU-based data plane
(Hypeman) because Unikraft/Firecracker had no device passthrough and no plan to
add it. Whether Unikraft itself exposes a 2D virtio-gpu/DRM path is unverified;
what is clear is that the GPU-capable direction at Kernel was away from
unikernels.

**OSv.** Single-application by design and explicitly does not support `fork`
("OSv does not support fork() (i.e., processes with separate address spaces)")
— https://mikelangelo-project.eu/technology/universal-unikernel-osv/. It cannot
host Chromium's process model. Iso-UniK is a research prototype that added
multi-process support to OSv with 66 µs forks
(https://link.springer.com/article/10.1186/s42400-020-00051-9); it is not a
deployable option.

**Others.** I found no unikernel other than Unikraft that has demonstrated
multi-process Chromium. I did not investigate MirageOS, IncludeOS or HermitOS;
none is known to me to have such a demonstration.

## 8. Verdict and the cheapest experiment that would change it

**"Vanilla Chromium + our Rust Wayland stack on Nanos with the sandbox on" is
not possible today, and is not "possible with bounded work".** The blockers are
not integration work:

| Requirement | Nanos 0.1.56 |
|---|---|
| second process / address space | absent (`thread.c:57-60`, probe `ENOSYS`) |
| `execve` | no handler |
| seccomp-bpf | no handler |
| user/pid/net namespaces | no handler |
| NoNewPrivs | silently ignored, returns 0 |
| `/proc/self/status` | absent |
| DRM/KMS + dumb buffers + atomic modeset | no driver, no ioctls |
| evdev `/dev/input/event*` | no driver |
| seatd / SCM_RIGHTS | no second process; `sendmsg` ignores control data |

A bounded port would have to add items 1-3 of §5 plus the process and sandbox
features listed in §2 and §3 before a single pixel could be scanned out by
tinyland and before Chromium's renderer could keep its sandbox. That is a
kernel project with an open-ended scope, not a backend integration.

**What Nanos does buy**, and why it was worth measuring: 0.23 s cold boot to a
running static binary, on a 13 MB image, under pure TCG. If bctl ever wants a
sub-second-start appliance, that number is the argument — but it is bought by
deleting every interface the appliance depends on.

**The single cheapest experiment that could change the verdict** is not on
Nanos. It is to run the same `probe.c` — plus the already-built Alpine
Chromium image — against Unikraft's `CONFIG_LIBPOSIX_PROCESS_MULTIPROCESS`
app-elfloader on arm64, and check three things in order: (1) `clone(SIGCHLD)`
returns a pid, (2) `seccomp(SECCOMP_SET_MODE_FILTER)` and
`prctl(PR_SET_NO_NEW_PRIVS)` are enforced rather than stubbed, (3) a
virtio-gpu/DRM path exists for a KMS compositor. If (1) and (2) both hold, the
unikernel option re-enters consideration as "possible with work" — on Unikraft,
not Nanos. If (2) fails, then no unikernel currently available can satisfy
bctl's hard sandbox rule, and the question is closed for the whole category.

What would *not* change the verdict: any amount of work on Nanos' graphics or
input drivers, because the process model and seccomp gaps are independent and
larger.

---

## Appendix A — environment

| Item | Value |
|---|---|
| Host | macOS 27.0.1 (build 26A434), arm64 (Apple Silicon) |
| QEMU | `/opt/homebrew/bin/qemu-system-aarch64`, version 11.1.2 |
| ops | 0.1.47, darwin/arm64, downloaded from `https://storage.googleapis.com/cli/darwin/aarch64/ops` (226 MB) |
| Nanos kernel | 0.1.56, aarch64 (`nanos-release-linux-0.1.56-virt.tar.gz`), 1.7 MB |
| Nanos source | tag `0.1.56` = `aad473aad2f73a9893d5c32be333f2796725ab82`, cloned to `.work/src/nanos` |
| ops source | tag `0.1.47`, cloned to `.work/src/ops` |
| Cross-compiler | `zig cc -target aarch64-linux-musl` (Homebrew zig) |
| Chromium tested | Alpine 3.22 `chromium-142.0.7444.59-r0` aarch64/musl |
| Chromium not booted | Chrome for Testing 157.0.8103.38 `chrome-headless-shell-linux-arm64` |
| Downloads | ops 226 MB + Chrome 122 MB + Alpine packages ~300 MB + indexes ~12 MB ≈ 660 MB (well under 3 GB) |
| Scratch | everything under `.work/` (git-ignored via `.git/info/exclude:14`), `OPS_HOME`/`HOME`/`TMPDIR` redirected there |

## Appendix B — commands, exit codes, decisive output

Every command below ran from `/Users/josh/d/bctl/.work/wt/unikernel`, with
`OPS_HOME=$PWD/.work/home HOME=$PWD/.work/home TMPDIR=$PWD/.work/tmp` unless
noted. `ops` is `.work/nanos/ops`.

**B1. Tool and version checks**

```
$ command -v qemu-system-aarch64        # exit 0, /opt/homebrew/bin/qemu-system-aarch64
$ qemu-system-aarch64 --version         # exit 0, "QEMU emulator version 11.1.2"
$ .work/nanos/ops version               # exit 0
Ops version: 0.1.47
Nanos version: 0.0
```

**B2. Kernel download**

```
$ .work/nanos/ops update                # exit 0
Update nanos to 0.1.56 version.
$ file .work/home/.ops/0.1.56/kernel.img
ELF 64-bit LSB executable, x86-64 ...

$ .work/nanos/ops update --arm          # exit 0
Update nanos to 0.1.56 version.
$ file .work/home/.ops/0.1.56-arm/kernel.img
ELF 64-bit LSB executable, ARM aarch64 ...
```

**B3. Boot a static aarch64 hello (success)**

```
$ zig cc -target aarch64-linux-musl -nostdlib -static -O2 .work/hello/raw.c -o .work/hello/raw-arm64   # exit 0
$ /usr/bin/time -p .work/nanos/ops run .work/hello/raw-arm64 --arch arm64 --imagename rawtest           # exit 0
running local instance
booting .../.work/home/.ops/images/rawtest ...
[0.013468] vtbln failed to attach: (status:failed to find free MSI-X slot)
[0.031678] en1: assigned 10.0.2.15
RAW_HELLO_FROM_NANOS
real 0.23
user 0.09
sys 0.05
```

(The `vtbln` warning appears on every boot and is benign.)

**B4. libc static binary hangs (unexplained, recorded)**

```
$ .work/nanos/ops run hello/hello-arm64 --arch arm64      # produced only boot/network lines
# waited 90 s, no program output, guest still running at 99.6% CPU → killed
```
A zig-built **static musl** hello-world never reached `main`. A freestanding
raw-syscall binary (B3) and a dynamically linked Alpine `busybox` (B8) both
ran. Cause not established; recorded because it shows Nanos userspace
compatibility is partial.

**B5. Syscall/device probe (the core evidence)**

Source: `.work/hello/probe.c`; built with
`zig cc -target aarch64-linux-musl -nostdlib -ffreestanding -fno-builtin -static -O2`.

```
$ .work/nanos/ops run hello/probe-arm64 --arch arm64 --imagename probe   # exit 0
PROBE_START
clone_SIGCHLD_newproc ret=-38 errno=38
clone_NEWUSER_NEWPID_NEWNET ret=-38 errno=38
unshare_NEWUSER ret=-38 errno=38
seccomp_SET_MODE_FILTER ret=-38 errno=38
prctl_SET_NO_NEW_PRIVS ret=0
getuid ret=0
setuid_1000 ret=0
getuid_after_setuid ret=0
open_proc_self_status ret=-2 errno=2
open_proc_self_maps ret=3
open_dev_dri_card0 ret=-2 errno=2
open_dev_fb0 ret=-2 errno=2
open_dev_input_event0 ret=-2 errno=2
open_sys_class_drm ret=-2 errno=2
open_dev_seatd_sock ret=-2 errno=2
PROBE_DONE
```

**B6. Acceleration attempts**

```
$ qemu-system-aarch64 -machine virt,gic-version=2 -accel hvf -cpu host \
    -kernel .work/home/.ops/0.1.56-arm/kernel.img \
    -device virtio-blk-pci,drive=hd0 -m 2G -device virtio-rng-pci \
    -drive file=.work/home/.ops/images/probe,format=raw,if=none,id=hd0 \
    -display none -serial stdio
qemu-system-aarch64: HVF does not support GICv2 emulation
# exit 1

$ qemu-system-aarch64 -machine virt,gic-version=3 -accel hvf -cpu host ...    # timeout 120 s, no output

$ .work/nanos/ops run hello/raw-arm64 --imagename noarch    # ops exit 0 (!)
qemu-system-aarch64: HVF does not support GICv2 emulation
exit status 1
```
Note: ops exits **0** even when QEMU fails; the failure is only a log line.

**B7. Graphics devices attached, still absent**

```
$ qemu-system-aarch64 -machine virt,gic-version=2 -cpu max \
    -kernel .work/home/.ops/0.1.56-arm/kernel.img \
    -device virtio-blk-pci,drive=hd0 -m 2G -device virtio-rng-pci \
    -drive file=.work/home/.ops/images/probe,format=raw,if=none,id=hd0 \
    -display none -serial stdio \
    -device virtio-gpu-pci -device virtio-keyboard-pci -device virtio-tablet-pci
# exit 0; probe output identical to B5 (all device opens ENOENT)
```

**B8. Alpine rootfs and dynamic musl busybox (success)**

```
$ curl -sSfL .../alpine-minirootfs-3.22.0-aarch64.tar.gz | tar xz -C tmp/alpine-root   # exit 0
$ (cd tmp/alpine-root && .work/nanos/ops run bin/busybox --target-root . --arch arm64 \
     --imagename alpine-busybox -e LD_LIBRARY_PATH=/lib)     # exit 0
... busybox prints its full applet list and exits
```
The `-e LD_LIBRARY_PATH=/lib` is required because ops' darwin lib resolver
never searches `/lib` (`lepton/ldd_darwin.go:131`).

**B9. Chromium on Nanos**

```
$ curl -sSfL -o tmp/chs.zip https://storage.googleapis.com/chrome-for-testing-public/157.0.8103.38/linux-arm64/chrome-headless-shell-linux-arm64.zip
# exit 0, 122 MB
$ .work/nanos/ops run tmp/chs/.../chrome-headless-shell --arch arm64 --imagename chrome-headless
Failed to build image, error is: failed building manifest: file does not exist
# exit 1
```
(glibc interpreter not resolvable from macOS — see §6.)

Alpine chromium, 183 packages extracted, image 329 MB:

```
$ (cd tmp/alpine-root && .work/nanos/ops run usr/lib/chromium/chromium --target-root . \
    --arch arm64 --imagename chromium -e LD_LIBRARY_PATH=/lib:/usr/lib:/usr/lib/chromium \
    --disable-args-copy -a --headless=new -a --no-sandbox -a --disable-gpu \
    -a --dump-dom -a about:blank)                            # exit 0
booting .../.work/home/.ops/images/chromium ...
[1109/003815.509294:ERROR:chrome/app/chrome_main.cc:207] Failed to create a unique user data directory for headless.
```

```
$ (cd tmp/alpine-root && .work/nanos/ops run usr/lib/chromium/chromium ... \
    -a --user-data-dir=/ -a --no-first-run ...)              # exit 0
[2:2:1109/003833.540407:ERROR:third_party/crashpad/crashpad/util/linux/socket.cc:45] setsockopt: Not supported (95)
*** signal 5 received by tid 2, errno 0, code 1
     esr: 00000000f2000000 brk
[0.558679] storage_shutdown error: no core generated: limit 0
```

```
# same, sandbox left ON (no --no-sandbox)
[2:2:1109/003842.531612:ERROR:.../socket.cc:45] setsockopt: Not supported (95)
*** signal 5 received by tid 2, errno 0, code 1
     esr: 00000000f2000000 brk
```

```
# same, --ozone-platform=headless --disable-breakpad --disable-crash-reporter
# identical SIGTRAP at the same point
```

Full logs retained at `.work/tmp/chrome-run2.log`, `.work/tmp/chrome-sandbox.log`,
`.work/tmp/chrome-hl.log`.

## Appendix C — things I did not verify

- **I did not try `--single-process` or `--no-zygote`.** `--no-sandbox` appears
  only in the labelled non-compliant characterization runs; the crash occurs
  before Chromium chooses its process layout, so a single-process run would
  only move the failure.
- **I did not obtain a Nanos running with HVF.** Timeout with `gic-version=3`
  produced no console output; I did not instrument further, so the exact
  failure point is unknown. The incompatibility itself is proven by QEMU's
  error with GICv2.
- **I did not run the glibc Chrome for Testing build.** Packaging it needs a
  glibc arm64 target root; I did not assemble one.
- **I did not verify Unikraft, Kernel or OSv by running anything.** §7 is from
  their published sources only, as the brief specified.
- **I did not test whether Nanos can be patched** to add processes, seccomp,
  DRM or evdev. The §8 estimate that this is a kernel project is an inference
  from the size of the missing surfaces, not a measured engineering result.
- **The 0.23 s boot time** is a single measurement with `/usr/bin/time -p` on a
  warm page cache; it is indicative, not a benchmark.

## Appendix D — files created in the worktree

All under the git-ignored `.work/` except the report itself:

```
.work/nanos/ops                     ops 0.1.47 darwin/arm64 binary
.work/home/.ops/                    OPS_HOME state: kernels 0.1.56 and 0.1.56-arm, images
.work/src/nanos                     nanos source, tag 0.1.56 checked out for citations
.work/src/ops                       ops source, tag 0.1.47
.work/hello/{raw.c,hello.c,probe.c,hello-arm64,raw-arm64,probe-arm64}
.work/tmp/alpine-root               Alpine 3.22 aarch64 rootfs + chromium 142
.work/tmp/chs                       Chrome for Testing 157 chrome-headless-shell (not booted)
.work/tmp/{chrome-run2.log,chrome-sandbox.log,chrome-hl.log,noarch.log}
.work/tmp/apk                       184 downloaded .apk files + APKINDEX
```

Nothing outside `docs/research/unikernel-nanos.md` was added to tracked paths,
and no commit was made.
