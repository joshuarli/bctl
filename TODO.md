Prefix: BCTL
Next: 22

### BCTL-15 · P2 · A run queue can hold a null scheduler task

- **Area:** Concurrency
- **Where:** `kernel/nanos/src/kernel/schedule.c:333`, `kernel/nanos/src/kernel/schedule.c:349`
- **Problem:** Formerly HYD-3. The parallel bctl up/down loops at `e4aad9cc` crash about once per 2,300 cycles with a page fault at address `0x10` in `sched_sort`, reached from `sync_complete` → `sched_enqueue` → `pqueue_insert`: the priority queue compares a null element. Earlier captures showed the same null task through `sched_dequeue`. It survived the locked-heap management repair, so it is not that double allocation; the writer is unknown. Evidence is in `../bctl/.work/tmp/par-3d97a9b0-1/diagnostics/live-up-516.log`. `0523dbbc` moved CPU 0's run-queue backing vector and other detected post-SMP allocations to the locked heap; the next 2,400 cycles were clean, but the original corrupting write remains unproven.
- **Recommendation:** Fix through the Rust run-queue ownership slice in [the active kernel campaign](docs/kernel/rust-adoption.md#hr1-run-queue-ownership-bctl-15), after its mixed-language foundation. Retain the locked-heap repair and capture the first invalid transition; move queue storage, membership and task-lifetime leases behind one Rust owner with audited C entry points, not a raw-pointer container translation. Add deterministic growth/OOM, duplicate enqueue, dequeue/steal and exit-versus-wakeup tests, then at least 5,000 recorded parallel lifecycle cycles and the architecture gates. Clean stress runs alone do not prove the unknown writer fixed; keep that causal gap explicit and the ticket open until its closure evidence is met.
- **Fix with:** `root-cause`
- **Effort:** L

### BCTL-16 · P2 · A browser can outlive SIGKILL at guest shutdown

- **Area:** Concurrency
- **Where:** `kernel/nanos/src/unix/signal.c:349`, `kernel/nanos/src/unix/syscall.c:542`
- **Problem:** Formerly HYD-4. Twice in about 1,500 parallel bctl up/down cycles (Hydrogen `e0db91ef` and the control `86e1c394`, which lacks `5ece982a`), the guest logged that the browser ignored `Browser.close` and `SIGTERM`, then sent `SIGKILL`; the browser never exited, both CPUs idled in `kernel_sleep`, and the host's 18 s stop read timed out. A thread stuck in an uninterruptible wait, such as a file read whose completion never runs, fits the symptom; unproven, and it did not recur in 2,300 cycles with the `debug/kill-watch` probe.
- **Recommendation:** Keep the `debug/kill-watch` probe, which prints each thread's syscall and wait 3 s after a `SIGKILL`, in the long loops until it fires, then fix the wait it names with a fixture that reproduces it.
- **Fix with:** `root-cause`
- **Effort:** M

### BCTL-17 · P2 · The 9p ownership review is incomplete

- **Area:** Concurrency
- **Where:** `kernel/nanos/src/fs/9p.c`
- **Problem:** The Linux kernel review found two supported 9p ownership risks that needed runtime regressions and a repair (`docs/kernel/campaign-report.md`, "Review and acceptance limits"). The later dentry and fsfile ownership repair (`aacbd732` and the "9p ownership and mutex wakeup repair" section) may cover them, but the review's detail lived only in a scratch file that no longer exists, so that cannot be checked. Separately, a regular file's handle free in `9p.c` mirrors the virtio-fs path that kept metadata because a file tuple has no parent link; by reading, the 9p path leaves the parent's entry pointing at freed memory, and no test covers it. The suspected free-path defect is unproven at runtime.
- **Recommendation:** Rerun and commit the independent ownership review, reproduce each supported risk, then fix through the Rust 9p lifetime slice in [the active kernel campaign](docs/kernel/rust-adoption.md#hr2-9p-lifetimes-bctl-17). Move dentry, metadata/cache membership, fid and open-file/in-flight-I/O leases under an explicit Rust ownership model; retain C transport and page-cache mechanisms behind audited adapters. Cover last-close/relookup, unlink/rename with open handles, delayed completion, allocation/transport failure and exactly-once release in host tests and Linux x86_64 `sh kernel/dev/test.sh 9p`; test shared adapters on macOS ARM/virtio-fs without claiming 9p coverage there. Close only after review findings are resolved and the ownership changes and regressions are independently reviewed.
- **Fix with:** `review`
- **Effort:** L

### BCTL-18 · P2 · The Linux runtime is not qualified on the in-tree kernel

- **Area:** Release
- **Where:** `runtime/build-nanos.sh`, `tests/build/build-nanos-cache.sh`
- **Problem:** Before Hydrogen moved into `kernel/`, the Linux builder pinned Hydrogen `44f4b61f` while the macOS builder pinned `4a862120`. Both now build `HEAD:kernel`, so Linux moved forward by the ARM and deadlock fixes in between without a Linux run. The rewritten cache test has not run either.
- **Recommendation:** On a Linux x86_64 KVM host, run `tests/build/build-nanos-cache.sh`, `sh kernel/dev/test.sh g0 probe-native probe 9p vsock helium`, a `nanos-helium` image build, and `tests/interop/run.sh`, and record the result in `docs/compatibility.md`.
- **Fix with:** none
- **Effort:** S

### BCTL-21 · P2 · Nanos misses the macOS workflow targets

- **Area:** Performance
- **Where:** `docs/macos-workflows.md`
- **Problem:** Against native Chromium, Nanos is slower on every measured workflow and uses more memory on Shadowdriver and in four-tab crawls, so neither the acceptance targets (20% faster, 50% less memory) nor the stretch targets (1.5× warm agent-browser steps, 2× crawl throughput per GiB) are met.
- **Recommendation:** Cross-process wakeups and address-space switches are fixed (ASIDs and idle polling; see docs/macos-workflows.md, "Wakeups and address-space switches"). In-page compute, allocation and canvas raster match native, and native software rendering is no slower than native GPU, so the remaining 2-4x gaps on screenshots, clicks and navigation are in the browser pipeline. Next, capture a Chromium trace of one screenshot and one click step on each side with BCTL_TRACE_CHROMIUM=1 and compare where the time goes. For density, measure how much of each VM's memory is shareable file-backed pages.
- **Fix with:** `make-it-fast`
- **Effort:** L

### BCTL-10 · P3 · The web UI has no tone for paused sessions

- **Area:** Types
- **Where:** `web/src/api.ts:7`, `web/src/app.tsx:9`
- **Problem:** The daemon reports `paused` sessions (`crates/bctl/src/daemon.rs:806`), but `SessionView["state"]` omits `paused`, so the type is wrong and `stateTone` returns undefined for a paused session's badge.
- **Recommendation:** Add `paused` to the state union and a tone for it in `stateTone`, and check the badge renders for a paused session.
- **Fix with:** none
- **Effort:** S

### BCTL-19 · P3 · Resource limits are partly modeled

- **Area:** Kernel
- **Where:** `kernel/nanos/src/unix/syscall.c`
- **Problem:** `setrlimit` accepts and drops resources it does not model (`RLIMIT_AS`, `RLIMIT_CPU`, and others). `RLIMIT_DATA` bounds `brk` but not private writable mappings. `prlimit64` on another pid reads and writes the caller's limits.
- **Recommendation:** Return the Linux error for unmodeled resources unless a consumer needs silent acceptance, apply `RLIMIT_DATA` to private writable mappings, and resolve the target pid in `prlimit64`, each with a probe check run natively and on Nanos.
- **Fix with:** none
- **Effort:** M

### BCTL-20 · P3 · The kernel fork has no benchmark report or independent review

- **Area:** Kernel
- **Where:** `kernel/`, `docs/kernel/campaign-report.md`
- **Problem:** Hydrogen's campaign closed every functional gate but never ran its benchmark report (cold boot, image size, guest RSS, fork and IPC latency against native), its dead-code pass, or an independent review of the whole fork diff since upstream `e4b9c9e6`. The macOS repairs in `docs/kernel/macos-qualification.md` were never independently reviewed.
- **Recommendation:** On a quiet host, write the benchmark table into `docs/kernel/campaign-report.md`. Then run an independent review of `git diff e4b9c9e65cd51ef29e5021835419a1d9a7733f20 HEAD -- kernel/nanos` and fix P1 and P2 findings with tests.
- **Fix with:** `review`
- **Effort:** L
