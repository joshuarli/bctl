# Hydrogen: Nanos with a real multiprocess Linux sandbox

Hydrogen is a proof-of-concept monorepo. It holds a source fork of
[NanoVMs Nanos](https://github.com/nanovms/nanos) and the harness and probes
showing that the fork can host a Chromium-shaped multiprocess application,
with MMU-isolated processes and an enforced renderer sandbox, inside one KVM
microVM on Linux x86-64. The end proof is the patched Helium 0.20.3.1 binary
(Chromium 156.0.8078.79) running headless with its sandbox on. If this works,
bctl (`~/d/bctl`) adopts it; the owner hands bctl to this campaign's owner (2026-11-10).

## Status

- **macOS resume (2026-11-12):** ARM harness, process probes, Helium, and
  200 fresh virtio-fs boots pass after moving filesystem initialization out
  of the storage spinlock. Linux 9p, virtio-fs, and process regressions pass.
  [macOS qualification](docs/macos-qualification.md) records the failure,
  repair, evidence, and independent-review gap.
- **Goal:** gates G0–G5 below, then the optimization pass and close.
- **Latest kernel revision:** `0bb4a9d9` (empty-flush cleanup, page-cache
  TLB rendezvous, vmap traversal, 9p metadata retention, and probe stress).
  Verification: [Linux report](docs/campaign-report.md).
  Work stopped at the owner's request. Resume from the
  [Linux handoff](docs/linux-resume.md).
  The final harness and browser checks pass. Intermittent bctl shutdown and
  two reviewed 9p ownership risks still block full Linux qualification.
- **Accepted:** G0. H. **G1** (G1.1 address spaces; G1.2 COW,
  zombies/wait, clone flags, vfork; G1.3 `execve`/`execveat`, per-slot
  close-on-exec, the lost-signal race in `blockq_check` plus signal IPIs;
  G1.4 `process_stress`: 1000 fork/exec/wait, 64 concurrent children,
  whole-process fatal signals, 20/20 on 2 and 4 vCPUs). X1 corpus. G5 prep.
  G2.2 (shared futex keys in one kernel table, REQUEUE/bitsets, robust
  list owner checks and a cycle bound, memfd flags, seals and truncate
  across processes, an `mremap` use-after-free from G1.2; `shmem_xproc`
  20/20 on 1 and 2 vCPUs; upstream `tlbshootdown` smp 2 now passes).
  G2.3 (AF_UNIX `shutdown`, `EPOLLRDHUP`/`EPOLLHUP`, `SIGPIPE` on stream
  sockets and pipes as Linux does, `MSG_DONTWAIT`/`MSG_NOSIGNAL`/`MSG_TRUNC`,
  `membarrier` private expedited; `ipc_xproc` 20/20 on 1 and 2 vCPUs).
  G3.1 (credentials and capabilities, a `prctl` table, a dynamic procfs on
  `/proc`, and an `mmap` bad-fd lock leak; `creds_procfs` 20/20 on 1 and 2
  vCPUs). G3.2 (seccomp-BPF: validator, interpreter, strict mode, TSYNC,
  the standard actions with forced SIGSYS, one dispatch point shared with
  strace; `seccomp` fixture green on 1 and 2 vCPUs, also under strace).
  Upstream 64 pass.
  G3.3 (user namespaces with one-shot id maps and scoped capabilities, a
  shared `fs_struct`, `chroot` with a root-clamped lookup, procfs
  `root`/`ns`/map files; `userns` 20/20 on 1 and 2 vCPUs, 11 checks native).
  G2.1 (SCM_RIGHTS/SCM_CREDENTIALS, SO_PASSCRED, SO_PEERCRED from the owner's
  session; `scm_rights` 20/20 on 1 and 2 vCPUs; gVisor cmsg 18/18).
  Realtime timers with a deadline before boot now fire (they wrapped to a
  far-future expiry and hung `shmem_xproc` intermittently).
  G3.4 (pid namespaces with per-level numbers and the PID-1 rules; net
  namespaces with isolated inet sockets; `pidns` 7 and `netns` 3 checks on
  1 and 2 vCPUs and native). Probe on Nanos: 55/55 at 1, 2 and 4 vCPUs.
  Abstract and autobound `AF_UNIX` addresses (gVisor peercred 3/3).
  Cross-process `tkill`/`tgkill`/`sigqueueinfo`. A syscall nullified by
  a blockq flush restarts when no signal is delivered (it leaked errno
  512). procfs numbers pids in its mount namespace and unlinks dead file
  nodes. Re-exec through `/proc/self/exe` keeps the real exe name. G5
  runs the headless archive `helium-0.20.3.1-r5` with `--headless=new`;
  native helium 10/10. Helium on Nanos passes all 10 records (CDP,
  navigate/evaluate/DOM, zygotes, sandboxed renderers, confinement, clean
  exit), 10/10 runs and 12/12 under 12 parallel VMs, after fixes to netlink
  `MSG_DONTWAIT`, thread exit, `/proc/<pid>/cmdline`, per-process
  `RLIMIT_DATA` (V8 code range), stream-socket `EPOLLOUT` wakeups, and brk
  over mprotect-split heaps.
  `process_visible_pid` translates through `pid_nr_ns` (`24355d89`); the
  probe checks that an invisible sender has pid 0.
  **Red:** setrlimit accepts
  and drops resources it does not model (`RLIMIT_AS`, `RLIMIT_CPU`, ...),
  `RLIMIT_DATA` bounds brk but not private writable mmaps, and
  `prlimit64` on another pid reads and writes the caller's limits.
  bctl's Nanos backend (bctl M7N) runs Helium headless in sessions and found
  three kernel bugs, now fixed with tests: an unprivileged `gid_map` was
  checked against the euid (Chromium's namespace probe failed for a browser
  user whose gid differs from its uid), a host vsock connect during early
  boot asserted before the socket layer existed, and guests with more than
  2 GiB asserted in `init_shared_pages` (harness `cow_stress_6g`).
  G4.1 accepted: the probe's 55 named checks are identical natively and on
  Nanos at 1, 2 and 4 vCPUs and cover every Acceptance row; the raw variant
  passes.
- **Running lane:** none. G4.2's stress implementation is integrated, and
  its timing report is in `docs/campaign-report.md`.
- **Final acceptance at `409d61e1`:** `g0` (harness on 1 and 2 vCPUs,
  upstream matching **Baseline**, the debug check), `9p`, `vsock`,
  `probe --native`, `probe` (55/55 and raw 6/6 at 1, 2 and 4 vCPUs),
  `probe --repeat 50`, `probe --stress` (1, 2, 4 vCPUs), `helium` (10/10
  records), `helium-stress` (10/10 parallel boots) and `external` all pass.
- **Next:** the open tickets in `TODO.md`: HYD-3 (null scheduler task,
  2,400 clean loop cycles since `0523dbbc`) and HYD-4 (browser outlives
  `SIGKILL` at shutdown). Perf gate deferred: host busy.
- **Gate matrix:** G0 PASS · H PASS · G1 PASS · G2 PASS · G3 PASS · G4 PASS · G5 PASS · P NOT RUN.

Only `PASS`, `FAIL`, `BLOCKED` and `NOT RUN` are allowed. A gate is `PASS`
only when every slice in it is accepted on `main`.

## Charter

- Decide and keep going. Record a decision in **Decisions** only when a later
  slice depends on it. Otherwise the commit body carries it.
- Run unattended inside the grant in **Execution boundary**. A plan does not
  supply missing permission. Park any item that needs authority outside the
  grant, and continue with independent items.
- Hard stops. Each one parks the affected item: an action outside the grant;
  a boundary this doc forbids; a loop that cannot check anything; a proven
  architectural blocker in a gate's prerequisite (park everything that
  depends on it, keep independent slices and documentation going).
- Parity first: every gate works end to end before any optimization. Product
  performance (boot time, RSS, fork/IPC latency) is measured and logged, never
  gated (owner decision 6).
- Fix egregious problems you meet in their own commit, at these thresholds: a
  harness boot of the static hello fixture taking over 5 s; an incremental
  `dev/build.sh` over 60 s after a one-file change; any guest test over 10×
  its native-host wall time when the cause is the kernel or the harness; a
  kernel panic or hang from user input in code you did not otherwise touch.
  Record anything smaller in the slice's commit body and leave it.
- Failing item rule: find the cause before retrying (`root-cause`). Never
  retry the same change unchanged. Three attempts per slice; then park it on a
  branch `park/<slice-id>` with the cause, the evidence and a retry
  condition, and rewrite **Status**.
- One writer by default. Parallel lanes only for the pairs named in
  **Sequence**, each in a worktree at `.work/wt/<lane>`, following
  the `orchestrate` skill's worktree lifecycle.
  Never `/tmp`, and never `$TMPDIR` outside `.work/tmp`.
- After each accepted slice, run the
  parent steering checkpoint from the `orchestrate` skill's campaign reference
  and rewrite **Status** in place. Never append checkpoints, logs or
  handoffs to this doc. Git history is the log.
- Every commit is a Conventional Commit whose scope is the workstream
  (`nanos`, `dev`, `probe`, `helium`, `docs`). The slice ID goes in the body
  with the acceptance command and its observed result. Code comments never
  cite this doc, a gate or a slice.
- No silently stubbed security syscalls. An unsupported operation returns a
  real Linux error (`ENOSYS`, `EINVAL`, `EPERM`, `EOPNOTSUPP` as Linux would),
  never 0. A feature that cannot be made correct makes its gate `FAIL`.
- Each kernel feature has a named consumer (the probe check or Chromium call
  site that needs it, by `file:line` in `docs/chromium-contract.md`), positive,
  negative and failure-path tests, and a commit body explaining why the
  existing Nanos facility was insufficient.
- Process lifetime rules: no kernel-global mutable state that is correct only
  because there used to be one process; explicit refcounts and locking for
  address spaces, processes, threads, file descriptions and shared mappings;
  no use-after-free shortcuts. Rule for every new cross-process path: the
  kernel never dereferences a user pointer belonging to a process other than
  the one whose page table is loaded.
- Keep the kernel small. Extend Nanos's existing C structures (`process`,
  `thread`, `vmap`, `fdesc`, the ELF loader, the scheduler); do not add a
  general process manager or rewrite anything in another language.

## Decisions

### Owner (2026-11-09)

1. **One monorepo.** Everything lives in hydrogen (`~/d/hydrogen`), including
   the Nanos fork. Superseded 2026-11-10: the owner gave this agent full
   ownership of bctl, so bctl may be built, run and changed under its own
   repo rules, and bctl may read and run hydrogen.
2. **musl only.** Guest programs, the native reference oracle, upstream
   regression tests and the browser all use musl. glibc is out of scope: no
   glibc sysroot, container or loader.
3. **The browser is the patched Helium.** The Chromium sandbox contract is
   read from the patched Helium 0.20.3.1 source tree that produced the
   accepted `helium-0.20.3.1-linux-x86_64-musl.tar.xz`: Chromium
   156.0.8078.79 + Helium core `2f81b9cafa38a95071e20de3f9cd6b0aca6d3c65` +
   helium-linux `83027a19267e526f08e25a3af1a30b45ee7e3e2e` + chromium-build's
   patches. G5 runs that artifact. Vanilla Chromium and Alpine `chromium` are
   not used.
4. **Helium source preparation is authorized.** The executor may run
   `~/d/chromium-build/chromium-build --product helium --profile helium-wayland-release`
   `image`, `fetch` and `prepare` (never `build` or any later step) with
   `HELIUM_WORK=hydrogen-helium-0.20.3.1-src-amd64`. That means about 3.2 GB of
   locked downloads into `~/d/chromium-build/.work` plus that Docker volume.
   It must not edit chromium-build's tracked files.
5. **Pushing.** After each accepted slice, push hydrogen `main` to `origin`
   (`git@github.com:joshuarli/hydrogen`). No other push target, no force
   push, no new remote repositories. `upstream` is fetch-only.
9. **Unattended with broader grant** (owner, 2026-11-09 night): proceed
   without the owner and make the judgment calls. Installing tools and
   forking or vendoring dependencies are preapproved (prefer pinned local
   installs under `.work/` or vendored sources with recorded provenance;
   host `apk` installs are allowed when a local install is impractical, and
   recorded in `dev/toolchain.lock`). The other forbidden effects stand.
6. **Performance gates are waived.** Only feature completeness counts.
   Measurements are still taken and reported, but they never gate a slice and
   `perf-gate-ready` is not consulted.
7. **Kernel changes are authorized** (owner, 2026-11-09, after the
   non-kernel work landed): fix the baseline bugs in the fork. Larger
   architectural kernel work (G1 onward) starts after the owner has seen the
   G1.1 design note. The owner then delegated the G1 go/no-go to the
   executor (2026-11-09): start G1 unless the design note proves a hard
   blocker.
8. **G5 display** (owner, 2026-11-09). Helium runs with `--headless=new`
   from the headless-Ozone archive `helium-0.20.3.1-r5` (sha256
   `f4b06e80a097587e508982a3c73f043dbd80e3660aab6d47f72648b6ffc36a05`). No
   compositor, DRM, KMS, evdev, virtio-gpu or kernel graphics.

### Prescribed

- **Nanos base: upstream master `e4b9c9e65cd51ef29e5021835419a1d9a7733f20`**
  (2026-11-08), 25 commits after release 0.1.56 (`aad473a`). It is preferred
  over 0.1.56 because it contains fixes this campaign needs: PROT_NONE
  mmap/mprotect (`9fe2ab36`), manifest-provided uid/gid credentials
  (`ab823eb4`, the starting point for G3.1) and virtio used-ring ordering
  (`f04f9ea3`). It builds clean on this host (see **Baseline**). Reverse to
  0.1.56 only if G0.4 shows a regression on master that 0.1.56 lacks.
- **Fork layout: subtree-merged with full history.** Add remote `upstream` =
  `https://github.com/nanovms/nanos.git`. Import with
  `git merge -s ours --no-commit --allow-unrelated-histories e4b9c9e && git read-tree --prefix=nanos/ -u e4b9c9e && git commit`.
  `git subtree` is not installed here. Keeping the history preserves upstream
  SHAs and the Apache-2.0 attribution, and later syncs are
  `git merge -X subtree=nanos upstream/master`. The draft's separate
  `chromium-compat` branch is superseded: hydrogen `main` is the
  integration branch, and a run records the hydrogen commit SHA plus whether
  the tree was dirty.
- **Repository layout** (the draft's bctl paths, renamed for hydrogen):

  ```text
  hydrogen/
  ├── nanos/                       the fork (src/, test/, docs/chromium-compat.md)
  ├── dev/                         bootstrap.sh doctor.sh build.sh image.sh run.sh debug.sh test.sh toolchain.lock
  ├── tests/mini_chromium/         mini_chromium.c, mini_chromium_raw.c, Makefile, fixtures/
  ├── tests/helium/                guest_init.c, CDP driver, HTTP fixture
  ├── docs/chromium-contract.md    the syscall contract, cited from the Helium tree
  └── docs/campaign-report.md      the final report (deliverable 10)
  ```

  Kernel regression tests go in `nanos/test/runtime/` following upstream's
  convention (a `.c` file, a `.manifest`, and an entry in
  `nanos/test/runtime/Makefile` and the top-level `RUNTIME_TESTS`).
- **Kernel toolchain: the host's GCC 15.2.0 and binutils (Alpine), with
  NASM.** Upstream supports it and it builds clean. The pinned LLVM can't
  build the kernel yet: lld rejects the UEFI loader's `R_X86_64_PC32`
  relocations against `IMAGE_BASE`/`efi_main`. `doctor.sh` fails unless the
  exact versions in `toolchain.lock` are present. Porting the kernel to the
  pinned LLVM is not in scope.
- **Guest toolchain: `llvm-musl-23.1.5-c4f9b08`** from
  `laputa-systems/llvm-prebuilt-musl`, asset
  `clang+llvm-23.1.5-x86_64-linux-musl.tar.xz` (sha256
  `8039dfa9e5b2ee53b04c1dcf94d65bd76c27842b0d4128c5a7bc6ba21d345fec`),
  unpacked to `.work/toolchain/`. The guest sysroot (headers, `libc.so`,
  `ld-musl-x86_64.so.1`) is musl 1.2.6, built by `bootstrap.sh` from the
  release tarball (sha256
  `d585fd3b613c66151fc3249e8ed44f77020cb5e6c1e635a616d3f9f82460512a`) with
  that clang into `.work/sysroot`. Static links use the package's bundled
  musl 1.2.6 `libc.a`. The same binaries are the native oracle, run directly
  on this host (Alpine edge, musl 1.2.6).
- **Vendored kernel dependencies are pinned by commit.** Change
  `nanos/Makefile:56-58` and `cmd_vendor` (`nanos/rules.mk:168`) to fetch
  and check out exactly these commits, and fail if `HEAD` differs:
  acpica `e01cc6b3d12b5f73f44d46fa15a7f569c793b328` (tag `R09_30_21`), lwip
  `e27f9b7af7e5b025a5b8803899f12b1d35f7d968` (`STABLE-2_1_x` head on
  2026-11-09), mbedtls `f93c44d01d716315dfebadcb799760b548e76822` (default
  branch head on 2026-11-09).
- **Go for upstream Go tests** (`fst`, `hwg`, `webg`): a go.dev release
  tarball unpacked under `.work/toolchain/go`. Its version and sha256 go in
  `toolchain.lock`. Go binaries are static and do not touch libc. Install Ruby
  or `ent` only if a selected upstream suite fails without it, and record why
  in `toolchain.lock`.
- **The harness owns QEMU.** `run.sh` invokes `qemu-system-x86_64` directly,
  with `-enable-kvm -cpu host`, `-device isa-debug-exit`, `-no-reboot` and
  user networking without fixed host ports. It never goes through
  `make run`, because `nanos/platform/pc/Makefile:351` maps QEMU's failure
  exit 1 to 0, and its fixed `hostfwd=tcp::8080` collides on this host. It
  enforces its own deadline and treats a deadline kill as `TIMEOUT`, because
  Alpine's BusyBox `timeout` returns the child's status and QEMU exits 0 on
  SIGTERM. A guest passes only when all of these hold: QEMU exit `(code<<1)|1`
  with the expected code, the fixture's expected records present exactly once,
  a terminal record present exactly once, and no `*** signal` or panic text on
  serial. If `/dev/kvm` acceleration is unavailable, the run fails. It never
  falls back to TCG.
- **arm64 on Apple Silicon runs the same suites under Hypervisor.framework.**
  On macOS, `dev/build.sh`, `make` and `dev/image.sh` run inside the
  `dev/aarch64.Dockerfile` container, with the checkout mounted at its host
  path so manifests stay valid. `bootstrap.sh` builds the musl 1.2.6 sysroot
  there with gcc and its `musl-gcc` wrapper. `run.sh` boots through
  `dev/krun-run.c` and a libkrun bundle named by `HYDROGEN_KRUN` (bctl's
  `.work/runtime/nanos-krun`, which holds `lib/libkrun.dylib` and
  `firmware.fd`). Native oracle runs stay in the container, which gets
  `seccomp=unconfined` and `SYS_PTRACE` so user namespaces work and Helium's
  confinement check can read a renderer's namespaces. Guest programs link at
  0x200000 on both architectures, so `low_user_va` probes the same layout.
  libkrun has no semihosting, so the kernel's PSCI fallback prints the exit
  code and `run.sh` reads it from serial. `--net` is not supported there.
  `HELIUM_ARCHIVE` must name the pinned arm64 Helium 0.20.3.1-r6 archive;
  Alpine's aarch64 musl package is checked by its pinned sha256 only, because
  `apk verify` needs an Alpine host.
- **Host directories over virtio-fs.** `nanos/src/virtio/virtio_fs.c` speaks
  FUSE 7.31 over a virtio-fs device (PCI or MMIO), and `nanos/src/fs/virtiofs.c`
  mounts it as the volume named by the device's tag, so a manifest's
  `mounts:(TAG:/path)` places it. `image.sh --mount TAG:/path` writes that
  entry. `run.sh --virtiofs TAG:DIR` shares DIR: under QEMU it starts
  `$VIRTIOFSD` (default `virtiofsd` on `PATH`, unprivileged with
  `--sandbox=none`) and backs guest memory with a shared memfd; under HVF it
  passes the share to libkrun, which has virtio-fs built in.
  `dev/test.sh virtiofs` checks reads, listings, multi-chunk writes, fsync,
  rename, unlink and truncate from the guest, then the result from the host.
  Freeing a file's handle keeps its metadata, because a regular file's tuple
  has no parent link that `fs_cleanup_dir_entry` could detach. `9p.c` has the
  same free path, which by reading would leave the parent's entry pointing at
  freed memory; that is untested.
- **Result protocol and storage.** Fixtures print
  `MINI_CHROMIUM_TEST name=<n> status=PASS|FAIL [detail=…]` and one
  `MINI_CHROMIUM_RESULT status=… passed=N failed=M`. Each run writes
  `.work/runs/<utc>-<id>/{serial.log,result.json}`. `result.json` holds the
  hydrogen SHA and dirty flag, the kernel and image sha256, the fixture binary
  sha256, the toolchain versions, the QEMU command line, the elapsed time and
  the parsed records, and a `reproduce` field: one command that rebuilds and
  reruns the exact case from a clean clone at that SHA. Failing runs are kept. Passing runs older than the last
  20 may be pruned.
- **Address-space design bounds for G1.** Each process gets its own top-level
  page table. Kernel mappings are shared by pre-populating every kernel-half
  PML4 entry at boot, so later kernel mappings show up in every address space
  without copying. CR3 switches on context switch only when the address space
  changes. TLB shootdown stays broadcast through G1 (correct without PCID,
  only slower; performance is waived); G1.1 maintains the set of CPUs that
  loaded each address space, and targeted shootdown plus PCID wait for `P.1`.
  `nanos/docs/chromium-compat.md` is the binding design for G1 (verdict:
  feasible); its §6 is the G1.1 commit plan and its §3 lists every
  foreign-context user access G1.1 must remove. Until G1.2, `fork` returns
  `ENOSYS` for parents with custom or `MAP_SHARED` anonymous mappings and is
  x86_64-only. aarch64 and riscv64 must keep compiling in principle, but no
  cross compiler exists here, so they go unverified (recorded, not gating).
  G1.1 may touch every file listed in the design note's owner-decision 1,
  including a `KERNEL_TEST_CFLAGS` hook in `nanos/rules.mk` for the
  `-DVM_ISOLATION_SELFTEST_BREAK` build. If Nanos places kernel
  data in the lower half or depends on one global user `vmap` set, the G1.1
  design note gives the dependency graph and the smallest relocation, and the
  slice carries it out.
- **The futex key is not the virtual address.** Private futexes key on
  (address space, uaddr). Shared futexes key on (backing object, offset), so
  cross-process `MAP_SHARED` waits and wakes meet. Any other kernel table
  keyed by a user virtual address gets the same treatment.
- **The `prctl` contract changes.** Unknown options return `-EINVAL`, as on
  Linux, instead of the current blanket 0 (`nanos/src/unix/syscall.c:2304`).
  `setuid`, `setgid`, `setgroups` and `capset` stop being `syscall_ignore`
  (`nanos/src/unix/syscall.c:2460-2464`), and so does `wait4`
  (`nanos/src/x86_64/unix_machine.c:260`). Existing Nanos programs that relied
  on the silent success will now see real results. That is intended; list it
  in the final report.
- **The sandbox layer is the namespace sandbox** (confirmed by H,
  `docs/chromium-contract.md` §2). Helium selects it whenever unprivileged
  user namespaces work; the setuid fallback cannot work in this package
  anyway (it ships `chrome_sandbox`, the browser looks for `chrome-sandbox`).
  Without user namespaces it dies with "No usable sandbox!". The setuid path
  and file mode bits as security state are non-goals.
- **Contract-driven scope** (`docs/chromium-contract.md` §7 is the
  checklist for G1–G3): `SOCK_SEQPACKET` with `SCM_CREDENTIALS` whose pid is
  translated into the receiver's PID namespace is required (G2.1); seccomp
  actions follow Linux: install accepts any return action, and at run time
  `ALLOW`, `ERRNO`, `TRAP`, `KILL_THREAD` and `KILL_PROCESS` behave as on
  Linux, `LOG` allows, `TRACE` with no tracer fails the syscall with
  `ENOSYS`; only the `NEW_LISTENER` (`USER_NOTIF`) flag and unknown flags give
  `EINVAL`; `SPEC_ALLOW` is accepted and `TSYNC` must really synchronize all
  threads, because Helium's GPU process installs with it (executor decision
  2026-11-09, from `nanos/docs/sandbox-design.md` findings) (G3.2); user namespaces nest two deep (G3.3);
  `ptrace`, `setns` and `mount` stay `ENOSYS`, but `PR_SET_PTRACER` must
  succeed (G3.1/G3.4); `memfd_create("", ~0)` and `eventfd2(0, ~0)` must fail
  with `EINVAL`; `/proc/self/task` needs `st_nlink == threads + 2`.

- **G2/G3 binding design:** `nanos/docs/sandbox-design.md` (verdict
  feasible). Its §5 slice table supersedes the G2 and G3 rows below for
  owned files, probe checks turned green and runtime tests; its findings
  correct this doc where they disagree (check placement: `proc_status` and
  `ptrace_denied` at G3.2, every zygote check at G3.4; `/proc` is not
  filtered by PID namespace; abstract `AF_UNIX` names are out of scope).
- **Parallel lanes** (owner preference 2026-11-09: fan out once designs are
  solid): run concurrent writer lanes whenever their owned files in the
  design tables are disjoint, plus shared headers edited in distinct
  sections. Integrate one lane at a time on `main`, each with its
  acceptance plus `dev/test.sh harness probe upstream`. Lanes never push.
- **Init exit ends the VM** (executor, 2026-11-09): a child's exit or fatal
  signal tears down only that process; the VM exits with init's code when
  init exits, as PID 1 does on Linux.

- **Independent differential corpus** (owner-supplied prior-art handoff,
  2026-11-09). Beside the bespoke probe, the campaign keeps a small bank of
  externally authored cases in `tests/external/`, each compiled once with the
  musl guest toolchain and run natively and on Nanos with the same records.
  gVisor's syscall tests (Apache-2.0, designed to also pass on native Linux)
  are ported case by case to C with the upstream notice, pinned commit,
  file and test name kept per case. Linux selftests (GPL-2.0) and Fuchsia
  Starnix are behavior references only: write independent tests, copy no
  code. Start with the 12–20 cases matching the slices in flight (fd
  ownership and SCM_RIGHTS lifetime now, seccomp/TSYNC with G3.2, including
  Starnix's `ErrnoIsMaxFFF` errno cap); grow only when a case exposes a real
  gap. A case passes only when every observation and its cleanup hold on
  both sides; a case that cannot run is recorded as such, never as a pass.

### Delegated (decide within the bound, record in the commit body)

- **Reproducible build check (G0.1).** Two clean builds of the same commit at
  the same absolute path must produce a byte-identical `kernel.img`. If they
  differ, find the nondeterministic input (embedded time, path, ordering) and
  remove it with at most two changes. If one remains, document it in
  `toolchain.lock` and compare the `.text`/`.rodata`/`.data` section hashes
  instead.
- **Upstream tests that won't build on musl.** Fix the test source to be
  portable (header conflicts such as `statx_timestamp` in `filesystem.c`, or
  `struct timespec` includes in `aio.c`), or point dynamic manifests at the
  musl loader. Never weaken an assertion. A test that cannot be made portable
  in 30 minutes is recorded as `NOT RUN (musl-incompatible: <reason>)` in
  **Baseline**.
- **Pre-existing upstream failures** that G0.4 finds: fix them if they meet
  the egregious threshold or block a later slice. Otherwise record them in
  **Baseline** and leave them.
- **Mechanism details inside each gate** (data-structure layout, lock
  choice, error-path order): use the smallest change that passes the slice's
  tests and keeps `nanos/docs/chromium-compat.md` accurate.
- **Contract surprises.** If the Helium tree needs a syscall or flag not
  listed in `docs/chromium-contract.md`, add it there with its `file:line`
  before implementing it. If it needs something this doc forbids (graphics,
  setuid binaries, `--no-sandbox`), park the slice.

## Baseline

Measured on 2026-11-09 on this host: Alpine edge (3.25.0_alpha20260805),
Linux 7.2.9, 32 CPUs, 60 GB, `/dev/kvm` usable by `josh`, QEMU 11.1.2,
GCC 15.2.0, NASM, GDB, `strace`, Docker. There is no Go, no `ops`, no glibc.
Upstream checkout in `.work/src/nanos` at `e4b9c9e`.

| Fact | Observation |
| --- | --- |
| Clean `make -j32 kernel` | 19.7 s wall; `kernel.img` 1.79 MB, `kernel.elf` 12.7 MB (with symbols) |
| Incremental build (touch `src/unix/syscall.c`) | 5.5 s (`dev/build.sh`: 6.4 s) |
| Fresh clone: `dev/bootstrap.sh && dev/doctor.sh && dev/build.sh` | 59 s, including the LLVM, Go and musl downloads and the musl sysroot build |
| Static musl hello image | 15.3 MB; boot to exit under KVM 0.23–0.65 s (1 and 2 vCPUs) |
| Upstream `make run` | exits 0 when QEMU fails to start (port 8080 busy) |
| Upstream test manifests | dynamic ones hard-code glibc paths (`/lib64/ld-linux-x86-64.so.2`) and fail at `mkfs` |
| **Known red: console `writev`** | writes only the last iovec and returns its length: `writev(1, {"WV1 ","WV2\n"})` prints `WV2`, returns 4. musl stdio flushes through `writev(buffer, new)`; when the last iovec is empty the call completes with 0 and musl retries forever, so any program that prints through stdio and exits through `exit()` hangs (`dev/test.sh harness` marks `hello_stdio`, `hello_pthread` known-red). Fixed in G0.3 |
| `ioctl(1, TIOCGWINSZ)` | `ENOSYS`. musl treats it as "not a tty"; harmless |
| `kernel.mk:117` | `gitversion.c` depends on `$(ROOTDIR)/.git/index`; breaks in worktrees and under `hydrogen/nanos/`. Fixed in G0.1 |
| Pinned LLVM building the kernel | fails at lld (`R_X86_64_PC32` against `IMAGE_BASE`) |
| Upstream runtime suite (`dev/test.sh upstream`, musl, after G0.3 and the identity-window fix) | 78 runs (39 tests × 1 and 2 vCPUs): 63 pass, 15 known-red, pinned to causes in `dev/test.sh` `upstream_known`: `filesystem` (no `faccessat2`), `io_uring` (ring `mmap` `EINVAL`), `mmap` (user write fault in `memset`), `netsock` (crash reading the `sendmsg` msghdr), `sigloop` (`SIG_DFL`/`SIG_IGN` without `SA_RESTORER` rejected), `signal` (silent hang), `tlbshootdown` smp 2 (`FUTEX_REQUEUE` missing), `tun` (`TUNGETIFF`). Runs in about 2.5 min |
| **Fixed: stale boot identity window** | PVH boot identity-maps `[0, 4M)` with 2 MiB supervisor pages and reclaim left `[0xa000, 2M)` and `[END, 4M)` mapped in the user half: intermittent `SEGV_ACCERR` in musl malloc whenever heap ASLR put `brk` below 4M, and a kernel panic from user `mmap(MAP_FIXED)` + `munmap` there. `low_user_va` fixture |
| Sandbox syscalls at `e4b9c9e` | `clone` without `CLONE_THREAD` returns `-ENOSYS` (`nanos/src/unix/thread.c:58`); `execve`/`fork`/`seccomp`/`unshare` unregistered; `wait4`, `setuid`, `setgid`, `setgroups`, `capset` are `syscall_ignore`; `prctl` returns 0 for every option it doesn't know; no `/proc/self/status` |
| Existing reusable pieces | `memfd_create` and seals (`nanos/klib/shmem.c:12`, `F_ADD_SEALS` in `syscall.c`), `pledge`/`unveil`/`sandbox` klibs (`nanos/klib/`), the `strace` klib, `NO_KASLR` (`nanos/src/kernel/init.c:86`) |
| Native oracle | unprivileged `unshare -Urpn` works on the host; seccomp present |
| Metrics (`dev/test.sh metrics`) | `kernel.img` 1,789,960 B; `kernel.elf` 12,714,736 B; static hello image 15,256,576 B; guest with 1 GiB after boot: MemTotal 1,044,792 KiB, MemFree 982,408 KiB (≈61 MiB used); boot to exit of static hello median 0.24–0.29 s over 10 boots (min 0.22, max 0.50) |
| Debug kernel | `dev/build.sh --debug` builds from a synced copy at `.work/build/nanos-debug` with `KCONFIG=no-kaslr`; it stays `-O3` because `nanos/kernel.mk:25` hard-codes it, and changing that is a kernel-build change. Overriding `OUTDIR` breaks the vdso headers, hence the copy. GDB loads `kernel.elf` at `KERNEL_BASE - START` (`0xffffffff7fe00000`), verified against `kas_kern_offset`. Kernel-frame symbolization (`addr - kernel load offset`) is implemented but not yet exercised by a kernel fault |

Loop: a kernel slice's inner loop is an incremental build (~6 s) plus
one or more guest boots (~1 s each). The full upstream runtime suite is
expected to take several minutes once hangs become failures under the
harness's deadline. Run it only at the milestones named in **Sequence**.

## Sequence

Items run in order. "Owns" lists the files a slice may change. Two slices
share a file only when they are serial. The acceptance command runs on
`main` after integration, followed by the narrowest affected suite.

**Parallel pairs:** `H` may run in a second lane alongside any G0 or G1 slice
(it owns only `docs/chromium-contract.md` and `.work/`). No other pair runs in
parallel: every kernel slice touches `nanos/src/unix/`.

**Milestone suites** (`dev/test.sh upstream`, the full musl runtime suite
under the harness on 1 and 2 vCPUs) run at the end of G0, G1, G2 and G3, and
at the close. A milestone with a regression against **Baseline** blocks the
next gate.

### G0: development environment (P0, ground)

| ID | Slice | Owns | Prereq | Acceptance |
| --- | --- | --- | --- | --- |
| G0.1 | Import the fork at `e4b9c9e` as described in **Decisions**. Pin vendored deps. Make `gitversion.c` work when `nanos/.git` is absent (ask `git -C $(ROOTDIR) rev-parse --git-dir`, and fall back to a fixed string when outside git). Write `dev/toolchain.lock`, `dev/bootstrap.sh` (fetch and verify the LLVM tarball, build the musl 1.2.6 sysroot, vendor deps at their pinned SHAs, fetch Go), `dev/doctor.sh` (each missing prerequisite on its own line, with the command to fix it; checks KVM by booting a 1-instruction guest with `-enable-kvm`), and `dev/build.sh` (wraps `make -C nanos kernel` incrementally, keeps `kernel.elf` unstripped). | `nanos/Makefile`, `nanos/rules.mk`, `nanos/kernel.mk`, `dev/{bootstrap,doctor,build}.sh`, `dev/toolchain.lock` (ignore `.work/` through `.git/info/exclude`, not a tracked file) | — | From a fresh `git clone` of hydrogen into `.work/wt/clean`: `dev/bootstrap.sh && dev/doctor.sh && dev/build.sh` succeed; a second clean build gives an identical `kernel.img` (or identical section hashes, per **Decisions**); `doctor.sh` with `PATH` stripped of `nasm` names `nasm` and exits non-zero |
| G0.2 | `dev/image.sh <binary> [--lib …] [--file …] [--arg …] [--env …] [--trace]` builds the manifest, resolves the musl `DT_NEEDED` closure from `.work/sysroot` or a given root, and calls the fork's `mkfs` with the source-built kernel and boot image. It prints the image's sha256. `dev/run.sh <image> --expect <spec> [--smp N] [--timeout S]` behaves as described in **Decisions**. Fixtures in `tests/mini_chromium/fixtures/`: static hello, dynamic hello, pthread hello, an expected-crash fixture. | `dev/{image,run}.sh`, `tests/mini_chromium/fixtures/`, `tests/mini_chromium/Makefile` | G0.1 | `dev/test.sh harness`: static hello passes; the crash fixture is reported as `CRASH`; a fixture that exits 0 without its terminal record is reported as `FAIL missing-record`; a fixture that sleeps forever is reported as `TIMEOUT` within its deadline; a fixture printing the terminal record twice is reported as `FAIL duplicate-record`; dynamic hello **fails** (the `writev` bug) |
| G0.3 | Fix console `writev` (and any other `file_iov` path with the same last-iovec bug) in `nanos/src/unix/`. Add `nanos/test/runtime/writev_console.c` with multi-iovec, zero-length-first and partial cases. | `nanos/src/unix/` (the console or tty write path), `nanos/test/runtime/` | G0.2 | `dev/test.sh harness` all green, including dynamic and pthread hello printing via `printf`; `dev/test.sh runtime writev_console writev` passes |
| G0.4 | Port the upstream runtime suite to musl and the harness. `dev/test.sh upstream` builds every `RUNTIME_TESTS` entry with the guest toolchain (static or musl-dynamic), runs each with its upstream expected exit code on 1 and 2 vCPUs, and prints a PASS/FAIL/TIMEOUT/NOT RUN table. Write that table into **Baseline**, replacing the partial row. Include the unit tests (`make -C nanos/test/unit`) if they build on musl. | `nanos/test/runtime/` (portability only), `nanos/test/unit/` (portability only), `dev/test.sh` | G0.3 | `dev/test.sh upstream` finishes in under 15 min, and its table matches **Baseline** |
| G0.5 | `dev/debug.sh <image>` builds a debug kernel (`NO_KASLR`, `-O0` or `-Og`, symbols), boots with `-s -S`, and prints the GDB attach command. `dev/debug.sh --check` drives GDB in batch mode to a kernel breakpoint (`kernel_runtime_init` or the syscall entry) and prints a backtrace. Symbolization: `run.sh` passes kernel `*** signal`/fault frame addresses through `addr2line -e kernel.elf`. Syscall tracing: `image.sh --trace` turns on Nanos's existing `trace`/`debugsyscalls` manifest keys. Baseline metrics: boot latency (median of 10 cold boots of static hello), kernel and image size, guest memory after boot (Nanos's `/proc/meminfo` read by a fixture). Write them to **Baseline**. | `dev/debug.sh`, `dev/run.sh` | G0.4 | `dev/debug.sh --check` prints a frame inside the named kernel function; `dev/test.sh harness` shows a symbolized frame (function name and file) for the crash fixture; the metrics row exists |

**G0 passes** when a fresh clone runs `dev/bootstrap.sh && dev/test.sh g0`
(harness, upstream, debug check) green without any opaque kernel binary.
Run the milestone suite.

### H: Chromium contract (independent lane)

| ID | Slice | Owns | Prereq | Acceptance |
| --- | --- | --- | --- | --- |
| H | Prepare the patched Helium tree (owner decision 4). Copy the read-only subtrees into `.work/src/helium/`: `sandbox/linux`, `sandbox/policy/linux`, `content/zygote`, `content/common/zygote`, `content/browser/zygote_host`, `base/process`, `base/posix`, `ipc`, `mojo/core` (platform channel), `base/memory` (shared memory), `third_party/crashpad/crashpad/util/linux`. Write `docs/chromium-contract.md`: for each process role (browser, zygote, renderer), every syscall, flag, `prctl`/`seccomp` op, procfs path, socket type and cmsg Chromium uses, each with its `file:line` in the Helium tree; the renderer's seccomp policy (actions used, whether TSYNC is used, the SIGSYS trap handler contract); the namespace sandbox sequence (`clone` flags, `uid_map`/`gid_map`/`setgroups`, the chroot to `/proc/self/fdinfo` through `CLONE_FS`, capability drops); and how Chromium chooses between the namespace and setuid layers. Cross-check by running the Helium artifact natively on this host: `strace -f -e trace=process,%ipc,%network,prctl,seccomp,unshare,clone,clone3` with `--headless=new` and the sandbox on, saving the trace in `.work/`. | `docs/chromium-contract.md` | — | Every row cites a `file:line` in the Helium tree, or is marked `observed-only` with a strace line; the native strace shows the renderer reaching `Seccomp: 2`; the doc states the layer-1 sandbox choice and whether the namespace decision holds |

### G1: multiprocess architecture (decisive gate)

| ID | Slice | Owns | Prereq | Acceptance |
| --- | --- | --- | --- | --- |
| G1.1 | Archaeology and the first architectural fix. Write `nanos/docs/chromium-compat.md`: how CR3 is set (`nanos/src/x86_64/page.c:115-125`), where user and kernel mappings live, what `process`/`thread`/`vmap` own, page-fault flow, scheduler transitions, and every global that assumes one process, as a dependency graph with the smallest redesign. Then implement per-process page tables (**Decisions**) and a minimal `fork` (`clone(SIGCHLD)` without `CLONE_VM`) that eagerly copies private mappings. Child exit tears down its address space. Add `nanos/test/runtime/vm_isolation.c`: parent and child map the same address, write different data, yield to each other for 10,000 rounds and check their own data; each tries to write the other's private data through every path it has (same VA, `/proc/self/mem` if present, `process_vm_writev` → expect `ENOSYS`/`EPERM`); reading and writing a kernel address faults with SIGSEGV. | `nanos/src/x86_64/page*.{c,h}`, `nanos/src/kernel/page*.{c,h}`, `nanos/src/unix/{unix.c,unix_internal.h,thread.c,mmap.c}`, scheduler files, `nanos/docs/chromium-compat.md`, `nanos/test/runtime/vm_isolation.*` | G0 | `dev/test.sh runtime vm_isolation --smp 1` and `--smp 2` pass 20 consecutive runs each; the same binary passes natively; a deliberately broken build (child shares the parent's CR3, built with `-DVM_ISOLATION_SELFTEST_BREAK`, never committed enabled) makes the test fail. This is how you check the test can detect sharing |
| G1.2 | Copy-on-write `fork`, `vfork` and `clone(CLONE_VM\|CLONE_VFORK)` (musl's `posix_spawn` uses it), `wait4`/`waitid` with exit status, signal status and `WNOHANG`, real PIDs and PPIDs, zombies, reparenting to PID 1, `getppid`, and process cleanup. Fd tables are copied on fork and shared under `CLONE_FILES`. Add `nanos/test/runtime/process_lifecycle.c` with positive, negative (`ECHILD`, `EINVAL` flags) and failure-path (`ENOMEM`/`EAGAIN` from a process-count limit) cases. | as G1.1 plus `nanos/src/unix/{syscall.c,signal.c}`, `nanos/src/x86_64/unix_machine.c`, `nanos/test/runtime/process_lifecycle.*` | G1.1 | `dev/test.sh runtime process_lifecycle vm_isolation` on 1 and 2 vCPUs; a COW counter shows pages are shared until the first write (child RSS from `/proc/self/status` `VmRSS` grows only after writing) |
| G1.3 | `execve`/`execveat` through the existing ELF loader, including `PT_INTERP` (musl's `ld-musl`), argv, envp and auxv; close-on-exec; the address space is replaced while the PID and the open fds without `O_CLOEXEC` are kept; `execve` from a multithreaded process kills the other threads. Add `nanos/test/runtime/exec.c`, which execs itself in a second role and a musl-dynamic helper, with `ENOENT`/`ENOEXEC`/`EFAULT`/`E2BIG` cases. | as G1.2 plus `nanos/src/unix/exec.c`, `nanos/test/runtime/exec.*` | G1.2 | `dev/test.sh runtime exec process_lifecycle` on 1 and 2 vCPUs |
| G1.4 | Cross-process signals and lifecycle: `kill`/`tgkill` to other processes with permission checks, `SIGCHLD`, default actions killing a whole process, `exit_group` with threads in a child, exit races, and threads (`CLONE_THREAD`) inside forked children. Start `tests/mini_chromium/mini_chromium.c` with the role dispatcher, the result protocol and the process and memory checks so far. Add `tests/mini_chromium/mini_chromium_raw.c` (raw syscalls, no libc) for the same process checks. Stress: 1,000 fork/exec/wait cycles, 64 concurrent children, and process-limit exhaustion. | as G1.3, `tests/mini_chromium/` | G1.3 | `dev/test.sh probe --native` then `dev/test.sh probe` on 1, 2 and 4 vCPUs; checks `process_fork`, `process_exec`, `process_wait`, `pid_identity`, `vm_isolation`, `cow`, `signals`, `lifecycle_stress` all `PASS` on both; milestone suite with no regressions |

**G1 passes** when the probe repeatedly forks, execs, communicates over a
pipe with, and waits on, MMU-isolated children on 1, 2 and 4 vCPUs, and the
milestone suite shows no regressions. If G1.1's design note shows the kernel
cannot have per-process page tables without changing something this doc
forbids, set G1 to `BLOCKED` with the dependency graph, park G1.2 onward and
everything after G1, and finish H and the docs.

### G2: Chromium-style IPC

| ID | Slice | Owns | Prereq | Acceptance |
| --- | --- | --- | --- | --- |
| G2.1 | Inventory first (list in `nanos/docs/chromium-compat.md` what already works across processes). Then: `SCM_RIGHTS` on `AF_UNIX` `SOCK_STREAM`, and `SOCK_SEQPACKET` if `docs/chromium-contract.md` lists it, with file-description refcounts held by in-flight messages and released on close, receive or truncation (`MSG_CTRUNC`); `SO_PEERCRED`, `SO_PASSCRED`, `SCM_CREDENTIALS` with kernel-checked credentials; the crashpad `setsockopt` that returns `ENOTSUP` today (`docs/research/unikernel-nanos.md` §3 in bctl). Negative cases: malformed `cmsg_len`, bad fd numbers (`EBADF`), too many fds (`EINVAL`/`ETOOMANYREFS`), a closed peer (`EPIPE`/`ECONNRESET`), fds dropped when the receiver's fd table is full. Add `nanos/test/runtime/scm_rights.c`. | `nanos/src/unix/socket.c`, `nanos/src/unix/unix_internal.h`, `nanos/test/runtime/scm_rights.*`, `tests/mini_chromium/` | G1 | `dev/test.sh runtime scm_rights unixsocket socketpair`; the probe's `scm_rights` and `peer_credentials` checks pass natively and on Nanos |
| G2.2 | Cross-process shared memory: `memfd_create` plus `ftruncate` plus `MAP_SHARED` in two processes see the same pages; seals are enforced across processes (`F_SEAL_WRITE` denies a writable map in the receiver); private mappings stay private alongside them; shared futexes wait and wake across processes (**Decisions**); robust futex lists on exit. This is the browser→renderer proof of the draft: memfd, payload, fd sent, renderer maps it, reads, writes an allowed region, browser sees the write. | `nanos/klib/shmem.c`, `nanos/src/unix/{mmap.c,futex.c}`, `nanos/src/kernel/pagecache*`, `nanos/test/runtime/shmem_xproc.*`, `tests/mini_chromium/` | G2.1 | `dev/test.sh runtime shmem_xproc shmem futex futexrobust` on 1 and 2 vCPUs; the probe's `shared_memory_fd`, `seal_enforcement`, `private_stays_private` and `futex_xproc` checks pass on both |
| G2.3 | `eventfd`, `epoll`, pipes and poll across processes: readiness from another process's write, `EPOLLHUP`/`EPOLLRDHUP` on peer exit, `shutdown(SHUT_WR)`, blocking and `EAGAIN`, `SIGPIPE`/`EPIPE`. | `nanos/src/unix/{poll.c,pipe.c,eventfd.c,socket.c}`, `nanos/test/runtime/`, `tests/mini_chromium/` | G2.2 | the probe's `epoll_eventfd`, `pipe_xproc` and `socket_shutdown` checks pass on both; milestone suite with no regressions |

**G2 passes** when every IPC check passes between real isolated processes.

### G3: renderer sandbox (fail-closed)

| ID | Slice | Owns | Prereq | Acceptance |
| --- | --- | --- | --- | --- |
| G3.1 | Credentials: per-process real, effective and saved UID/GID and supplementary groups, starting from the manifest `uid`/`gid` (`ab823eb4`); `setuid`, `setgid`, `setresuid`, `setresgid`, `setgroups` and `getres*` with Linux permission rules; `capget`/`capset` with a real per-process capability set (root holds all, a user holds none, a user namespace grants caps inside it); `PR_SET/GET_NO_NEW_PRIVS` as real state, inherited on fork and kept across exec; `PR_SET_DUMPABLE`/`PR_GET_DUMPABLE`; unknown `prctl` options return `EINVAL`; `/proc/self/status` and `/proc/<pid>/status` with factual `Pid`, `PPid`, `Uid`, `Gid`, `NoNewPrivs`, `Seccomp`, `Seccomp_filters` and `VmRSS`. | `nanos/src/unix/{syscall.c,special.c,unix.c,unix_internal.h}`, `nanos/test/runtime/creds.*`, `tests/mini_chromium/` | G2 | `dev/test.sh runtime creds`; the probe's `credentials`, `nnp_state` and `proc_status` checks pass natively and on Nanos, including denial cases (unprivileged `setuid(0)` gives `EPERM`; NNP cannot be cleared) |
| G3.2 | seccomp-BPF: `seccomp(SECCOMP_SET_MODE_STRICT\|SECCOMP_SET_MODE_FILTER)`, `prctl(PR_SET_SECCOMP)`, and `SECCOMP_GET_ACTION_AVAIL` if the contract uses it; a classic-BPF validator (Linux's rules: forward jumps only, in bounds, ends in `RET`, loads aligned within `seccomp_data`, at most 4,096 instructions) and an interpreter; enforced at the one syscall dispatch point before handler lookup, so unregistered syscalls are filtered too, and the vsyscall emulation path and any `int 0x80` path go through the same check (or are disabled for filtered tasks); `arch` = `AUDIT_ARCH_X86_64` and x32-bit rejection as in Chromium's policy; actions `ALLOW`, `ERRNO`, `TRAP` (SIGSYS with `si_syscall`, `si_arch` and `si_call_addr` set, as Chromium's trap handler expects), `KILL_THREAD`, `KILL_PROCESS`, plus `TRACE`/`LOG`/`USER_NOTIF` only if the contract uses them (otherwise `EINVAL`); `SECCOMP_FILTER_FLAG_TSYNC` if the contract uses it; filters stack and are inherited on fork, clone and exec; installing requires NNP or `CAP_SYS_ADMIN`; unknown flags give `EINVAL`. Reuse the `pledge` klib's dispatch hook if one exists; don't duplicate it. | `nanos/src/unix/seccomp.c` (new), `nanos/src/unix/syscall.c`, `nanos/src/x86_64/` syscall entry, `nanos/test/runtime/seccomp.*`, `tests/mini_chromium/` | G3.1 | `dev/test.sh runtime seccomp`: a corpus of invalid programs (backward jump, out-of-bounds jump, no `RET`, misaligned load, too long) each gets `EINVAL`; a denied syscall gets the expected errno, SIGSYS or kill; allowed syscalls keep working; filters survive fork and exec; installing without NNP and without the capability gives `EACCES`; the probe's `seccomp_enforcement`, `seccomp_trap`, `bpf_rejects_malformed` and `seccomp_inherit` checks pass natively and on Nanos |
| G3.3 | User namespaces: `clone(CLONE_NEWUSER)` and `unshare(CLONE_NEWUSER)`, one-time writes to `/proc/<pid>/uid_map` and `gid_map`, `/proc/<pid>/setgroups` `deny`, capabilities scoped to the namespace, credentials mapped through it, `CLONE_FS` and `chroot` with a per-process root and cwd (for Chromium's chroot to `/proc/self/fdinfo`), `setns` only if the contract uses it. | `nanos/src/unix/` (namespace and fs-root code), `nanos/test/runtime/userns.*`, `tests/mini_chromium/` | G3.2 | `dev/test.sh runtime userns`; the probe's `userns_map`, `userns_caps_scoped` and `fs_restricted` checks pass natively and on Nanos: after the chroot the renderer cannot open any path outside it; a second write to `uid_map` gives `EPERM` |
| G3.4 | PID and network namespaces: `CLONE_NEWPID` (the first child is PID 1 inside; `getpid` and `/proc` show only its namespace; `kill` and `ptrace` cannot reach outside processes; PID 1's death kills the namespace) and `CLONE_NEWNET` (a new namespace has no interfaces except a down loopback, so `connect` to any IP destination fails with `ENETUNREACH`; abstract `AF_UNIX` names are per netns; filesystem `AF_UNIX` sockets still work). `ptrace` stays `ENOSYS` unless the contract needs it; if it does, implement only `PTRACE_TRACEME`/`ATTACH` permission denial semantics and the ops the contract uses, under the Yama-like rule Chromium assumes. | `nanos/src/unix/` (pid and net namespace code), `nanos/src/net/` (netns gate only), `nanos/test/runtime/pidns_netns.*`, `tests/mini_chromium/` | G3.3 | `dev/test.sh runtime pidns_netns`; the probe's `pidns_isolation`, `netns_denied`, `ptrace_denied` and `kill_outside_denied` checks pass natively and on Nanos; milestone suite with no regressions |

**G3 passes** when every negative check in the probe holds on Nanos.
`NoNewPrivs: 1` and `Seccomp: 2` in `/proc/<pid>/status` count only as
corroboration, never as proof.

### G4: the miniature Chromium

| ID | Slice | Owns | Prereq | Acceptance |
| --- | --- | --- | --- | --- |
| G4.1 | Complete `mini_chromium`. `--role=browser` creates the memfd, the socketpairs and the eventfd, forks and execs `--role=zygote`, sends work, collects results and runs its own checks. `--role=zygote` performs the namespace setup in the order the Helium tree does (from `docs/chromium-contract.md`), then forks and execs `--role=renderer`. `--role=renderer` installs NNP and a seccomp policy shaped like the renderer's (the same actions, a TRAP handler, an errno-denied `socket(AF_INET)`), receives the memfd, does the work and runs every negative check: reading browser-private memory, `ptrace` of the browser, `kill` outside its pid namespace, a network connect, opening a path outside the chroot, a forbidden syscall, a malformed filter. Cover the full table in **Acceptance**. The harness rejects missing records, duplicate terminal records, crashes, timeouts and unexpected skips, and writes `result.json` (**Decisions**). | `tests/mini_chromium/`, `dev/test.sh` | G3 | `dev/test.sh probe --native` and `dev/test.sh probe` (1, 2, 4 vCPUs) report `MINI_CHROMIUM_RESULT status=PASS failed=0` with every required check named; `mini_chromium_raw` passes its process, memory and seccomp subset on both |
| G4.2 | Repetition and stress: `dev/test.sh probe --repeat 50` (50 cold boots, all must pass) and `--stress` (8 concurrent renderers × 100 launch/exit cycles in one boot, then a check that the process count and the guest's free memory return to their starting values). | `dev/test.sh`, `tests/mini_chromium/` | G4.1 | both commands pass; record the wall time and per-boot median in `docs/campaign-report.md` |

### G5: the prebuilt Helium binary

| ID | Slice | Owns | Prereq | Acceptance |
| --- | --- | --- | --- | --- |
| G5.1 | Package `~/d/chromium-build/.work/artifacts/helium-0.20.3.1-r5/helium-0.20.3.1-r5-linux-x86_64-musl.tar.xz` (verify sha256 `f4b06e80a097587e508982a3c73f043dbd80e3660aab6d47f72648b6ffc36a05`; copy it, never modify it in place) with its musl runtime from Alpine 3.24.2 (pinned `.apk` files under `.work/dl/`). `tests/helium/guest_init.c` is the Nanos program: it starts Helium with `--headless=new --remote-debugging-port=9222 --user-data-dir=/profile` and the sandbox on (no `--no-sandbox`, `--single-process` or `--no-zygote`), then reports each process's role from `/proc/<pid>/cmdline` and its `/proc/<pid>/status`. | `tests/helium/`, `dev/image.sh` (rootfs input) | G4 | `dev/test.sh helium boot` reaches a live browser process plus separate zygote and renderer processes on serial, or stops with the earliest causal failure written in the slice's commit body |
| G5.2+ | One slice per causal failure Helium hits, with the root cause found by tracing (`--trace`, guest `strace` klib, Helium's `--enable-logging=stderr --v=1`): the smallest kernel fix plus a standalone regression test in `nanos/test/runtime/`. Never patch Helium to hide unsupported kernel semantics. | the kernel area at fault, `nanos/test/runtime/` | G5.1 | that test, plus `dev/test.sh probe` and `dev/test.sh helium boot` progressing past the failure |
| G5.N | Full validation over CDP from the host (QEMU user network, dynamically chosen forwarded port): `/json/version`; navigate to an HTTP fixture served by the host harness; `Runtime.evaluate` returns the fixture's computed value; `DOM.getDocument` contains its marker; `guest_init` reports `NoNewPrivs: 1` and `Seccomp: 2` for every renderer; a sandbox confinement check, where the fixture page triggers a renderer-side operation the policy forbids and the browser keeps running while the denied action does not occur; `Browser.close`, and then `guest_init` sees every child reaped and guest free memory back to within 5% of its pre-launch value before it exits with code 0. | `tests/helium/` | G5.1… | `dev/test.sh helium-stress` passes (10 parallel cold boots) |

### P: optimization and polish (only after G5, or after G4 if G5 is parked)

| ID | Slice | Acceptance |
| --- | --- | --- |
| P.1 | Benchmark report in `docs/campaign-report.md`: cold-boot latency (median and p95 over 20 boots), kernel and image size, guest RSS (idle, probe, Helium), `fork`+`exec`+`wait` latency, and Unix-socket round trip and fd-passing throughput, each against **Baseline** and the native host, with method and spread. Measurements only; nothing gates on them (owner decision 6). | the report table exists |

### Close

| ID | Slice | Acceptance |
| --- | --- | --- |
| C.1 | Dead code: remove harness paths, fixtures and kernel code that no gate test reaches (`dead-code-archaeology`: prove liveness with `rg` for callers plus a passing `dev/test.sh all` after removal). Delete `-DVM_ISOLATION_SELFTEST_BREAK` only if a cheaper detection proof replaces it. | `dev/test.sh all` green |
| C.2 | Run `ultrareview` over the campaign diff (`git diff e4b9c9e..HEAD -- nanos` and all of hydrogen's own files). Fix every P1 and P2 finding with a test. | `dev/test.sh all` green after the fixes |
| C.3 | Write `docs/campaign-report.md` (architecture, implementation, measured evidence, gate matrix with revisions, unresolved failures, the next minimal step toward full Helium qualification, the contract changes list). Also update `nanos/docs/chromium-compat.md`. Rewrite **Status** to its final state. | the report exists, cites run IDs from `.work/runs/` by their hydrogen SHAs, and its gate matrix matches **Status** |

## Acceptance

`dev/test.sh all` = `g0` + `upstream` + every new `runtime` test + `probe
--native` + `probe` (1, 2, 4 vCPUs) + `probe --repeat 50` + `probe --stress`
+ `helium`. A run counts only with the guest-side records; QEMU's or the
launcher's exit status alone never counts.

| Deliverable | Proof |
| --- | --- |
| 1. Source development environment | fresh clone → `dev/bootstrap.sh && dev/doctor.sh && dev/test.sh g0` green; doctor names each missing tool |
| 2. Fork with verified kernel changes | `nanos/` on `main`; each kernel change has its `nanos/test/runtime/` test in `dev/test.sh all` |
| 3. KVM boot, image and GDB workflow | `dev/test.sh harness`, `dev/debug.sh --check` |
| 4. `mini_chromium` | `tests/mini_chromium/` builds the same source natively and for the guest, plus the raw variant |
| 5. Native reference and guest runs | `dev/test.sh probe --native` and `dev/test.sh probe` with identical check sets |
| 6. Negative isolation tests | the probe's Isolation and Sandbox rows below, each `PASS` on Nanos and natively |
| 7. Upstream regression results | `dev/test.sh upstream` table in **Baseline** with no regression at close |
| 8. Gate report and revisions | **Status** gate matrix; `docs/campaign-report.md` with hydrogen SHAs |
| 9. Benchmark report | `P.1` |
| 10. Campaign report | `docs/campaign-report.md` (`C.3`) |

The probe's required checks: each row must have at least one named `PASS`
record, on Nanos and natively.

| Category | Proof |
| --- | --- |
| Processes | `clone`/`fork`, `vfork`/`posix_spawn`, `execve` with an interpreter, `wait4` status, PID and PPID identity |
| Memory | per-process address spaces, COW, cross-process `MAP_SHARED` memfd, `mprotect` denial (a write to a `PROT_READ` page gives SIGSEGV) |
| IPC | Unix stream (and seqpacket if contracted), `SCM_RIGHTS` with refcount cleanup, `SO_PEERCRED`/`SCM_CREDENTIALS` |
| Synchronization | threads in child processes, cross-process futex, epoll plus eventfd across processes |
| Sandbox | user, pid and net namespaces, uid/gid maps, credentials, NNP, seccomp filter install, inheritance and actions |
| Isolation | reading or writing browser memory fails, `ptrace` denied, `kill` outside the namespace denied, network denied, paths outside the chroot denied, a forbidden syscall denied, malformed BPF rejected, a kernel address faults |
| Lifecycle | exit status, signals, zombie reaping, repeated launch, resources back to their starting level |

## Needs environment

None.

## Execution boundary and non-goals

**Authorized:** create and edit files in `~/d/hydrogen` (tracked files and
`.work/`); commit there and push `main` to `origin` after each accepted slice;
add and fetch the `upstream` remote; download pinned public sources and
release artifacts into `.work/dl/` (verify the sha256 on first fetch and
record it in `toolchain.lock`); run QEMU/KVM guests; run Docker only for
owner decision 4; read `~/d/bctl`, `~/d/chromium-build` and
`~/d/laputa-systems/llvm-prebuilt-musl`; run the Helium artifact natively on
this host for the H cross-check.

**Forbidden:** editing tracked files
in `~/d/chromium-build` or `~/d/laputa-systems`; chromium-build `build` or
any step after `prepare`; building Chromium or Helium from source; pushing to
any target other than hydrogen `origin main`; force pushes; creating remote
repositories; `sudo` (host `apk` installs only as owner decision 9 allows); `/tmp` and `$TMPDIR` outside `.work/tmp`;
`--no-sandbox`, `--single-process` or `--no-zygote` in any qualifying run;
fake sandbox state or syscalls that report success without effect; Nix,
systemd, Ansible or other heavy orchestration; `ops` (`mkfs` from the fork
builds images); glibc in any form.

**Non-goals:** kernel or device graphics: DRM/KMS, evdev, seatd and virtio-gpu/input and any Wayland compositor;
aarch64, macOS and HVF (lifted by the owner on 2026-11-10 for after this
campaign: bctl's roadmap M7M plans Nanos on Apple Silicon and the arm64 port it
needs here); the setuid `chrome-sandbox` layer and filesystem
mode-bit security; general process management beyond what the probe and
Helium require; a kernel build with LLVM; PCID and other performance work
before `P.1`.

**Contract changes to report to the owner** (in `docs/campaign-report.md`):
`prctl` unknown options now return `EINVAL`; `setuid`, `setgid`,
`setgroups`, `capset` and `wait4` stop being silent no-ops; `clone` without
`CLONE_THREAD`, plus `fork`, `vfork`, `execve`, `seccomp` and `unshare`, go
from `ENOSYS` to implemented; `/proc/<pid>/status` and the namespace procfs
files are new; vendored dependency fetching now checks out exact commits;
the Nanos `make run` exit-code behaviour is unchanged, but hydrogen never
relies on it.

## Reference

- Required reading for the executor, read-only: `~/d/bctl/docs/research/unikernel-nanos.md`
  (the 0.1.56 gap analysis; its aarch64/macOS details do not apply here),
  `~/d/bctl/docs/research/unikernel-unikraft.md`,
  `~/d/bctl/docs/CAMPAIGN-ROADMAP.md`, `~/d/bctl/docs/image-contract.md`
  (its renderer `NoNewPrivs: 1` / `Seccomp: 2` definition of "sandbox ok"),
  and upstream's `nanos/README.md` and `nanos/CHARTER.md`.
- Pinned sources: Nanos `e4b9c9e65cd51ef29e5021835419a1d9a7733f20`; Chromium
  tag `156.0.8078.79` = `a21ce95c24ce3964416f09ae1862f53f319eb4d0`; Helium core
  `2f81b9c…`, helium-linux `83027a1…` (`~/d/chromium-build/config/helium.inputs.lock`);
  chromium-build's patches that touch the sandbox:
  `config/patches/musl-seccomp-clone-detached.patch`,
  `musl-seccomp-membarrier.patch`, `musl-sandbox-cmsg-sign-compare.patch`,
  `musl-gpu-sandbox-broker-tsync.patch`, `musl-gpu-pwritev2-sandbox.patch`,
  `musl-gpu-sched-affinity-errno.patch`, and Helium's `no-sandbox-settls.patch`
  (see `config/helium.patch-disposition.json`).
- Prior art, reference only (not in scope here): Nanos booting under
  libkrun on Apple Silicon (`github.com/tsirysndr/bsdkrun`, fork
  `tsirysndr/nanos` branch `fix/aarch64-libkrun-boot`: virtio-mmio
  enumeration, DC ZVA before MMU, EFI cache maintenance, GIC selection,
  firmware interrupt routing, level de-assert) for a later bctl
  convergence; Chimera's musl Chromium recipe (`chimera-linux/cports`
  `main/chromium`) for comparing musl patches in chromium-build (its
  `libgcc-chimera` shim is why "LLVM + musl" is not the same as no
  libgcc); waymux (`tek-cat/waymux`) was considered for the Wayland stopgap
  and not adopted, because it brings Mesa, FFmpeg and a Go viewer.
- Nanos code map at `e4b9c9e` (paths relative to `nanos/` after import):
  CR3 load `src/x86_64/page.c:115-125`; process creation refusal
  `src/unix/thread.c:58`; syscall registration `src/unix/syscall.c:2460-2465`
  and `src/x86_64/unix_machine.c:260`; `prctl` `src/unix/syscall.c:2304`;
  `writev` `src/unix/syscall.c:288` → `iov_internal` (`:140`); process struct
  `src/unix/unix_internal.h:518`; special files `src/unix/special.c`;
  `memfd_create` `klib/shmem.c:12`; KASLR `src/kernel/init.c:80-86`;
  `gitversion.c` rule `kernel.mk:117`; vendored deps `Makefile:56-58`,
  `rules.mk:168`; QEMU flags `platform/pc/Makefile:250-357`; upstream test list
  `Makefile:101-146`.
