# macOS performance handoff, 2026-11-14

Source: the Antigravity CLI session `e18aa7f2-df15-45d4-83dc-2cb9454b5fd3`
(2026-11-13 16:21 to 2026-11-14 08:43 local; models changed mid-session from
Gemini 4 Argon to Claude Opus 5.5 to Gemini 4 Hydrogen XHigh). Its working
directory was `/Users/josh/d/bctl`. Pull `main` at `fd74442b`, which is pushed.

**A second agent is fixing every ticket in `TODO.md`. Do not touch `TODO.md`.**
This handoff covers only the performance campaign against
[macos-workflows.md](../macos-workflows.md) "Targets" (BCTL-21, and BCTL-22
where already committed).

Native reopen of the source session (`agy --conversation
e18aa7f2-df15-45d4-83dc-2cb9454b5fd3`) is possible but would replay a context
whose last state is the user's "contineu" with no model turn after it. This
file is the handoff. Do not copy the transcript into git.

## Targets and status

From [macos-workflows.md](../macos-workflows.md). Every one is unmet as of
2026-11-14, and nothing measured today is recorded in that file or in
[optimizations.md](../optimizations.md) yet (see "Docs gap").

| Measure | Acceptance target | Best observed 2026-11-14 |
| --- | --- | --- |
| Warm agent-browser form (browser-side median) | ≥20% below native; 1.5× = ~25 ms | 20.0–33.8 ms (3 samples, bctl only, not interleaved) |
| Warm agent-browser news (browser-side median) | ≥20% below native; 1.5× = ~66 ms | 78.1–112 ms |
| Grab, complete task | ≥20% below native | Native 15,612 ms vs bctl 15,673 ms (n=1, inconclusive) |
| Shadowdriver complete task | ≥20% below native | Native 565.6 ms vs bctl 595.5 ms (n=1, inconclusive) |
| Fresh browser startup to usable CDP | ≤300 ms **and** no slower than native | Warm 302–307 ms; native shadowdriver startup 268.7 ms (bctl 306.7 ms). Target not met on shadowdriver, roughly tied on grab |
| Host physical footprint | ≥50% below tuned native | Not re-measured today (316 MiB marginal/VM vs ~280 MiB native-software, Nov 13) |
| CPU seconds per completed task | No increase over native | Bctl shadowdriver CPU 0.93 s vs native 1.34 s; bctl grab 2.36 s vs 2.42 s (n=1) |

Statistical bar: at least five alternating clean samples per side, same
outputs, host-load gate green before and after every retained sample, change
above 3% and non-overlapping medians. Nothing from today meets that bar.

## What landed on main

All pushed. Roughly chronological order. Numbers cited are from commit messages
and receipts; where a receipt is under `.work/` it is disposable.

Kernel (`kernel/`, Hydrogen):

- `75a832fa` direct FDT boot on aarch64; `0ee9a87e` nanos ELF booted directly,
  EDK2 fetch/flash/ACPI removed (~45 ms cold boot on HVF); `181f68ca`
  (kernel fix) dev runner boots the ELF directly; `c1070ae1` pairs the macOS
  images with their matching kernel ELF instead of a firmware-paired boot.
- `1923bd99` direct DAX block mapping for read-only clean pages; `fb7aee42`
  maps the read-only raw root disk via an HVF DAX region as virtio-blk SHM
  region 0, so clean guest pages are not duplicated into guest RAM.
- `838fa44c` direct aarch64 `tlbi` broadcast instead of rendezvous shootdown
  (preserving `processors_rendezvous` for membarrier), plus DAX fault-around
  mapping up to 512 KiB of contiguous uncached DAX extents per fault and
  directly copying writable private DAX pages on write faults.
- `026e329b` DAX DAX-backed pagecache: skip async readahead on DAX nodes,
  `pagecache_read_sg` copies DAX extents directly, private program segments
  copied synchronously in 64 KiB aligned chunks, batch-map filled windows.
- `a21c8882` aarch64 `CNTVCT` vDSO clock under LINUX_2.6.39 (`__kernel_*`
  symbols musl and rustix expect), `vdso_now` backed by EL0 counter, `-O3`
  on VDSO objects.
- `56349fa5` waking threads migrate to the caller's CPU when the last CPU
  used is not polling.
- `f2463fdc` yield `kern_pause()`, lazy kernel address space, 16-bit ASIDs,
  polling-first push.
- Kernel gate suites (`sh kernel/dev/test.sh helium`, `virtiofs`, `harness`,
  `latency`, `probe`, `probe --stress`) were run around the TLB/DAX batch
  (06:24–06:29) with all reported passing. Which suites ran for each of the
  later DAX-read/pagecache commits is not recorded; re-run before claiming
  kernel qualification.

Guest (`guest/appliance/`, musl, built into the Nanos image):

- `d684120a` binds the readiness/check port before browser boot; cold startup
  830→475 ms, warm 567→379 ms, shadowdriver total 866–931→652–713 ms.
- `89938271` skips the duplicate headless probe on the first boot check.
- `5cf948df` prebuilds the fontconfig cache for Nanos epoch-zero font dirs
  and spawns Chromium directly; `ad6744a3` polls CDP every 2 ms at startup and
  leans headless flags; `ac9ab491` in-process GPU, drops `--disable-dev-shm-usage`,
  disables SpareRendererForSitePerProcess/OptimizationHints/MediaRouter.
- `9a3760b3` passes `--disable-frame-rate-limit` (BackToBackBeginFrameSource).
  Owner approved the active-CPU trade-off. Interop passed 113 of 113 on the
  rebuilt image. Change-tier receipt: form total 93.22→27.23 ms browser
  (wall 279.56→128.61 ms), news total 149.80→110.24 ms (wall 255.65→
  224.61 ms), active VM CPU 1.96→3.82 s. Recorded in
  [optimizations.md](../optimizations.md) line 107 and the
  [macos-workflows.md](../macos-workflows.md) frame-rate section.
- `fd74442b` (this session's commit, see below) headless flag batch.

Host (`crates/`) and relay:

- `6de992f6` macOS launches `nanos-krun serve` directly in an owned process
  group (one fewer process hop) and the daemon binds its socket right after
  locking so CLI retries land in the backlog.
- `51e357f3` drops profile-clone fsyncs, SQLite `synchronous=NORMAL`, skips
  empty reconciliation writes and eager startup GC, tightens daemon/readiness
  backoff; `d3819c5d` shortens readiness polling intervals.
- `56edd8c4` sets `TCP_NODELAY` on host and guest TCP streams.
- `a5ea837c` cuts gateway/daemon/IPC latency: cached SQLite lookups, one-shot
  socket header reads instead of 1-byte reads, no per-connection thread, no
  IPv6 localhost fallback stall, cached `/json/version` vsock round trip.

Runtime:

- `58ebfc68` propagates partial display damage through virtio-gpu and VNC
  (alternating scanouts publish only bounded dirty regions). BCTL-22's headed
  qualification is not done.

Bench: `636cbb90` `kernel_experiment.py` (build baseline+candidates from git
refs, cache by input trees, run tier gates, interleave, ≥5 non-overlapping
run medians per side, 3% minimum difference) and `macos_warm_cdp.py` /
`macos_workflows.py` / `macos_startup.py` / `macos_warm_trace.py`.

## `fd74442b`: what it is and what it lacks

Headless-only Chromium arguments added: `--animation-duration-scale=0`,
`--wm-window-animations-disabled`, `--disable-smooth-scrolling`,
`--cc-scroll-animation-duration-in-seconds=0`, `--force-prefers-reduced-motion`,
`--hide-scrollbars`, `--disk-cache-dir=/tmp/cache` (Nanos tmpfs, not the
persistent profile volume), `--metrics-recording-only`, `--no-pings`,
`--password-store=basic`, `--disable-hang-monitor`,
`--enable-features=NetworkServiceInProcess2,LazyKeyedServiceInstantiation,…`,
`--disable-features=…,SegmentationPlatform,overlay-scrollbars,helium-noise,helium-split-clienthello`;
removed `--use-gl=disabled` (in-process GPU) and `--disable-crash-reporter`
(`--disable-breakpad` retained). Unit test
`headless_disables_frame_rate_limit_and_caches_in_tmpfs` asserts the flag list.

Verification, honestly: `cargo check --tests --target
aarch64-unknown-linux-musl` passes; the guest unit tests cannot execute on
macOS (Linux-only libc). The last two edits (adding `--disk-cache-dir` and the
test rename) were made **after** the last benchmark sample. No image was built
from the exact committed state; interop and workflows were not run with it; the
flag batch is in no benchmark receipt that establishes a median. Its only
evidence is:

- `.work/tmp/wc-opt1.json` (08:13, 3 rounds, bctl-only): FORM browser
  32.9/20.0/20.4 ms, NEWS browser 105.4/78.1/91.6 ms; cold open 18.2 ms.
- `.work/tmp/wf-opt1-quick.json` (n=1 per side, dirty tree at `a5ea837c`):
  grab 15,672.6 ms vs native 15,612.7; shadowdriver 595.5 vs 565.6; CPU
  shadowdriver 0.93 vs 1.34 s. No verdicts possible; its "after"
  `perf-gate-ready` printed `hot process tree (>= 100% CPU)` with code 0,
  which the intermediate model read as clean — treat that sample as
  contaminated until re-gated.

Keep-or-revert decision is yours; see "Needs" below.

## Startup investigation (where it stopped)

Numbers (`macos_startup.py`, `startup_ms`, three samples each, first is cold):

| Receipt | bctl_commit | Samples |
| --- | --- | --- |
| `.work/tmp/startup-post-vdso.json` | `a21c8882` | 784.4 / 477.6 / 471.6 |
| `.work/tmp/startup-post-kernel.json` | `026e329b` | 704.4 / 338.2 / 324.4 |
| `.work/tmp/startup-host.json` | `026e329b` (pre-`a5ea837c` binary) | 609.9 / 347.9 / 340.0 |
| `.work/tmp/startup-nativeflags.json` | `a5ea837c` | 456.0 / 331.9 / 301.9 |
| `.work/tmp/startup-opt1.json` | `a5ea837c` + dirty flags | 438.1 / 305.2 / 306.7 |

Earlier in the campaign: direct ELF boot + readiness pre-bind + fontconfig
cache took cold startup 830→475 ms and warm 567→379 ms (`d684120a`).

Root-cause threads left open:

- The 2026-11-12 profile recorded first visible guest-readiness at
  823 ms (658–888), cold `up` 923 ms, daemon startup 140 ms — all far worse
  than the above, so today's numbers reflect the new stack. The remaining
  warm ~300 ms breaks down roughly as: ~9 ms host `bctl` spawn (vs ~6 ms
  `/bin/echo` fork+exec baseline on this host; the 8.4 MiB binary links
  Security, CoreFoundation, sqlite3, iconv), ~35 ms between `bctl up` and the
  supervisor starting (in `daemon::up` — session record, SQLite writes,
  directory creation, archive_environment, ublock-disable in fresh profiles),
  ~57 ms entering the VM, ~19 ms network setup, then guest init to the
  readiness port. The ~35 ms pre-supervisor stage was the open question;
  `BCTL_STARTUP_TRACE` is plumbed through `crates/runtime-nanos/src/startup.rs`
  and `runtime/nanos-krun.c` and the guest (`guest/appliance/src/startup.rs`)
  for in-guest stage timing; compare against the plain `startup_ms` receipts.
- The macOS path spawns `nanos-krun` → `serve`; `6de992f6` removed the extra
  `launch` hop. Loading 6.1 MiB `libkrun.dylib` is part of the VMM stage.
- `cpus: 4` measured worse on warm CDP (`.work/tmp/wc-cpu4.json`: FORM
  62–83 ms, NEWS 121–143 ms); keep 2 vCPUs.

## News workflow (the actual gap)

`macos_warm_trace.py` (traced, single round, console output only — not saved):

- `news_open`: CDP browser 49 ms (`Runtime.evaluate` 43.6, `Page.navigate`
  5.0), but four active threads (ThreadPoolForegroundWorker, CrRendererMain,
  CrBrowserMain, VizCompositorThread) with wall ≫ CPU, i.e. descheduled on
  the vCPU budget.
- `news_snapshot` is dozens of serial round trips dominated by
  `Runtime.callFunctionOn` (385 calls) and `Accessibility.getFullAXTree`.
- NEWS TOTAL wall (~197–233 ms across rounds) vs CDP command union
  (~92 ms) leaves roughly ~105 ms outside browser CDP: agent-browser process
  spawn, daemon IPC, and the fixed 100 ms `wait` poll. All are shared with
  the native side; confirm the union with `macos_warm_cdp.py` / cdp_relay
  before spending more kernel or flag effort there.

## Docs gap (blocks calling any of this done)

Repo rules require every finished attempt to be recorded in
[optimizations.md](../optimizations.md) and the status in
[macos-workflows.md](../macos-workflows.md). **None of 2026-11-13/14's
kernel, host, or guest commits are recorded anywhere**: no ledger rows, no
write-ups, no receipts under `tests/bench/results/2026-11-14-*`. Everything
lives in `.work/tmp/*.json` (`wf-opt1-quick.json`, `wc-*.json`,
`startup-*.json`, `wf-dfrl.json`, `wf-noanim-quick.json`) and
`.work/exp/receipts/20261114T015613-vdso-clock.json`. `tlb-dax` and
`sched-asid-ab` edit-tier receipts are loose in `.work/tmp/`
(2026-11-14 06:29 / 00:57, all 17 metrics "no change established").
Minimum before claiming done: move receipts into
`tests/bench/results/2026-11-14-*/`, add ledger rows (adopted/rejected/
no-change with the disclaimer that Kernel A/B found no warm-CDP median
change), extend the macos-workflows.md startup section with the table above.
The frame-rate-limit entry already exists (2026-11-13).

Kernel A/B results to record: vDSO candidate improved only
`Runtime.callFunctionOn` (edit tier, `20261114T015613-vdso-clock.json`);
TLB+DAX batch and the sched/ASID A/B: 17/17 "no change established" each.
Do not claim browser-side wins for the kernel batch; its value is plausible
in boot and steady-state kernel time that these metrics do not see.

## Reproduction commands

```sh
sh kernel/dev/test.sh helium        # after any kernel/ commit; latency, harness, virtiofs, probe, probe --stress
BCTL_BROWSER_ARCHIVE=DIR images/nanos-helium/build-macos.sh   # needs Docker, arm64 Helium archive
perf-gate-ready                     # before every measurement; perf-gate-ready -1 after; discard overlapping runs
.work/interop/venv/bin/python tests/bench/kernel_experiment.py --name NAME --candidate REF [--candidate REF2] [--tier change]
.work/interop/venv/bin/python tests/bench/macos_workflows.py --workloads shadowdriver,grab,agent-browser,agent-browser-news --rounds 5 --resource-rounds 5 --cpu-rounds 5 --out FILE
.work/interop/venv/bin/python tests/bench/macos_startup.py --rounds 3 --out FILE
.work/interop/venv/bin/python tests/bench/macos_warm_cdp.py --out FILE
.work/interop/venv/bin/python tests/bench/macos_warm_trace.py --rounds 1
sh tests/interop/run.sh             # Nanos default on Linux; macOS needs the qualifier envs from the macOS handoffs
```

Guest unit tests compile for musl only:
`cargo check --tests --target aarch64-unknown-linux-musl --manifest-path guest/appliance/Cargo.toml`.
Host is macOS arm64; measurements only on a quiet host (`perf-gate-ready`).

## Rules carried forward

- `kernel/` must be committed before `runtime/build-nanos*.sh` (they build
  only the committed tree).
- Never two measurements at once on this host; interleave baseline/candidate;
  ≥5 non-overlapping samples; medians; 3% minimum; same output checks.
- Keep the binary/flag changes behind the headless path; do not enable
  experimental browser flags broadly (`docs/webmcp.md` for the WebMCP stance).
- Do not touch `TODO.md` (other agent owns it today). The `docs/handoffs/`
  convention is a dated file per handoff; state that must outlive the session
  is committed in the repo, not left in `.work/`.
- Linux x86_64 qualification of the Nanos leg is still unrun (see
  `docs/kernel/development.md` and the 2026-11-11 handoff); Hydrogen
  benchmark report and independent review are BCTL-20.

## Needs you

1. **Keep or revert `fd74442b`.** Recommend keep and qualify: rebuild the
   image, run change tier / workflows with ≥5 interleaved rounds and the 113
   interop checks, then record the ledger. Revert if it regresses or if you
   prefer committed-code evidence before any more flag surface.
2. **Docs updates** (optimizations.md ledger + macos-workflows.md sections)
   for the whole 2026-11-13/14 campaign — required before "done". Also decide
   whether the `.work/tmp` receipts get promoted into
   `tests/bench/results/2026-11-14-*`.
3. **Startup**: pursue the ~35 ms pre-supervisor `daemon::up` stage, or accept
   ~305 ms warm CDP and move to the news path? Native is ~269 ms on
   shadowdriver startup, so "no slower than native" is not met yet.
4. **News path**: attack the serial CDP snapshot (agent-browser-side command
   count and round-trip latency) or the ~105 ms non-CDP share (spawn, daemon
   IPC, wait poll)? The kernel A/B says kernel work will not move it.
5. **BCTL-22** headed-damage qualification and **BCTL-23** WebMCP VM
   qualification remain open; Linux x86_64 qualification remains open.
