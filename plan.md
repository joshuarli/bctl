# Coding Agent Campaign: Nanos Chromium Compatibility

You are operating in `bctl`, on a **Linux x86-64 host with KVM**. Your assignment is to establish and develop a fully open-source, source-rebuildable fork of [NanoVMs Nanos](https://github.com/nanovms/nanos) capable of eventually running Chromium with its native Linux multiprocess sandbox enabled.

This is an **implementation campaign**, not a request for a design document. Work autonomously, implement code, execute real guest tests, and commit verified changes.

## Mission and constraints

The final objective is a small, high-performance Chromium-compatible unikernel, supporting multiple MMU-isolated processes inside a single microVM.

Before attempting full Chromium integration, develop a small **Chromium compatibility probe** that exercises the essential Linux process, memory, IPC and security mechanisms Chromium requires.

The probe must prove actual behavior rather than simply checking whether syscall numbers exist.

Hard constraints:

- Host: Linux x86-64, QEMU/KVM. No macOS, HVF or arm64 work.
- Kernel foundation: Nanos, maintained as an independent source fork.
- Everything must build locally from public source.
- No proprietary runtime, proprietary guest additions, or opaque Nanos kernel images.
- No Chromium source build during the initial compatibility campaign.
- No `--no-sandbox`, `--single-process`, or `--no-zygote` as a qualifying workaround.
- No fake sandbox implementations or syscalls that silently report success.
- Prefer reusing and extending existing Nanos code.
- Keep the kernel small; do not turn it into a general Linux distribution.
- Preserve Nanos's existing capabilities and regression tests.
- Do not implement graphics, DRM/KMS, evdev, Wayland or seatd during this campaign.
- Do not modify bctl's existing SmolVM appliance or its acceptance criteria.
- Do not introduce Nix, systemd, Ansible, or heavy build orchestration.

The browser's sandbox is non-negotiable. VM-level isolation does not substitute for MMU separation and syscall confinement between renderer and browser processes within the guest.

### Required reading

Inspect these existing project files before making changes:

- `docs/research/unikernel-nanos.md`
- `docs/research/unikernel-unikraft.md`
- `docs/CAMPAIGN-ROADMAP.md`
- `docs/image-contract.md`
- `README.md`

Review upstream Nanos's `README.md`, `CHARTER.md`, build system, x86-64 page tables, process structures, syscall dispatcher, ELF loader and existing tests.

Study the Chromium Linux sandbox implementation, not just its high-level documentation:

- https://github.com/chromium/chromium/tree/main/sandbox/linux
- https://chromium.googlesource.com/chromium/src/+/main/docs/linux/sandboxing.md

Pin all source revisions examined. Do not make implementation decisions based on an assumed syscall contract when the actual Chromium source can resolve it.

## Repository architecture

Use independent sibling repositories:

```text
workspace/
├── bctl/
│   ├── dev/nanos/
│   │   ├── bootstrap.sh
│   │   ├── doctor.sh
│   │   ├── build.sh
│   │   ├── image.sh
│   │   ├── run.sh
│   │   ├── debug.sh
│   │   ├── test.sh
│   │   └── toolchain.lock
│   ├── tests/nanos/
│   │   ├── mini_chromium.c
│   │   ├── Makefile
│   │   └── fixtures/
│   └── docs/research/
│       └── nanos-chromium-campaign.md
└── nanos/
    ├── src/
    ├── test/
    └── docs/chromium-compat.md
```

Adapt filenames to existing conventions where appropriate.

`../nanos` is an independent Git repository, initially based on upstream release `0.1.56` (`aad473a`), unless a newer verified upstream commit supplies a materially better starting point. Pin the exact SHA and record why it was selected.

Use a dedicated development branch, `chromium-compat`. Configure the public NanoVMs repository as `upstream`. Preserve a separate `origin` for a user-owned fork if one exists; do not create an external repository or push to an unspecified remote.

Keep Nanos changes in Nanos. Keep browser qualification, execution harnesses and test fixtures in bctl.

Record the exact Nanos commit consumed by each bctl test run. Never consume a floating branch implicitly.

Commit both repositories in focused, independently reviewable increments.

---

# G0 — Full Nanos development environment

**This is a mandatory deliverable, not preliminary setup to be discarded.**

Build a reproducible, maintainable Nanos kernel development environment on the Linux x86-64 host.

### G0.1 Host toolchain

Inspect the host and install or locally provision the minimal required toolchain, including:

- GCC/binutils, make, NASM and required kernel build tools.
- QEMU x86-64 with KVM support and `qemu-img`.
- GDB, ELF inspection and symbolization tools.
- Git, Python 3 and required Nanos image utilities.
- Go, Ruby and `ent` where the upstream test/build workflow actually requires them.
- A suitable C compiler and libc toolchain for the guest test programs.

Do not assume a particular distribution or package manager. Detect the environment.

Prefer isolated or pinned local dependencies where practical. Avoid broad global changes.

Verify `/dev/kvm` access and that actual KVM acceleration works. Do not silently fall back to TCG.

Record compiler and tooling versions, Nanos source revisions, external source dependencies, and build configuration in `toolchain.lock`.

Nanos's build system fetches third-party dependencies. Identify all of them, record their exact commit SHAs, and ensure future bootstrap operations cannot silently change the effective source tree.

The source fork is Apache-2.0. Build the Nanos kernel from source; do not rely on NanoVMs's separately licensed commercial binary distributions.

### G0.2 Reproducible builds

Implement a cohesive CLI surface using small POSIX shell scripts:

```sh
dev/nanos/bootstrap.sh
dev/nanos/doctor.sh
dev/nanos/build.sh
dev/nanos/image.sh
dev/nanos/run.sh
dev/nanos/debug.sh
dev/nanos/test.sh
```

Required behavior:

- `bootstrap.sh` provisions/checks the environment and initializes the sibling fork.
- `doctor.sh` diagnoses every missing prerequisite and prints an actionable report.
- `build.sh` incrementally builds the source fork, including any necessary klibs and boot artifacts.
- `image.sh` packages the specified test executable, its runtime libraries, and any supporting files.
- `run.sh` boots the resulting image with QEMU/KVM, captures serial output, enforces a timeout, and reports actual guest success or failure.
- `debug.sh` launches a debug kernel with a QEMU GDB stub and usable symbol files.
- `test.sh` executes the selected regression suites and compatibility gates.

Use Nanos's native Makefile tooling, including `make kernel` and its existing runtime tests. Wrap it rather than replacing the upstream build system.

Avoid using `ops` as a black box. It may be used for image construction if necessary, but explicitly supply the source-built kernel and verify the resulting image identity.

All scratch files, downloads, build products and logs should be under ignored `.work/` paths.

### G0.3 Debugging and observability

Provide:

- Symbolized kernel backtraces.
- Unstripped debug ELF artifacts.
- Kernel page-fault diagnostics.
- Deterministic panic reporting.
- QEMU serial logs.
- Guest syscall tracing where available.
- A documented GDB attach workflow.
- Debug builds with KASLR disabled when required.
- Explicit QEMU and guest exit-status handling.

The VM harness must not treat a successful QEMU launcher exit or an `ops` exit code of zero as proof that the guest passed. Require an explicit guest test-completion record and expected assertions.

Preserve logs from failing tests.

Make it possible to reproduce any reported failure with one command, from a clean checkout and pinned sources.

### G0.4 Baseline validation

Run and record:

1. A completely source-built Nanos kernel booting on KVM.
2. A static x86-64 hello-world executable.
3. A dynamically linked executable with its loader and libraries.
4. Existing Nanos process/thread, memory, filesystem, socket and futex regression tests, where supported.
5. A GDB session reaching a kernel breakpoint.
6. A second clean build producing equivalent artifacts from unchanged inputs.

Record kernel boot latency, image size and idle guest memory as baseline metrics. Do not optimize yet.

**G0 passes only when a fresh checkout can rebuild, boot, debug and test the custom Nanos kernel without opaque kernel binaries.**

---

# G1 — Establish a genuine multiprocess architecture

This is the decisive feasibility gate.

Nanos is designed around a single application. Its existing `process` structures do not imply independent MMU address spaces.

Investigate at least:

```text
src/unix/unix.c
src/unix/unix_internal.h
src/unix/thread.c
src/unix/exec.c
src/x86_64/page.c
src/x86_64/page_machine.h
src/kernel/
```

Understand the current virtual-memory model, its use of CR3, user mappings, page faults, scheduler transitions, global kernel state and process lifetime before implementing anything.

Pay particular attention to assumptions that guest virtual addresses uniquely identify physical mappings across the whole VM.

### G1.1 Architectural proof

Implement the smallest possible modification supporting two independent user address spaces, with shared kernel mappings but separate user mappings.

The first executable should:

1. Start process A.
2. Start process B.
3. Map the same virtual address in each.
4. Write distinct contents.
5. Switch repeatedly between the two processes.
6. Verify that each sees only its own contents.
7. Verify that each cannot modify the other's private memory.

Test with one vCPU, then at least two vCPUs to expose TLB and scheduler issues.

Prove that user processes cannot read or write kernel memory.

Do not advance if address-space isolation is merely simulated by distinct process structures or bookkeeping.

### G1.2 Process lifecycle

Once the architectural proof passes, implement the required Linux process semantics, initially for x86-64:

- `clone()` creating a process without `CLONE_THREAD`.
- `fork()` with correct parent/child execution state.
- Copy-on-write private mappings.
- `vfork()` or the semantics required by the selected Chromium version.
- `execve()` of a normal Linux ELF, including its dynamic interpreter.
- `wait4()` and child exit-status reporting.
- Process IDs, parent relationships and cleanup.
- Signal delivery and termination.
- Correct file descriptor inheritance and close-on-exec.
- Interaction with existing `pthread`/thread cloning behavior.

Do not build a general-purpose process manager beyond what this scope demands.

Reuse Nanos's existing ELF loader, filesystem, memory allocator, thread scheduler and POSIX structures.

Explicitly model the ownership and lifetimes of address spaces, processes, threads, files and shared mappings. Avoid global mutable state that is valid only because the old kernel had one process.

Test parent/child memory isolation, COW faults, `execve` replacement, zombie cleanup, inherited descriptors, exit races, concurrent process creation, and resource exhaustion.

Every new syscall requires positive tests, negative tests, and relevant failure-path tests.

**G1 passes when a normal Linux test binary can repeatedly fork, exec, communicate and wait on an MMU-isolated child, without corrupting other processes or the kernel.**

---

# G2 — Chromium-style interprocess communication

Build on the real multiprocess foundation.

First inventory what Nanos already implements. For example, `memfd_create` and file sealing exist in Nanos's shared-memory code. Do not duplicate these implementations.

Extend only missing semantics that our test fixture and selected Chromium release require.

Relevant capabilities:

- `socketpair(AF_UNIX, SOCK_STREAM)` and `SOCK_SEQPACKET` where required.
- `sendmsg()`/`recvmsg()` with genuine `SCM_RIGHTS` file descriptor passing.
- Proper descriptor reference counts and cleanup.
- `SO_PEERCRED`, `SO_PASSCRED`, and credential messages.
- `memfd_create()`, `ftruncate()`, shared `mmap()`, and sealing.
- `eventfd`, `epoll`, pipes and related polling behavior.
- Cross-process shared-memory mapping and visibility.
- Futex operations, including the relevant robust and wakeup semantics.
- Correct Unix socket shutdown, blocking and error behavior.

Use the existing Nanos runtime tests as a foundation.

The critical proof is cross-process transfer of a shared-memory FD:

```text
browser
  creates memfd
  writes a payload
  sends FD over Unix socket
       |
       v
renderer
  receives the actual FD
  maps the object
  reads payload
  modifies allowed shared region
       |
       v
browser
  observes the modification
```

Include denial/error tests for malformed control messages, invalid file descriptors, closed peers and permission violations.

Verify that private memory remains private even while shared memory is intentionally shared.

**G2 passes only when the communication takes place between real isolated processes.**

---

# G3 — Implement an actual Linux-style renderer sandbox

This gate must be fail-closed.

The goal is not simply to satisfy Chromium's feature-detection code. It is to enforce the corresponding security properties.

### G3.1 Credentials and privilege state

Implement the required subset of user/group credentials:

- Real/effective UID and GID semantics.
- `setuid`, `setgid` and relevant related operations.
- Inheritance and privilege transitions.
- `PR_SET_NO_NEW_PRIVS` and `PR_GET_NO_NEW_PRIVS`.
- Correct `execve` behavior under `no_new_privs`.

Remove or replace any Nanos stubs that claim success without changing the required security state.

For example, setting `NO_NEW_PRIVS` must affect real kernel state and read back correctly.

### G3.2 Namespaces

Determine the exact namespace operations used by the selected upstream Chromium zygote implementation.

Implement the minimal correct subset of:

- User namespaces.
- PID namespaces.
- Network namespaces.
- Required mount or IPC namespace operations.
- `clone()` namespace flags and `unshare()`.
- Namespace inheritance and cleanup.
- UID/GID mapping interfaces and relevant procfs state.

Preserve actual namespace isolation. A process reporting a separate PID namespace while retaining unrestricted visibility or access to unrelated processes is not acceptable.

If a namespace cannot be implemented correctly, return a real unsupported error and fail the gate.

Do not substitute fake success based on the fact that the entire unikernel resides inside a microVM.

### G3.3 seccomp-BPF

Implement actual syscall filtering with the Linux-compatible classic BPF format and relevant `seccomp()`/`prctl()` interfaces.

Requirements:

- Validate filter programs and reject invalid instructions or control flow.
- Enforce the actual filter against every applicable syscall entry.
- Respect x86-64 syscall architecture and argument semantics.
- Support the actions required by Chromium's pinned policy, including allow, errno, trap and process/thread termination as applicable.
- Correctly handle policy inheritance and the installation constraints enforced by `no_new_privs`.
- Prevent bypass through alternative syscall entry paths.
- Reject unsupported flags or actions rather than silently ignoring them.

Before writing a generic interpreter, inspect Nanos's existing syscall hooks, `pledge` implementation and any BPF-related infrastructure to identify reusable mechanisms.

Keep the implementation focused, safe and testable.

### G3.4 Security acceptance tests

The probe must independently verify:

- Renderer and browser have separate address spaces.
- Renderer cannot read or overwrite browser-private memory.
- Renderer cannot ptrace or otherwise alter a more privileged process.
- Renderer cannot access prohibited network destinations.
- Renderer cannot escape its filesystem/namespace restrictions.
- Seccomp denies a deliberately prohibited syscall.
- Allowed syscalls continue to function.
- Malformed BPF policies are rejected.
- NNP and seccomp state survives the relevant process transitions.
- `/proc/self/status` reports factual `NoNewPrivs`, `Seccomp`, UID/GID and process state.

A report containing `NoNewPrivs: 1` and `Seccomp: 2` is useful corroboration, but it is insufficient without enforcement tests.

**G3 passes only when negative tests prove the relevant security boundaries actually hold.**

---

# G4 — The miniature Chromium executable

Create one small C executable:

`tests/nanos/mini_chromium.c`

It must compile for normal Linux and for the Nanos guest, using the same test logic.

It should emulate the important architecture of Chromium without including Chromium itself.

The executable should support role selection through command-line arguments so it can `execve` itself as different processes.

Suggested structure:

```text
mini_chromium --role=browser
    |
    | clone/fork
    v
mini_chromium --role=zygote
    |
    | create sandboxed child
    v
mini_chromium --role=renderer
    |
    | restricted syscalls
    | shared memory
    | Unix IPC
    v
report results to browser
```

The browser role creates resources, launches children, sends work, waits for responses and verifies that sandboxed processes cannot perform prohibited operations.

The zygote role exercises Chromium-relevant process initialization and namespace setup.

The renderer role installs the required restrictions, receives shared-memory descriptors, performs work, communicates the result, and runs deliberate security-negative tests.

Use real syscalls and libc behavior. Avoid a fake protocol that simply prints success.

### Exact acceptance contract

The executable must verify, at minimum:

| Category | Required proof |
|---|---|
| Processes | `clone`/`fork`, `execve`, `wait4`, PID identity |
| Memory | Per-process VM, COW, shared mappings, `mprotect` |
| IPC | Unix socket, FD passing, peer credentials |
| Synchronization | Threads, futexes, epoll/eventfd |
| Sandbox | Namespaces, credentials, NNP, seccomp |
| Isolation | Invalid memory access and privileged syscalls denied |
| Lifecycle | Exit, signals, resource cleanup and repeated launch |

Build the probe once for native Linux as the reference oracle and once for the Nanos appliance.

Use libc-linked builds representative of a normal Linux workload, not only a freestanding raw-syscall executable. Maintain an optional raw-syscall variant to isolate kernel bugs from libc behavior.

Use an explicit, machine-readable result protocol:

```text
MINI_CHROMIUM_TEST name=process_fork status=PASS
MINI_CHROMIUM_TEST name=vm_isolation status=PASS
MINI_CHROMIUM_TEST name=scm_rights status=PASS
MINI_CHROMIUM_TEST name=seccomp_enforcement status=PASS
MINI_CHROMIUM_RESULT status=PASS passed=... failed=0
```

The harness must reject missing records, duplicate terminal records, crashes, timeouts and unexpected skips.

Store structured JSON results in ignored `.work/` directories.

For each run record the exact source SHAs, binary digest, kernel digest, host toolchain, QEMU settings, elapsed time and observed results.

Run at least 50 repeated cold-boot executions after all tests pass, with additional concurrent-process stress testing.

A passing fixture is a necessary compatibility proof, **not a guarantee of full Chromium compatibility**. It must be treated as evidence that the core architectural prerequisites are ready for browser integration.

---

# G5 — Validate a prebuilt Chromium binary

Do not rebuild Chromium.

After G1–G4 pass, try an existing x86-64 Linux Chromium binary:

1. Prefer a version-pinned, already-built Chromium or Helium artifact from `chromium-build` if available.
2. Otherwise use a version-pinned distribution Chromium package with its complete runtime dependency closure.
3. Package the binary and libraries with the custom Nanos kernel.
4. Run in headless mode initially, with the actual sandbox enabled.
5. Capture startup logs, syscall failures, process state and sandbox status.

The minimum browser validation is:

- A live browser process with separate zygote/renderer processes.
- Successful CDP `/json/version`.
- Navigate to a local HTTP fixture.
- Evaluate JavaScript or retrieve page DOM through CDP.
- Confirm renderer `NoNewPrivs: 1` and `Seccomp: 2`.
- Verify actual sandbox confinement.
- Exit cleanly and release all guest resources.

Do not proceed to a full custom Chromium compilation until this test exposes a specific need that cannot be solved with the prebuilt binary or a narrow kernel compatibility change.

If Chromium fails, diagnose the earliest causal failure using tracing and implement the smallest correct kernel fix with a standalone regression test.

Do not patch Chromium merely to conceal unsupported kernel semantics.

No graphics work is included in G5. A later campaign will introduce headed Chromium, DRM/KMS, input and the bctl compositor integration.

---

# Engineering discipline

## Correctness

- No silently stubbed security syscalls.
- No uncontrolled kernel-global mutable process state.
- No use-after-free or reference-counting shortcuts in process and FD lifetimes.
- Explicit locking and cleanup rules.
- Tests for resource exhaustion and malformed userspace inputs.
- Never treat an unsupported feature as implemented merely because the syscall returns zero.
- Keep existing Nanos semantics unchanged unless the new process architecture requires a justified modification.

## Minimality

Do not implement Linux interfaces just because they exist.

Every new kernel feature must have a named consumer, a test demonstrating its requirement, and an explanation of why existing Nanos facilities are insufficient.

Avoid new general-purpose abstractions unless shared functionality clearly justifies them.

Do not rewrite Nanos in Rust. Extend its existing C architecture appropriately.

## Testing

For every feature:

1. Establish a baseline on normal Linux.
2. Demonstrate the failing behavior on the pinned Nanos baseline.
3. Implement the Nanos kernel change.
4. Re-run the same fixture.
5. Run affected existing Nanos tests.
6. Add failure-path and isolation tests.
7. Commit with proof in the commit message or accompanying report.

Use timeouts and preserve logs. Never retry indefinitely or silently downgrade a failed condition.

Maintain a gate matrix with `PASS`, `FAIL`, `BLOCKED` or `NOT RUN`. No optimistic statuses.

## Scope and execution

Implement G0 first, completely. Then advance sequentially.

G1 is an explicit architectural checkpoint. If it reveals that Nanos's global page-table assumptions require broad redesign, write down the concrete dependency graph and smallest possible redesign before continuing.

Do not abandon the campaign merely because a feature is absent upstream—that is the reason this fork exists.

However, stop a dependent implementation when the preceding gate has a proven hard blocker that cannot be resolved within the current architecture. Preserve all verified work instead of adding untested speculative layers.

You may continue independent tests, code archaeology and documentation without compromising those gates.

Commit successful work in small increments to each local repository. Do not push externally without an existing authorized destination.

---

# Final deliverables

At completion, provide:

1. A working Linux x86-64 Nanos source-development environment.
2. An independent Nanos fork with verified kernel compatibility changes.
3. A reproducible KVM boot, image-building and GDB debugging workflow.
4. The standalone `mini_chromium` compatibility executable.
5. A native Linux reference run and actual Nanos guest runs.
6. Negative tests establishing real process, memory and sandbox isolation.
7. The upstream regression test results.
8. A structured gate status report and exact code revisions.
9. A compact benchmark report containing cold-boot latency, kernel/image size, guest RSS, process creation and IPC measurements.
10. `docs/research/nanos-chromium-campaign.md`, documenting architecture, implementation, measured evidence, unresolved failures and the next minimal step toward an unmodified Chromium binary.

Prioritize correct, reusable kernel primitives over optimizing microbenchmarks.

Success is not that the probe compiles, nor that syscall probes print plausible values. Success is a **small, independently rebuildable Nanos fork that actually executes a Chromium-shaped multiprocess application with enforceable security boundaries**.

Begin by establishing the source-built Nanos environment and reproducing the G1 process-isolation failure. Implement the first architectural fix, validate it on KVM, and continue through the gates as far as proven results permit.
