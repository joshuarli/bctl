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

## Browser

| Change | Result | Status | Evidence | Retry when |
| --- | --- | --- | --- | --- |
| GPU rasterization as the explanation for the native lead | Native with `--disable-gpu` is no slower than native with the GPU (form screenshot 11.8 vs 19.5 ms, form total 39.7 vs 37.8 ms). | Ruled out | [native software receipt](../tests/bench/results/2026-11-13-macos-workflows/warm-cdp-native-software.json) | — |
| Slower guest CPU or missing CPU features | In-page compute, allocate-and-touch, and canvas raster match native within 3%. `AT_HWCAP` advertises NEON, CRC32, and the crypto extensions. | Ruled out | [workflow write-up](macos-workflows.md#wakeups-and-address-space-switches) | — |
| Operation-scoped warm screenshot and click tracing (startup-only BCTL_TRACE_CHROMIUM=1 is insufficient) | — | Untried | [BCTL-21 graphics campaign](CAMPAIGN-GRAPHICS-PERFORMANCE.md) | — |
| Sharing read-only file pages, such as the browser binary, across VMs for crawl density | — | Untried | BCTL-21 | — |
| Running a woken thread on the waker's CPU (wake-affine), as an alternative or complement to idle polling | — | Untried | — | Idle polling's remaining 6 µs round trip matters in a profile |
