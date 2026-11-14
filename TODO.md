Prefix: BCTL
Next: 24

### BCTL-23 · P2 · Experimental WebMCP bridge is not qualified on Helium runtimes

- **Area:** Browser
- **Where:** `crates/bctl/src/webmcp.rs`, `docs/webmcp.md`, `tests/interop/`, `images/helium/`
- **Problem:** The optional `bctl webmcp list/call` bridge is implemented over native page-eval with same-origin filtering and separate write/consequential opt-ins, but only expression-level unit tests are present. We have not proved that the deployed Helium/Chromium binary exposes `document.modelContext`, that origin-trial/permissions-policy conditions are met, or that `getTools`/`executeTool` operate end-to-end. WebMCP is a draft, so claiming general runtime support would be premature.
- **Recommendation:** First compare the homegrown `document.modelContext.getTools` / `executeTool` JavaScript adapter with agent-browser v0.43.0's independently implemented **CDP WebMCP** discovery and invocation. Verify the JavaScript methods actually exist in the browser's WebMCP implementation; otherwise replace the incorrect adapter with a typed CDP surface or delete it in favor of the agent-browser provider rather than maintaining two divergent protocols. Then add a purpose-built HTTPS WebMCP fixture with known `snapshot`/`query`/`act`-style tools, read-only/write/consequential annotations, callback counters, and registration/navigation tests. Opt the feature in on a test image only and qualify v0.43.0 across Nanos and SmolVM on macOS and Linux, recording build, flags, secure-context policy, and VM receipts in `docs/compatibility.md`. Prove refusals never invoke callbacks, hostile data cannot inject JavaScript, cross-origin tools stay isolated, and human takeover is respected. Keep ordinary native CDP actions as fallback; do not globally enable experimental flags or add JS dependencies.
- **Fix with:** `test-pyramid`
- **Effort:** M
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
- **Problem:** The [independent ownership review](docs/kernel/9p-ownership-review.md) at `a21c8882` identifies four source-supported risks: open-unlink/rename metadata lifetime, overlapping last-close/reopen ownership, fid-pressure metadata eviction, and fid reuse after failed clunk. Runtime regressions and the Rust lifetime repair remain outstanding. The regular-file last-close metadata free was already repaired by `aacbd732`; it is distinct from these remaining paths.
- **Recommendation:** Reproduce each supported risk and fix through the Rust 9p lifetime slice in [the active kernel campaign](docs/kernel/rust-adoption.md#hr2-9p-lifetimes-bctl-17). Move dentry, metadata/cache membership, fid and open-file/in-flight-I/O leases under an explicit Rust ownership model; retain C transport and page-cache mechanisms behind audited adapters. Cover last-close/relookup, unlink/rename with open handles, delayed completion, allocation/transport failure and exactly-once release in host tests and Linux x86_64 `sh kernel/dev/test.sh 9p`; test shared adapters on macOS ARM/virtio-fs without claiming 9p coverage there. Close only after review findings are resolved and the ownership changes and regressions are independently reviewed.
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
- **Recommendation:** Cross-process wakeups and address-space switches are fixed (ASIDs and idle polling; see docs/macos-workflows.md, "Wakeups and address-space switches"). In-page compute, allocation and canvas raster match native, and native software rendering is no slower than native GPU, so the remaining 2-4x gaps on screenshots, clicks and navigation are in the browser pipeline. Next, implement bounded operation-scoped warm Chromium traces of screenshots, clicks and navigation on native-default, native-software and Nanos: BCTL_TRACE_CHROMIUM=1 currently captures startup only and cannot attribute warm operations. Separate renderer/copy-output/readback/encoding/IPC/scheduling, then run one-variable gated A/B experiments. See docs/CAMPAIGN-GRAPHICS-PERFORMANCE.md. For density, measure how much of each VM's memory is shareable file-backed pages.
- **Fix with:** `make-it-fast`
- **Effort:** L

### BCTL-22 · P2 · Headed host display discards damage and recopies full frames

- **Area:** Performance
- **Where:** `third_party/smolvm/libkrun/src/devices/src/virtio/gpu/`, `third_party/smolvm/src/agent/display.rs`, `third_party/smolvm/src/agent/vnc.rs`
- **Problem:** The virtio-gpu resource-switch path can read whole frames; the host display callback ignores supplied damage and copies full staging to front; VNC clones and compares full images to rediscover sparse changes. Small RFB payloads do not prove efficient upstream processing. Buffer generations, partial RFB requests and slow viewers complicate correctness.
- **Recommendation:** Execute [the graphics campaign](docs/CAMPAIGN-GRAPHICS-PERFORMANCE.md) alongside upstream tinyland TINY-14 and BCTL-21. Instrument every copy/compare, preserve resource and visible-scene damage, introduce bounded immutable frame generations and cumulative viewer damage, and retain full-refresh fallbacks. Test real alternating scanout resources, partial requests, slow/reconnecting viewers, resize/restore and recovery to sparse frames with an exact-pixel oracle. Gate CPU, copied bytes, latency, memory and fullscreen regressions; only consider GPU virtualization after measured need.
- **Fix with:** `make-it-fast`
- **Effort:** L

### BCTL-20 · P3 · The kernel fork has no benchmark report or independent review

- **Area:** Kernel
- **Where:** `kernel/`, `docs/kernel/campaign-report.md`
- **Problem:** Hydrogen's campaign closed every functional gate but never ran its benchmark report (cold boot, image size, guest RSS, fork and IPC latency against native), its dead-code pass, or an independent review of the whole fork diff since upstream `e4b9c9e6`. The macOS repairs in `docs/kernel/macos-qualification.md` were never independently reviewed.
- **Recommendation:** The [partial independent review](docs/kernel/fork-review.md) records a supported shared-`brk` race that needs a reproducer and repair. On a quiet host, write the benchmark table into `docs/kernel/campaign-report.md`. Complete the independent review of `git diff e4b9c9e65cd51ef29e5021835419a1d9a7733f20 HEAD -- kernel/nanos` and fix P1 and P2 findings with tests.
- **Fix with:** `review`
- **Effort:** L
