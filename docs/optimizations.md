# Optimization ledger

This ledger records every performance change and hypothesis that bctl has
tested, whether it was adopted or not, so that nobody repeats a dead end
without new evidence. Check it before starting a performance change. Add an
entry when an attempt finishes, including attempts that change nothing.

Each entry gives the change, what was measured, the decision, the evidence, and
the condition under which a retry would make sense. Numbers are medians unless
stated. The detailed write-ups stay where they are; entries link to them.

## Running an experiment

`tests/bench/kernel_experiment.py` runs a kernel or image experiment end to
end on Apple Silicon. Commit the candidate first, then run, from the repository
root:

```sh
.work/interop/venv/bin/python tests/bench/kernel_experiment.py --name NAME --candidate REF
```

It builds the baseline (`main` unless `--baseline` names another ref) and each
`--candidate` in a throwaway worktree and caches the result in
`.work/exp/builds/` by the ref's `kernel`, `guest`, `images`, and `runtime`
trees. It runs the tier's kernel gates on each build and stops if one fails. A
gate that does not exist at an older ref is reported as absent, never as
passed. Measurement interleaves the variants round-robin through the warm
agent-browser workload. It checks `perf-gate-ready` before and after each run
and repeats a run that overlapped load, so a busy host delays the experiment but
never contributes a number. The receipt goes to `.work/exp/receipts/`, with a
suggested ledger row at the end of the output.

A metric is reported improved or regressed only when five or more run medians
per variant do not overlap and their medians differ by more than 3%. A
comparison of a build with itself (A/A) then disagrees on about 0.8% of metrics,
and the [pilot](../tests/bench/results/2026-11-13-experiments/asid-idle-poll-pilot.json)'s A/A comparison reported no change on all 17 metrics while detecting the ASID change's faster round trips. `--cflags
-DHYDROGEN_FLUSH_PROFILE` builds every variant with the profile counters and
adds their deltas to the receipt. Several `--candidate` options share one
baseline. The bctl binary is `target/release/bctl` unless `--bctl` names
another, so rebuild it after host-side changes.

| Tier | What runs | Observed cost | When |
| --- | --- | --- | --- |
| Edit | `--tier edit` (default): build, `kernel/dev/test.sh latency probe`, 5 interleaved pairs | 72 s for three cached variants; about 45 s more per new build | Every candidate |
| Change | `--tier change`: adds the 104-run `harness` gate (103 s) | about 2 min more per new build | Before adopting a candidate |
| Scenario | `BCTL_RUNTIME=nanos sh tests/interop/run.sh`, `tests/bench/macos_workflows.py`, `tests/bench/macos_crawl.py` | unmeasured | Before landing an adopted change, when named |
| Full | Linux x86_64 qualification, `kernel/dev/test.sh g0 upstream external helium-stress` | tens of minutes | Only on an explicit request |

Costs were measured on an Apple M5 Max with warm build caches and a quiet
host. The toolchain must be bootstrapped once in the main checkout with
`sh kernel/dev/bootstrap.sh`. Delete `.work/exp/builds/KEY` to force a rebuild.
Record each finished experiment below, adopted or not.

Statuses:

- **Adopted:** shipped, with a measured benefit.
- **Rejected:** measured, with no benefit or a net cost.
- **Inconclusive:** measured, but the spread is too wide to decide.
- **Ruled out:** a suspected cause that measurement showed is not the cost.
- **Untried:** a candidate that has not been measured.

## Guest kernel

| Change | Result | Status | Evidence | Retry when |
| --- | --- | --- | --- | --- |
| ASIDs on aarch64 and 50 µs idle polling before WFI (Hydrogen `9f9b61d2`, 2026-11-13) | Socketpair round trip on 2 vCPUs 12.8 → 6.0 µs p50, 34 → 9 µs p99. `DOM.resolveNode` about 30% faster. Seven warm iterations 6% less wall time, same VM CPU, idle CPU 0.7 → 0.8% of a core. Browser-side step totals only 1–3% better. | Adopted | [workflow write-up](macos-workflows.md#wakeups-and-address-space-switches), [A/B receipt](../tests/bench/results/2026-11-13-macos-workflows/asid-idle-poll-ab.json) | — |
| Targeted TLB shootdown instead of broadcast with rendezvous | Not implemented. A profiling kernel measured 21 ms of rendezvous and join wait across a 4 s warm workload. | Ruled out | [workflow write-up](macos-workflows.md#wakeups-and-address-space-switches) | Rendezvous wait exceeds about 5% of browser-side time in a profile |
| Free-page reporting off (guest does not negotiate `VIRTIO_BALLOON_F_REPORTING`), 2026-11-13 | Warm form and news browser-side totals overlap in three interleaved pairs. Reporting keeps VMM memory returning to the host. | Rejected | [A/B receipt](../tests/bench/results/2026-11-13-macos-workflows/free-page-reporting-ab.json) | — |
| Free-page reporting every 5 s regardless of freed memory | Idle CPU 0.42% of a core against 0.32% for the 32 MiB-threshold version. | Rejected | [performance findings](performance.md#findings) | — |
| Free-page reporting after 32 MiB freed (Hydrogen `868726ad`) | On Linux QEMU, VMM RSS after closing a 768 MiB tab 1281 → 533 MiB within 5 s. Idle RSS and CPU unchanged. | Adopted | [performance findings](performance.md#findings) | — |
| `transparent_hugepage: "never"` (small anonymous pages) | Shadowdriver form 1150 vs 1155 ms, CPU and footprint ranges overlap. | Rejected | [page-size experiment](macos-workflows.md#anonymous-page-size-experiment) | — |
| Page-fault cost and the per-process vmap lock | 34 ms of kernel paging time and lock wait across about 65,000 faults in a 4 s warm workload. | Ruled out | [workflow write-up](macos-workflows.md#wakeups-and-address-space-switches) | — |
| 16-bit ASIDs on aarch64 (`622785e5`, 2026-11-13) | Form 86.4 → 89.7 ms, news 155.4 → 150.0 ms, VM CPU 1.85 → 1.87 s (no change established). The guest spawns fewer than ten processes, so 8-bit ASIDs never roll over during the workload. | Ruled out | [batch receipt](../tests/bench/results/2026-11-13-experiments/queue-batch.json) | A workload spawns more than 255 concurrent address spaces |
| 200 µs idle polling window instead of 50 µs (`a1755913`, 2026-11-13) | Cuts wakeup IPIs from 1,005 to 445 and improves `Accessibility.getFullAXTree` 9.94 → 9.43 ms, but regresses VM CPU seconds 1.85 → 2.12 s (+14.6%) with no established change on form or news totals. | Rejected | [batch receipt](../tests/bench/results/2026-11-13-experiments/queue-batch.json) | — |
| ARM64 `yield` instead of `dsb sy; wfe` in `kern_pause()` (`4f1d15f3`, 2026-11-13) | Regresses VM CPU seconds 1.85 → 2.12 s (+14.6%) and increases TLB flush rendezvous wait from 4.4 ms to 147.9 ms because `kern_pause()` is also used in spin-lock and rendezvous loops. | Rejected | [batch receipt](../tests/bench/results/2026-11-13-experiments/queue-batch.json) | — |
| Running a woken thread on the waker's CPU (wake-affine, `ea86a6e3`, 2026-11-13) | Socketpair round-trip p50 6.0 → 5.2 µs, but p99 degrades 7.6 → 13.3 µs and scheduler push migrations jump from 1,640 to 3,917 because woken threads queue behind the still-running waker; form and news totals unchanged. | Rejected | [batch receipt](../tests/bench/results/2026-11-13-experiments/queue-batch.json) | — |
| Stack fast path in `epoll_wait` for `timeout == 0` or already-ready fds, skipping the waiter, staging buffer, and closure allocations (`0c332384`, 2026-11-13) | Form 85.4 → 88.8 ms, news 147.6 → 145.0 ms, VM CPU 1.99 → 1.96 s: no change established on any of 17 metrics. Chromium's non-blocking `epoll_wait` calls are not a measurable cost. | Rejected | [receipt](../tests/bench/results/2026-11-13-experiments/epoll-timer-frame-receipt.json) | A profile attributes more than 5% of guest CPU to `epoll_wait` |
| `RUNLOOP_TIMER_MIN_PERIOD_US` 100 instead of 1000 (`81ee8c44`, 2026-11-13) | Form 85.4 → 86.2 ms, news 147.6 → 149.5 ms, VM CPU 1.99 → 1.91 s: no change established. The 16.7 ms stalls in the traces come from Chromium's frame clock, not the kernel timer. | Rejected | [receipt](../tests/bench/results/2026-11-13-experiments/epoll-timer-frame-receipt.json) | — |
| Steering `thread_wakeup` directly to an idle CPU before syscall exit (`0234df97`, 2026-11-13) | Cuts scheduler push migrations from 1,501 to 603 and steals from 196 to 13, but increases wakeup IPIs from 955 to 1,134 when wakers block immediately after waking a peer; form (86.8 → 87.2 ms) and news (148.5 → 145.2 ms) overlap. | Rejected | [wake-idle receipt](../tests/bench/results/2026-11-13-experiments/wake-idle.json) | — |

## VM configuration

| Change | Result | Status | Evidence | Retry when |
| --- | --- | --- | --- | --- |
| Four vCPUs instead of two, first attempt | Stalled at boot on the third attempt (a kernel deadlock, fixed in Hydrogen `4a862120`). | Rejected | [four-vCPU experiment](macos-workflows.md#four-vcpu-experiment) | — |
| Four vCPUs, after the deadlock fix | Complete tasks inconclusive, since native medians moved up to 22% between runs. Browser-side: renderer round trips 3.4 → 1.7 ms, but screenshots 41–44 → 58–59 ms, and form totals unchanged. | Rejected | [retest](macos-workflows.md#kernel-deadlock-fix-and-four-vcpu-retest), [warm CDP receipts](../tests/bench/results/2026-11-13-macos-workflows/warm-cdp-cpu4.json) | Screenshot cost no longer grows with vCPU count |
| One vCPU | Renderer round trips 5.4–6.6 ms against 3.4–3.8 ms on two. | Rejected | [warm CDP write-up](macos-workflows.md#browser-side-time-of-the-warm-agent-browser-steps) | — |
| macOS latency tier 0 (`taskpolicy -l 0`) for the VMM | Warm form 182 → 277 ms, news 283 → 251 ms, with overlapping ranges. | Inconclusive | [QoS experiment](../tests/bench/results/2026-11-13-macos-workflows/qos-experiment.json) | CPU QoS class, not the latency tier, is tested |
| Stronger host reclamation (`SMOLVM_BALLOON_RECLAIM`) | Only a 16-capture smoke check, which passes. No savings measured. | Untried | [balloon write-up](macos-workflows.md#balloon-queue-routing) | A density measurement is set up |
| Balloon reporting queue routed by negotiated features | A correctness fix. Crawl latency, CPU, and footprint ranges overlap. | Adopted | [balloon write-up](macos-workflows.md#balloon-queue-routing) | — |

## Host and client

| Change | Result | Status | Evidence | Retry when |
| --- | --- | --- | --- | --- |
| Daemon readiness poll every 10 ms instead of 100 ms | Fresh daemon readiness 116.7 → 21.0 ms, complete form task 10.7% faster, same CPU. | Adopted | [daemon discovery interval](macos-workflows.md#daemon-discovery-interval) | — |
| Bundled uBlock Origin pre-disabled on new profiles (`8f2166b4`) | Idle CPU 7.6% → 0.32% of a core on Linux. Its first-run filter compile also held every request until done. | Adopted | [performance findings](performance.md#findings) | Helium ships an ad blocker with precompiled lists |
| Partial framebuffer damage propagation across libkrun virtio-gpu, SmolVM display, and VNC (`58ebfc68`, 2026-11-13) | Negotiates `KRUN_DISPLAY_FEATURE_PARTIAL_FRAMEBUFFER`, tracks per-resource and per-scanout damage across double-buffered scanout flips, publishes shared `Arc<Vec<u8>>` frames with a 64-generation damage ring, and bounds VNC tile scanning and viewport clipping while retaining unrequested off-viewport damage. Verified by 50 unit and exact-pixel oracle tests across alternating two-buffer flips, missed generations, partial RFB requests, overflow, and resize. | Adopted | [graphics campaign](CAMPAIGN-GRAPHICS-PERFORMANCE.md) | — |

## Browser

| Change | Result | Status | Evidence | Retry when |
| --- | --- | --- | --- | --- |
| GPU rasterization as the explanation for the native lead | Native with `--disable-gpu` is no slower than native with the GPU (form screenshot 11.8 vs 19.5 ms, form total 39.7 vs 37.8 ms). | Ruled out | [native software receipt](../tests/bench/results/2026-11-13-macos-workflows/warm-cdp-native-software.json) | — |
| Slower guest CPU or missing CPU features | In-page compute, allocate-and-touch, and canvas raster match native within 3%. `AT_HWCAP` advertises NEON, CRC32, and the crypto extensions. | Ruled out | [workflow write-up](macos-workflows.md#wakeups-and-address-space-switches) | — |
| Operation-scoped warm screenshot and click tracing (`436d8458`, 2026-11-13) | Bounded per-operation CDP `Tracing.start`/`Tracing.end` traces show `copy_output_readback` is ≤1.3 ms on all sides, while `bctl` spends more time in `scheduler_toplevel` (31.5 vs 15.7 ms on `news_click`), `compositor_viz` (11.7 vs 2.4 ms), `ipc_mojo` (7.9 vs 3.7 ms, with 4.1 ms CPU), and unattributed CDP intervals on screenshots (38.4–58.4 ms). | Adopted | [workflow write-up](macos-workflows.md#operation-scoped-warm-traces-bctl-21), [trace receipt](../tests/bench/results/2026-11-13-experiments/warm-trace-receipt.json) | — |
| Headless `--disable-frame-rate-limit`, so Viz uses `BackToBackBeginFrameSource` instead of the 60 Hz `DelayBasedBeginFrameSource` (`735df288`, 2026-11-13) | Traces showed screenshots waiting two to three 16.7 ms frame ticks and each click's `mouseReleased` hit-test query queued behind the next `BeginMainFrame` commit. With the flag: form click 25.3 → 4.1 ms, form screenshot 42.6 → 8.0 ms, news screenshot 49.7 → 15.9 ms, form total 85.4 → 28.3 ms, news total 147.6 → 121.8 ms (all improved). Active VM CPU across the workload 1.99 → 3.70 s (regressed, +86%); idle CPU after load 0.8–1.1% → 1.2–1.7% of a core. | Not adopted: latency against CPU trade-off awaiting a decision | [receipt](../tests/bench/results/2026-11-13-experiments/epoll-timer-frame-receipt.json) | A decision accepts the CPU cost, or a variant keeps the latency win without it |
| Multi-instance density and shareable guest browser file-backed footprint (`c8680799`, 2026-11-13) | Across 1, 2, and 4 concurrent instances, `bctl` uses 316.7, 633.2, and 1,267.4 MiB deduplicated host physical footprint (316.9 MiB marginal per VM, 11.52 workflows/s/GiB at 4 instances) against `native-software`'s 287.5, 568.0, and 1,127.1 MiB (279.9 MiB marginal, 16.07 workflows/s/GiB). Native shares ~471 MiB of clean `__TEXT` and mapped files via the host page cache, whereas each VM loads guest browser files (247 ELF, `.pak`, and `icudtl.dat` files totaling 597.9 MiB upper bound) into private host `VM_ALLOCATE` dirty memory. | Adopted | [workflow write-up](macos-workflows.md#multi-instance-density-and-shareable-file-footprint-bctl-21), [density receipt](../tests/bench/results/2026-11-13-experiments/density-receipt.json) | Shared read-only DAX/virtio-fs or shared file-backed guest mappings are prototyped |
