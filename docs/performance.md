# Performance

`bctl-bench` (`crates/bench`) runs named suites against real appliance VMs and
writes one dated JSON result per run to `tests/bench/results/`. This page holds
the method, the latest results with their spread and conditions, the gates, and
what is deferred. Gates live in `tests/bench/gates.json`.

Nothing here is a claim about unikernels, other VMMs, or other products. External
numbers are targets, never measurements of bctl.

## Running a suite

Prerequisites: `runtime/build.sh`, `BCTL_BROWSER_ARCHIVE=DIR images/build.sh helium`
(Helium r6 or later), a Speedometer checkout (below), and a release build:

```sh
cargo build --release -p bctl -p bctl-bench
target/release/bctl-bench lifecycle   [--rounds N] [--quiet-wait SECS] [--out DIR]
target/release/bctl-bench interaction [--rounds N] [--quiet-wait SECS] [--out DIR]
target/release/bctl-bench browser     [--rounds N] [--iterations N] [--quiet-wait SECS] [--out DIR]
target/release/bctl-bench resources   [--rounds N] [--quiet-wait SECS] [--out DIR]
target/release/bctl-bench viewer      [--rounds N] [--quiet-wait SECS] [--out DIR]
target/release/bctl-bench gate [RESULT.json...]
target/release/bctl-bench serve
```

Every suite measures the `helium` image, the one `bctl up` launches, creates its
own `BCTL_HOME` under `.work/tmp` with its own daemon, and names sessions
`h1-...`, so it never touches the user's daemon. A suite that is killed leaves
its machines behind; stop them with `bctl down` under that home. `serve` hosts
the pinned Speedometer tree for a manual run.

The interaction and browser suites load pages from a Speedometer checkout:

- Speedometer 3.1: `git clone --depth 1 -b release/3.1 https://github.com/WebKit/Speedometer .work/dl/speedometer/src`
  (commit `1386415be8fef2f6b6bbdbe1828872471c5d802a`). It is served locally to
  every browser, so no run depends on the public site. Besides the benchmark
  itself, the interaction suite drives its React TodoMVC build and the browser
  suite loads its `news-next` site.

## Method

- **Quiet host.** A time-based sample starts only when `perf-gate-ready` exits 0
  and is kept only when `perf-gate-ready -1` exits 0 right after it. A sample that
  fails either check is stored under `discarded` with its value and reason, never
  in `samples`. Waiting for a quiet host is bounded per suite (`--quiet-wait`,
  default 900 s). When the budget is spent the suite stops starting timed work,
  records `perf gate deferred: host busy`, and no statistic is computed from
  discarded samples. The check reads the one-minute load average, so a spike
  shorter than a minute can pass it.
- **Memory and size are never gated on load.** They are measured on any host.
- **Statistic.** Latency measures report the median and, where the claim is about
  the tail, p95 (nearest rank). Each measure carries `n`, `min`, `q1`, `q3`, `max`,
  and every raw sample. A measure with fewer than 5 clean samples has status
  `unclean` and no gate. `info` marks a single fact (a size, a count).
- **Interleaving.** Where two paths are compared, the suite alternates them in
  blocks and flips the order each round (gateway and direct).
- **Result file.** `suite`, `started` (UTC), `host` (model, chip, CPUs, memory,
  macOS), `commit` and `dirty`, `smolvm`, `images` (tar size and config digest),
  `browser_versions`, `measures`, and `notes`.

### What each suite times

| Suite | Timed region | Source of truth |
| --- | --- | --- |
| `lifecycle` | `bctl up` from invocation to exit (it blocks until ready), `bctl down`, daemon start (first command on an empty home to answer). Stage timeline: a poller runs `bctl status` every 20 ms and records when `stage` first names each stage. | CLI wall time |
| `lifecycle` pause | `bctl pause` and `bctl resume` on a ready session with a page open; afterwards `bctl page get title` must return that page's title | CLI wall time |
| `interaction` automation | one TodoMVC workflow per round through `bctl page`: `goto` the React TodoMVC build (waiting for `load`), `fill` and `press Enter` 5 times, `check` the first todo, `snapshot`; each step and the whole workflow | CLI wall time |
| `interaction` CDP | one sequential `Runtime.evaluate("1")` at a time on one WebSocket, 10 warm-up calls, blocks of 100 | client wall time |
| `interaction` input | RFB pointer press or key tap written, until an incremental update repaints a probe pixel of a full-viewport color toggle; 400 ms between trials | client wall time |
| `browser` | Speedometer 3.1 `Score` per iteration from `benchmarkClient._metrics`, headed, `iterationCount` per run (default 5), one fresh page load per run; page load of Speedometer's `news-next` site (a client-rendered Next.js build of about 1 MiB) with Navigation Timing and largest contentful paint, 5 loads per batch, cache disabled | page JS |
| `resources` | `ps` RSS and CPU time, `footprint` for the macOS physical footprint, `du` for disk | host tools |
| `viewer` | RFB frame rate, draw-to-receive latency, and stream bandwidth on the direct VNC TCP port and the dashboard WebSocket: a page whose rAF loop increments a 16-bit black/white marker strip and records `performance.timeOrigin+performance.now()` per counter; three 10 s scenarios (`marker`, `fullscreen`, `scroll`) per path | page JS, RFB stream byte counts, CDP clock probes |

Checks that the timed work happened: a lifecycle sample requires the session to
be `ready`, and a resume requires the page opened before the pause; a workflow
requires the app to report 5 todos with 1 completed and the snapshot to list 5
checkboxes with exactly one checked; a Speedometer run requires `iterationCount`
scores; a page load requires `loadEventEnd` for the load's own URL and the
site's articles in the DOM; the input suite reads the page's own toggle counter
over CDP afterwards and reports it against the trials sent.

### Caveats that shape the numbers

- Cold means a fresh daemon, profile, and machine. The smolvm image cache and the
  OS page cache are warm, because the host is shared and cannot be purged.
- The stage split is the daemon's own: it names only the first missing stage at
  each of its probes, so a stage that passes between probes is not seen and the
  split is coarse. It is the resolution the product exposes.
- `vmm_rss` as reported by macOS excludes compressed pages and moves with memory
  pressure (an earlier session saw 158 MiB for a VM that later read 1.3 GiB), so
  the footprint measure is the one to gate.
- The VMM process tree is found by the per-home runtime path in its command line.
  Idle CPU is `ps` CPU time over a 60 s window, 30 s after ready.
- Each automation step is a separate `bctl` process, so every step includes CLI
  start, the daemon round trip, and the gateway, as a user's script would.
- The guest reaches the host benchmark server at `100.96.0.1`, SmolVM's gateway
  to the host loopback, through `--allow-private 100.96.0.0/30`, so page load
  time includes SmolVM's network relay.
- **Viewer.** The suite asks for Raw only. The VNC server is SmolVM's host-side
  server (`third_party/smolvm/src/agent/vnc.rs`), not libkrun, and Raw is its
  only pixel encoding; it also offers the DesktopSize, Cursor, and
  ContinuousUpdates pseudo-encodings. The suite measures the RFB stream, not
  noVNC's rendering in the dashboard. `gateway` counts the RFB payload through
  the WebSocket with the input lease; `direct` is the VMM's VNC TCP port, and
  both paths carry the same framebuffer. Latency needs the guest and host clock
  offset, taken NTP-style from the minimum-RTT of 20 bracketed `Runtime.evaluate`
  probes, with uncertainty half that RTT (0.15 ms in the latest run). Byte counts
  are protocol payload: they exclude WebSocket framing and TCP/IP overhead.
  `fps` is distinct counters decoded per one-second bucket, median over seconds
  per round and then over rounds; `fps.worst_second` is the minimum bucket across
  rounds. `missed` is the fraction of drawn counters between the first and last
  marker the viewer saw that it never decoded, so a viewer that keeps up reports
  0. The `fullscreen` and `scroll` scenarios repaint most of the viewport, so
  their bandwidth sits near `fps × width × height × 4`.

## Conditions of the latest results

| | |
| --- | --- |
| Host | Mac17,7, Apple M5 Max, 18 CPUs, 48 GiB, macOS 27.0.1 (26A434) |
| SmolVM | 1.34.0, vendored with this repository's patches |
| Guest | `helium` r6 (config `b59ec01a...`, tar 218,243,072 bytes), Helium 0.20.3.1; 2 CPUs, 4096 MiB, 1280x800 |
| Tree | main `4cd94c1` with the suite changes uncommitted (browser: `41db3e9` with the container removed; interaction: `759b308`); `bctl` release build |
| Load | No other work ran. The one-minute load average was 2.0 to 4.0 at the start of each suite (0.11 to 0.22 per core), and the suites waited 1 to 5 s in total for a quiet host. |

Suites ran one after another: lifecycle and interaction with 7 rounds, viewer 7
rounds, resources 7 boots, browser 5 rounds of `iterationCount=3`.

## Results

All time results passed the quiet-host check before and after each sample; no
sample was discarded. "Block" is one alternation unit; the block medians show
run-to-run spread where samples within a block are not independent.

### Lifecycle (7 rounds, median and minimum to maximum)

| Measure | Result |
| --- | --- |
| Daemon start (first command on an empty home) | 114.8 ms (111.8 to 116.6) |
| Cold `up` to ready | 1466 ms (1439 to 1572; quartiles 1460, 1500) |
| Warm `up` to ready (same session and profile after `down`) | 1455 ms (1442 to 1490) |
| `down` | 112.8 ms (79.1 to 159.1), 14 samples |
| Cold `up`: request to first failed readiness probe | 803 ms (789 to 871) |
| Cold `up`: first failed probe to ready | 673 ms (639 to 707) |
| Warm `up`: request to first failed readiness probe | 820 ms (783 to 857) |
| Warm `up`: first failed probe to ready | 633 ms (626 to 672) |
| `pause` | 1817 ms (1735 to 1853) |
| `resume` | 2253 ms (2160 to 2395) |
| Page kept across pause and resume | 7 of 7 |

Stage timeline: the daemon named `compositor` as the first missing stage at its
first failed probe in 7 of 7 cold and 7 of 7 warm `up`s, and named no other
stage: everything after the compositor passed between its probes. Cold and warm
do not differ beyond the spread.

Resume takes 0.8 s longer than a cold `up` at the median, but it returns the
session with its tab and page state, where a cold `up` starts with neither.
Where pause and resume spend their time was not profiled.

### Interaction (gateway and direct alternating)

Taken on `759b308` (the first-paint wait fixed) with `PERF_GATE_MAX_LOAD=0.25`
on a host with other work running: 6 of 7 rounds of the workflow and 5 of 7
direct and 3 of 7 gateway input blocks passed the quiet-host check; the rest
are under `discarded`.

TodoMVC workflow through `bctl page`, one per round:

| Step | p50 | p95 | Samples |
| --- | --- | --- | --- |
| `goto` with `--wait load` | 39.5 ms | 62.9 ms | 6 |
| `fill` | 14.3 ms | 40.2 ms | 30 |
| `press Enter` | 15.4 ms | 23.1 ms | 30 |
| `check` | 65.7 ms | 72.3 ms | 6 |
| `snapshot` | 11.9 ms | 14.7 ms | 6 |
| Whole workflow | 277.2 ms (251.8 to 313.2) | | 6 |

Before `759b308` the workflow took 450.6 ms (263 to 485) on an idle host,
because the first `fill` after `goto` waited out a 200 ms quiet interval in 4 of
7 rounds (see Findings); no step waits that long now, and the slowest `fill` was
46 ms.

CDP round trip, one call at a time, 700 calls per cell. `direct` is the exposed
Unix socket, which bypasses the gateway only; both include the guest relay and
the browser.

| Call | Path | p50 | p95 | Block medians |
| --- | --- | --- | --- | --- |
| `Runtime.evaluate("1")` | direct | 170 us | 202 us | 167 to 177 us |
| `Runtime.evaluate("1")` | gateway | 256 us | 332 us | 251 to 295 us |

The gateway adds 86 us at the median, and the block medians of the two paths do
not overlap.

Input to pixel: an RFB pointer press or key tap to the update that repaints the
probe pixel, 20 trials per block. The page counted 560 toggles for 560 trials
sent; none timed out.

| Input | Path | p50 | p95 | Block medians |
| --- | --- | --- | --- | --- |
| pointer | direct | 8.9 ms | 9.6 ms | 8.6 to 9.1 ms |
| pointer | gateway | 10.3 ms | 11.1 ms | 10.2 to 10.6 ms |
| key | direct | 8.8 ms | 9.6 ms | 8.6 to 9.0 ms |
| key | gateway | 10.2 ms | 10.9 ms | 10.2 to 10.7 ms |

Input to pixel has two modes. The idle-host run before `759b308` had every
block at 19 to 24 ms; this run had every block at 8.6 to 10.7 ms; a run between
them started at 9 to 11 ms and moved to 17 to 21 ms in its last two rounds. The
change in `759b308` does not touch the RFB path, so the mode is not the build.
What selects it is open (see Findings).

### Viewer (7 rounds, gateway and direct alternating, 10 s windows)

42 of 42 windows retained clean samples. The guest clock ran 291.9 ms from the
host clock, and the minimum-RTT offset probe had an uncertainty of 0.15 ms.
Latency has 4,151 to 5,079 samples per cell; fps, bandwidth, and missed have 7
(one per round). fps and bandwidth spreads are the per-round medians from
minimum to maximum.

| Scenario | Path | fps median (min to max) | Worst second | Latency p50 | Latency p95 | Bandwidth MB/s (min to max) | Missed |
| --- | --- | --- | --- | --- | --- | --- | --- |
| marker | direct | 58.0 (58.0 to 60.0) | 57 | 8.40 ms | 10.07 ms | 0.62 (0.61 to 0.64) | 0 |
| marker | gateway | 58.0 (58.0 to 59.0) | 57 | 8.33 ms | 10.34 ms | 0.61 (0.60 to 0.64) | 0 |
| fullscreen | direct | 78.0 (56 to 79) | 55 | 11.04 ms | 15.74 ms | 283 (213 to 298) | 0 |
| fullscreen | gateway | 77.0 (55 to 80) | 54 | 11.53 ms | 17.16 ms | 278 (217 to 297) | 0 |
| scroll | direct | 61.0 (55.5 to 67) | 54 | 12.47 ms | 13.89 ms | 234 (215 to 250) | 0 |
| scroll | gateway | 61.5 (58.5 to 68.5) | 54 | 13.32 ms | 15.19 ms | 235 (227 to 255) | 0 |

The marker scenario changes only the strip, so its bandwidth is the strip's
damage rectangles: about 0.62 MB/s at 58 fps. Fullscreen and scroll repaint most
of the 1280x800 viewport, so their bandwidth is near `fps × 1280 × 800 × 4`. The
two paths differ by less than the round-to-round spread in every cell, and the
viewer kept up everywhere (`missed` 0). Latency grows from marker through
fullscreen to scroll as the amount of changed pixels grows. An earlier run on
Helium r5 with an older runtime measured marker latency p50 at 4.3 to 4.5 ms;
the image and runtime both changed since, so the doubling is not attributed.

### Browser (Speedometer 3.1, headed, `iterationCount=3`, 5 rounds)

Speedometer finished 5 of 5 runs at a score of 31.43 (29.54 to 31.95). The
score is the mean of the iteration scores of one run; the spread is across runs.

Page load of the `news-next` site, 25 loads (5 batches of 5), median (minimum to
maximum):

| `responseEnd` | `loadEventEnd` | Largest contentful paint |
| --- | --- | --- |
| 14.2 ms (4.4 to 21.9) | 71.9 ms (60.8 to 96.5) | 140 ms (96 to 180) |

A container on the same host running Alpine's Chromium 142 received the same
document at 2.0 ms and fired the load event at 22.1 ms, so most of the guest's
load time is the guest's network path to the host through SmolVM's relay, which
every script and asset of the site takes.

### Resources (7 boots, one VM per boot; minimum to maximum)

| Measure | `helium` |
| --- | --- |
| VMM physical footprint, 30 s after ready (VMM and encoder helper) | 457.9 MiB (433.9 to 462.9) |
| VMM RSS, same moment (not gated) | 1318 MiB (1116 to 1418) |
| VMM RSS after 60 s more idle | 1147 MiB (1007 to 1165) |
| VMM idle CPU, 60 s window | 3.60 percent of a core (2.53 to 3.63) |
| Machine data directory on disk | 682.7 MiB (682.7 to 682.7) |
| Profile directory on disk | 21.7 MiB (3.1 to 22.0) |
| Image archive | 218,243,072 bytes |

| Measure | Value |
| --- | --- |
| `bctl` release binary (embeds the dashboard) | 5,161,552 bytes |
| Daemon RSS, no session | 4.08 MiB (4.05 to 4.11) |
| Daemon RSS, one ready session | 12.6 MiB (12.6 to 12.7) |
| Daemon idle CPU, one session, 60 s windows | 0.03 percent of a core (0.02 to 0.03) |

The `bctl` binary is 0.7 percent under its 5,200,000-byte gate.

## Results on Nanos

The same suites on `nanos-helium` under hydrogen's Nanos, on Linux amd64 with
QEMU and on the Mac with libkrun, beside the SmolVM numbers above. The SmolVM
column is the M5 Max, headed; the Nanos columns are headless and use other
VMMs, and the Linux column is another machine, so a row compares the products
as shipped, not the VMMs alone. Nanos has no pause, resume, VNC or
input-to-pixel, and the viewer suite does not apply. The Mac Nanos column ran
lifecycle, resources and browser only.

| | Linux | Mac |
| --- | --- | --- |
| Host | MS-A1, AMD Ryzen 9 9950X, 32 CPUs, 60 GiB, Alpine Linux edge (7.2.9) | Mac17,7, Apple M5 Max, 18 CPUs, 48 GiB, macOS 27.0.1 (26A434) |
| Runtime | hydrogen `bea1e222`, QEMU 11.1.2 with KVM, vhost-device-vsock 0.3.0 with the half-close patch | hydrogen `bbeea356` (both CPUs online), libkrun on Hypervisor.framework (`nanos-krun`) |
| Guest | `nanos-helium` (Helium 0.20.3.1 r5), 2 CPUs, 4096 MiB, headless | `nanos-helium` arm64 (Helium 0.20.3.1), 2 CPUs, 4096 MiB, headless |
| Tree | main `602bfde` (uBlock Origin disabled), release build | main `6500b15`, release build |
| Quiet host | every sample passed | every time sample passed at 0.25 load per core; 2 of 7 idle CPU windows overlapped load and were discarded |

| Measure | Nanos, Linux | Nanos, Mac | SmolVM, Mac |
| --- | --- | --- | --- |
| Cold `up` to ready | 938 ms (904 to 1043) | 584 ms (574 to 611) | 1466 ms |
| Warm `up` to ready | 982 ms (931 to 990) | 561 ms (554 to 579) | 1455 ms |
| `down` | 164 ms (109 to 165) | 289 ms (259 to 376) | 112.8 ms |
| Daemon start | 100.7 ms | 113.6 ms | 114.8 ms |
| TodoMVC workflow | 144 ms (120 to 283) | not run | 277 ms |
| `goto`, `fill`, `press`, `check`, `snapshot` p50 | 38.8, 4.2, 4.3, 41.9, 3.5 ms | not run | 39.5, 14.3, 15.4, 65.7, 11.9 ms |
| `Runtime.evaluate` p50 direct / gateway | 281 / 289 us | not run | 170 / 256 us |
| Speedometer 3.1 score (7 of 7 runs) | 28.2 (26.8 to 28.7) | 38.6 (38.3 to 39.7) | 31.4 |
| News site `responseEnd` / `loadEventEnd` / LCP (35 loads) | 3.9 / 82.4 / 152 ms | 4.0 / 64.2 / 112 ms | 14.2 / 71.9 / 140 ms |
| VMM memory 30 s after ready | 515 MiB RSS (513 to 519) | 295 MiB footprint (294 to 296); 758 MiB RSS | 458 MiB footprint |
| VMM idle CPU | 0.30 percent of a core (0.27 to 0.32) | 0.48 percent (0.48 to 0.53) | 3.6 |
| Machine disk | 3.8 MiB (3.1 to 3.8), qcow2 overlay | 636 MiB, an APFS clone of the image that `du` counts in full | 683 MiB |
| Profile disk, before browsing | — | 3.9 MiB (3.8 to 4.5) | — |
| Daemon RSS, one session | 5.0 MiB | 4.8 MiB (4.7 to 5.0) | 12.6 MiB |

- **The Mac column was rerun on 2026-11-11 with both guest CPUs online.**
  The earlier run had only one: an AP took a spinlock before enabling its
  MMU, and under Hypervisor.framework an exclusive access to non-cacheable
  memory never succeeds, so the AP spun and the kernel gave up on it.
  Hydrogen `bbeea356` starts APs one at a time without that lock. Compared
  with the one-CPU run:
  - Speedometer rose from 28.4 to 38.6;
  - cold `up` fell from 2728 to 584 ms, its first probe from 2371 to 438 ms;
  - idle VMM CPU fell from 100 to 0.48 percent of a core.

  The SmolVM column is from the earlier run on this host.
- **Cold and warm `up` on the Mac now differ by 23 ms** (first probe 438 ms
  cold, 421 ms warm). With one CPU online, cold was 1.4 s slower, all before
  the guest answered.
- **A new Mac profile filled its 2 GiB sparse template.** The supervisor
  copied the template byte by byte, writing every hole: 2062 MiB per profile.
  It now clones the template, and a profile costs 3.9 MiB until the browser
  writes to it.
- **Four of the five seconds of `up` were uBlock Origin, so sessions now
  start with it disabled.** Helium bundles uBlock Origin 1.75.0 as an
  extension the user may disable, and on a profile it has never seen, uBlock
  downloads nothing but compiles the filter lists it bundles (EasyList, EasyPrivacy and uBlock's
  own among them) into its filtering engine, in JavaScript on its background
  page. Until that finishes, it holds every request a tab
  makes and then reloads the tabs it held, which is why a `filters` ready
  stage existed. The compile took 2.5 to 2.9 s natively on the Ryzen
  host and about 4 s in the 2-CPU guest, and it is paid every boot because a
  Nanos profile does not outlive its session. It also cost memory: Nanos does
  not return freed pages to QEMU, so the compile's peak stayed in the VMM's
  RSS. A test image carrying uBlock's compiled lists from a host run idled at
  855 to 862 MiB 30 s after ready, against 1715 to 1752 MiB for the normal
  image (two boots each). Seeding those lists into the image did not work:
  uBlock rejected the copied cache ("Filtering engine selfie not available"),
  even in place on the host, and the cause was not found.
  `bctl-guest browser` now writes the extension as disabled by the user into
  a profile's `Default/Preferences` before the browser first opens it, and
  the `filters` stage is gone. Neither `--disable-extensions` nor
  `--disable-component-extensions-with-background-pages` stops Helium loading
  it. Content blocking without that cost would need an engine whose compiled
  lists load without a per-profile compile, such as Brave's `adblock-rs`
  (Rust, with a serialized engine format) built into Helium; that is a Helium
  change, not a bctl one. Helium has an experimental integration on Chromium
  155; chromium-build's `docs/helium-adblock-rust.md` (CHB-9) audits
  forward-porting it to our 156 pin. Sessions block no ads until then.
- **The Nanos VMM's RSS now shrinks when the guest frees memory.** QEMU's
  memfd backing kept every page the guest had ever touched. Since hydrogen
  `868726ad` the guest reports free 2 MiB blocks through virtio-balloon free
  page reporting once 32 MiB has been freed. A tab that touched 768 MiB took
  the VMM from 504 to 1281 MiB, and 5 s after the tab closed it was at
  533 MiB; without reporting it stayed at 1291 MiB. Idle RSS (514 MiB) and
  idle CPU (0.32 percent of a core) are unchanged, because an idle session
  frees nothing. A first version that reported every 5 s regardless cost
  0.42 percent.
- **Before uBlock was disabled (`5dd8a92`)** the same suites measured cold
  `up` 5193 ms (CDP answered at 1043 ms), Speedometer 24.1, news site
  `loadEventEnd` 148 ms and LCP 232 ms, VMM RSS 1696 MiB, idle CPU 7.6
  percent of a core and a 205 MiB disk overlay. Its background page was most
  of the idle CPU and the overlay as well as the compile.
- **The first-load stalls were a guest kernel deadlock (fixed in hydrogen
  `bea1e222`).** About one boot in ten froze both vCPUs during the first
  page load: a unix socket reader took a copy-on-write fault and waited in
  a TLB flush rendezvous while the other vCPU spun on that socket's lock
  with interrupts off. CDP, the guest check and egress all stopped, so
  Speedometer stalled at 0 tests, a CDP connection died with a broken pipe,
  or `bctl up` failed at the guest stage. gdb on a stalled no-KASLR kernel showed
  both stacks; hydrogen's `unix_cow_read` case reproduces it, and 80
  boots after the fix showed no stall. All 7 Speedometer runs above
  finished, the first one after `up` included.
- **About 1 in 1,500 fresh egress connections still fails** with
  `ERR_SOCKS_CONNECTION_FAILED` when Helium opens several at once (2 of
  2,880 parallel fetches, before and after the deadlock fix). The host proxy
  accepts the connection and reads end of stream before the first byte of
  the SOCKS greeting, while the guest relay has read Chrome's greeting and
  written it to vsock. Rotating the guest's vsock ports did not change the
  rate. Open.
- **The page actions were 300 to 1000 ms before hydrogen `2c46223e`.** A
  message one byte past a full 1460-byte segment on the guest's loopback TCP
  waited for a delayed ACK that a timer queued and nothing delivered. Every
  locator query crossed that path through the guest's CDP relay.

## External clients

`BCTL_INTEROP_SUITE=measure tests/interop/run.sh` (`tests/interop/measure.py`)
times each pinned client against a ready session, 7 rounds, with the clients
alternating. Times are host wall time in ms, as the client's user sees them:

- agent-browser's include starting its CLI.
- browser-use's are timed inside its REPL cells, except the attach (from
  `Agent.run` to the end of its first cell) and the whole run.

The page is the fixture's `/form`. bctl was `b3d419a` plus the E5 changes, a
debug build, on the host above. `perf-gate-ready` passed before and after each
runtime's run.

| Client | Operation | SmolVM median | p95 | Nanos (libkrun) median | p95 |
|--------|-----------|------:|----:|------:|----:|
| browser-use | attach, first cell run | 307.9 | 311.1 | 313.0 | 323.4 |
| browser-use | navigate | 18.4 | 27.9 | 14.1 | 334.1 |
| browser-use | accessibility tree | 5.1 | 6.8 | 1.6 | 12.7 |
| browser-use | screenshot | 43.2 | 55.6 | 43.7 | 50.1 |
| browser-use | run, start to finish | 396.7 | 423.2 | 398.3 | 748.4 |
| agent-browser | attach (first `eval`) | 136.2 | 142.3 | 132.5 | 140.3 |
| agent-browser | navigate (`open`) | 30.1 | 42.0 | 29.4 | 41.1 |
| agent-browser | accessibility tree (`snapshot`) | 16.2 | 20.1 | 14.7 | 21.3 |
| agent-browser | screenshot | 42.2 | 55.4 | 39.0 | 50.0 |
| agent-browser | close | 11.4 | 12.1 | 11.6 | 13.8 |

Memory, in MiB:

- browser-use's agent process peaked at 94.4 (SmolVM) and 94.6 (Nanos). That
  figure leaves out browser-harness's own daemon.
- agent-browser's daemon held 13.3 and 13.2.

With 7 samples, p95 is the largest sample. The first Nanos round, the
session's first navigation, made the browser-use navigate p95 334 ms and its run
p95 748 ms; the other six stayed near the median. Neither client reports CDP call or byte counts, and the
gateway does not count them, so those are not measured. Linux x86_64 is not
measured.

## Findings

- **The first action after a navigation waited out a 200 ms quiet interval
  (fixed in `759b308`).** An action that sends input first checks that the page
  has its first contentful paint, and between checks it waits for a DOM
  mutation or a layout or navigation event, at most 200 ms. A paint is neither,
  so a check that ran a few milliseconds before the paint waited the full
  200 ms. Re-navigating a plain form to its own URL hit it in 9 of 10 fills.
  The wait now also ends at that paint;
  `n7_an_action_right_after_a_navigation_starts_when_the_page_first_paints`
  keeps it.
- **Input to pixel switches between about 9 and about 22 ms.** Seen within one
  run as well as between runs, with the same build and image. The fast mode
  appeared on a busier host and the slow one on an idle host, which points at
  host scheduling or power state rather than the guest, but that is not
  established. One candidate is macOS timer coalescing, which defers a
  default-QoS process's timers by up to 5 ms each when the host is idle
  (`kern.timer_coalesce_tier1_ns_max`). Setting the VMM's latency tier with
  `taskpolicy -l N -p PID` changed nothing on a loaded host (load 5 to 14):
  tiers 0 and 5 both gave 8.7 to 12.5 ms blocks, and tier 3 gave 10.9 to 15.3
  ms in a single run. Under load timers fire early anyway, so the test needs an
  idle host: tier 0 against the default, interleaved, while the slow mode shows.
- **Host connections to the host's own LAN address stall; the guest was not at
  fault.** The h1 guests reached the benchmark server at the host's LAN address
  (`10.0.1.28`), so SmolVM's relay dialed that address on the host. On this host
  a macOS socket content filter is active (`sysctl net.cfil.active_count` is 2;
  CrowdStrike Falcon and Tailscale are installed), and it attaches to every TCP
  connection to the LAN address and to none to `127.0.0.1`
  (`net.cfil.sock_attached_count` grows by 2 per connection). Such connections
  stop mid-response: in an inspected stall the server sat in `sendto` with 134
  KiB of a 286 KiB module script unsent while the kernel reported the
  connection's window open and nothing queued, SmolVM's relay had forwarded every
  byte it received, and the guest had acknowledged all of it. Chromium waits
  forever for the rest, which is the stalled module script with no resource
  timing entry. The same stall needs no VM: six keep-alive Python clients on the
  host fetching the Speedometer tree from `bctl-bench serve` for 60 s stalled 19
  times (10 s timeouts) through `10.0.1.28` and never through `127.0.0.1`
  (276,272 requests). Through SmolVM's gateway, which the relay dials as
  `127.0.0.1`, Speedometer finished 24 of 24 runs in the guest; through the LAN
  address it stalled in 7 of 24. The container never stalled because Docker
  reaches the host by loopback too. The browser suite now uses the gateway, and
  `crates/bctl/tests/host_http.rs` keeps the guest-to-host keep-alive load as a
  VM test. Which filter stalls, and whether it stalls connections to other LAN
  hosts, is not established.
- **SmolVM refuses guest TCP connections beyond 512 tracked ones.** Its relay
  keeps every guest connection in a 512-entry table until smoltcp closes it, and
  a connection whose host side closes first holds its entry through smoltcp's
  10 s TIME-WAIT. A guest opening more than about 50 such connections per second
  (for example HTTP/1.0 or `Connection: close` requests) gets `Connection
  refused`; 200 concurrent `wget`s per round from the guest failed 180 of 200 by
  the fourth round, and the VMM log reads `relay table is full active=512`.
  Chromium's keep-alive pool stays far below the limit (9 connections for 3,660
  module requests). This is SmolVM's, not patched here.
- **The input lease dies with the WebSocket.** Before the authority change, a viewer
  that reconnected to the gateway while its previous connection was still closing
  could lose the lease it had just taken, because the old connection's teardown
  released the viewer's authority. The harness waits 1 s between gateway viewers and
  requires `authority: human` after taking control. The race was not re-tested
  after the authority change.
- **macOS RSS misreports a VM.** `ps` RSS excludes compressed pages: one VM read
  158 MiB where others read 1.3 GiB. The footprint is the measure to gate.

## Gates

`bctl-bench gate` reads `tests/bench/gates.json` and the latest result of each
suite. A `time` gate applies only to a measure whose status is `clean` and is
reported `deferred` otherwise, never `pass`; `memory` and `size` gates always
apply. The gate compares the measure's own statistic (the median, or the p95 for
`.p95` measures) with its limit. Limits are set from the worst observed sample:
latency and CPU limits sit 20 to 80 percent above the largest sample, score
floors at 80 percent of the smallest run, memory and size limits 5 to 50 percent
above the largest observation. For a p95 measure the largest sample is the
largest block p95, because one call in hundreds can be an order of magnitude
slower than the rest.

All 50 gates in the file pass on the latest results. The lifecycle, interaction,
browser, and helium resource limits sit 50 percent above the largest round or
block, the Speedometer floor at 80 percent of the smallest run (24.7), and the
footprint, disk, and daemon limits are unchanged or 30 percent above the largest
boot. The 30 `viewer` gates were set from an earlier 5-round Helium run: fps and
`fps.worst_second` floors at 80 percent of the smallest round, latency p95
limits 20 percent above the largest raw sample, bandwidth limits 50 percent
above the largest round, and `missed` at 5 percent.
Log, artifact, and profile-disk bounds are not set: the profile directory grew by
94 MiB in 1 of 10 first-boot samples earlier in the session, which a bound has to
allow for.

## Not covered yet

- CDP round trip inside the guest is not measured. `direct` skips only the
  gateway.
- JetStream and MotionMark: Speedometer only.
- Checkpoint restore: `bctl-bench restore` times it, but there is no CLI for it
  yet, so it has no baseline here.
- Run and takeover measures follow M3.
- Allocation and transport counters for the automation workflow. When
  `crates/cdp` exposes its counters (commands started, completed, timed out,
  cancelled; pending high-water; bytes and messages each way; largest message;
  parse time; late responses; subscription overflows), read one snapshot per
  workflow and push each counter into `Outcome.measures` under
  `interaction.automation.cdp.<counter>`.
