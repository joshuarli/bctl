# Kernel development

`kernel/` holds Hydrogen, bctl's fork of [NanoVMs Nanos](https://github.com/nanovms/nanos),
with the harness and probes that qualify it for Chromium's multiprocess
sandbox. This document explains how the fork is laid out, which rules kernel
changes follow, and what counts as proof. Hydrogen was a separate repository
until it was merged here with its full history, so its commit hashes still
resolve. Its campaign plan is in that history as `git show 4a862120:plan.md`.

## Layout

| Path | Contents |
| --- | --- |
| `kernel/nanos/` | the fork, subtree-merged from upstream with its history |
| `kernel/dev/` | `bootstrap.sh`, `doctor.sh`, `build.sh`, `image.sh`, `run.sh`, `debug.sh`, `test.sh`, and the pinned `toolchain.lock` |
| `kernel/tests/mini_chromium/` | the multiprocess probe, its raw variant, and fixtures such as `epoll_cow` |
| `kernel/tests/helium/` | the guest init, CDP driver, and HTTP fixture for the Helium gate |
| `kernel/tests/external/` | externally authored cases run natively and on Nanos |
| `docs/kernel/chromium-contract.md` | the syscall contract, cited by `file:line` from the Helium tree |

The scripts under `kernel/dev/` find their root from their own path and keep
scratch, toolchains, and runs under `kernel/.work/`. Run them from anywhere:

```sh
sh kernel/dev/bootstrap.sh && sh kernel/dev/doctor.sh && sh kernel/dev/test.sh g0
```

`runtime/build-nanos.sh` and `runtime/build-nanos-arm64.sh` build the
committed `kernel/` tree, identified by `git rev-parse HEAD:kernel`, and refuse
uncommitted kernel changes. Rebuilding the same tree produces a byte-identical
`kernel.img`.

## Upstream

Upstream Nanos is merged as a subtree, which keeps upstream commit hashes and
the Apache-2.0 attribution in `kernel/nanos/LICENSE`. The fork's base is
upstream `e4b9c9e65cd51ef29e5021835419a1d9a7733f20`. To take later upstream
changes:

```sh
git remote add upstream https://github.com/nanovms/nanos.git
git fetch upstream
git merge -X subtree=kernel/nanos upstream/master
```

## Rules for kernel changes

- Guest programs, the native reference oracle, upstream regression tests, and
  the browser all use musl. glibc is out of scope.
- Never stub a security-relevant syscall. An unsupported operation returns the
  error Linux would return (`ENOSYS`, `EINVAL`, `EPERM`, `EOPNOTSUPP`), never
  0. Fake sandbox state is forbidden.
- Each kernel feature has a named consumer, the probe check or Chromium call
  site that needs it, cited by `file:line` in `docs/kernel/chromium-contract.md`.
  It also has positive, negative, and failure-path tests. Its commit body says
  why the existing Nanos facility was insufficient.
- Add a syscall or flag to the contract, with its `file:line`, before
  implementing it.
- No kernel-global mutable state that is correct only because there used to be
  one process. Address spaces, processes, threads, file descriptions, and shared
  mappings use explicit reference counts and locking.
- The kernel never dereferences a user pointer belonging to a process other
  than the one whose page table is loaded.
- Private futexes key on (address space, address). Shared futexes key on
  (backing object, offset), so waits and wakes meet across processes. Any other
  kernel table keyed by a user address follows the same rule.
- Keep the kernel small. Extend Nanos's existing structures (`process`,
  `thread`, `vmap`, `fdesc`, the ELF loader, the scheduler) instead of adding a
  process manager or rewriting anything in another language.
- Put kernel regression tests in `kernel/nanos/test/runtime/` following
  upstream's convention: a `.c` file, a `.manifest`, and entries in that
  directory's `Makefile` and the top-level `RUNTIME_TESTS`.
- Externally authored tests: gVisor's syscall tests (Apache-2.0) are ported
  case by case with the upstream notice, pinned commit, file, and test name.
  Linux selftests (GPL-2.0) and Fuchsia Starnix are behavior references only:
  write independent tests and copy no code.
- When a lock waiter can spin with interrupts masked while the holder waits in
  a TLB flush rendezvous, take the lock with `spin_lock_flush`, which runs
  pending flush work while waiting.

## Toolchains

The kernel builds with GCC and binutils plus NASM, at the versions
`kernel/dev/toolchain.lock` names; `doctor.sh` fails on any other version.
The pinned LLVM cannot link the x86 UEFI loader, so the kernel does not build
with it. Guest programs use the pinned `llvm-musl` toolchain and a musl 1.2.6
sysroot that `bootstrap.sh` builds under `kernel/.work/`. Vendored kernel
dependencies (acpica, lwip, mbedtls) are checked out at pinned commits, and the
build fails if a checkout differs.

On Apple Silicon, `build.sh`, `make`, and `image.sh` run inside the
`kernel/dev/aarch64.Dockerfile` container, and `run.sh` boots through
`kernel/dev/krun-run.c` with the libkrun bundle named by `HYDROGEN_KRUN`
(bctl's `.work/runtime/nanos-krun`).

## What counts as a pass

The harness owns the hypervisor. `run.sh` starts QEMU with KVM directly, never
through `make run`, enforces its own deadline, and fails instead of falling back
to TCG. A guest passes only when all of these hold:

- the exit code is the expected one;
- each expected record appears exactly once;
- the terminal record appears exactly once;
- serial shows no `*** signal` or panic text.

A run writes `kernel/.work/runs/<utc>-<id>/` with `serial.log` and a
`result.json`. That file holds the source revision and dirty flag, the kernel,
image, and fixture digests, the toolchain versions, the command line, the
parsed records, and a one-line `reproduce` command.

Full qualification runs these `kernel/dev/test.sh` suites:

- `g0`, which is `harness`, `upstream`, and the debug check;
- `probe-native` and `probe`, whose checks must be identical natively and on
  Nanos;
- `probe --repeat 50` and `probe --stress`;
- `helium`;
- `9p` and `vsock`, on Linux x86_64 only.

A launcher's exit status alone never counts as a pass.

## History

- [Chromium syscall contract](chromium-contract.md)
- [Linux campaign report](campaign-report.md) and [Linux handoff](linux-resume.md)
- [macOS qualification](macos-qualification.md) and [macOS startup timing](macos-startup.md)
- [Crashpad cmsg handoff](handoff-crashpad-cmsg.md)
