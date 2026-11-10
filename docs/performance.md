# Performance

`bctl-bench` (`crates/bench`) runs named suites against real appliance VMs and
writes one dated JSON result per run to `tests/bench/results/`. This page holds
the method, the latest results with their spread and conditions, the gates, and
what is deferred. Gates live in `tests/bench/gates.json`.

Nothing here is a claim about unikernels, other VMMs, or other products. External
numbers are targets, never measurements of bctl.

## Running a suite

Prerequisites: `runtime/build.sh`, `BCTL_BROWSER_ARCHIVE=DIR images/build.sh helium`
(Helium r6 or later), Docker, a Speedometer checkout (below), and a release build:

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

The interaction and browser suites load pages from a Speedometer checkout, and
the browser suite builds its own container image:

- Speedometer 3.1: `git clone --depth 1 -b release/3.1 https://github.com/WebKit/Speedometer .work/dl/speedometer/src`
  (commit `1386415be8fef2f6b6bbdbe1828872471c5d802a`). It is served locally to
  every browser, so no run depends on the public site. Besides the benchmark
  itself, the interaction suite drives its React TodoMVC build and the browser
  suite loads its `news-next` site.
- The container is `tests/bench/chromium-container`: Alpine 3.22's `chromium`
  package, Xvfb 1280x800, sandbox on, limited to 2 CPUs and 4 GiB like the guest.

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
  blocks and flips the order each round (gateway and direct), or rotates the start
  of the variant order each round (Speedometer in the guest and the container).
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
- The container reaches the host benchmark server as `host.docker.internal`; the
  guest reaches it at `100.96.0.1`, SmolVM's gateway to the host loopback,
  through `--allow-private 100.96.0.0/30`. Both paths add their own network
  stack to the page load time.
- The guest runs Helium and the container runs Alpine's Chromium 142, so the
  browser comparison is the whole stack, not the VM alone.
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
| Container | Chromium 142.0.7444.59 (Alpine 3.22 `chromium`), Linux arm64 on OrbStack, `--cpus=2 --memory=4g`, Xvfb 1280x800, renderer `NoNewPrivs: 1`, `Seccomp: 2` |
| Tree | main `4cd94c1` with the suite changes uncommitted; `bctl` release build |
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

### Interaction (7 rounds, gateway and direct alternating)

TodoMVC workflow through `bctl page`, one per round:

| Step | p50 | p95 | Samples |
| --- | --- | --- | --- |
| `goto` with `--wait load` | 39.9 ms | 71.7 ms | 7 |
| `fill` | 14.4 ms | 216.0 ms | 35 |
| `press Enter` | 14.1 ms | 26.0 ms | 35 |
| `check` | 65.8 ms | 69.5 ms | 7 |
| `snapshot` | 11.5 ms | 15.2 ms | 7 |
| Whole workflow | 450.6 ms (263 to 485) | | 7 |

The workflow is bimodal: 263 to 307 ms in 3 rounds and 451 to 485 ms in 4. The
difference is the first `fill` after `goto`, which took 215 to 222 ms in those 4
rounds and 13 to 21 ms otherwise; later fills took 11 to 22 ms.

CDP round trip, one call at a time, 700 calls per cell. `direct` is the exposed
Unix socket, which bypasses the gateway only; both include the guest relay and
the browser.

| Call | Path | p50 | p95 | Block medians |
| --- | --- | --- | --- | --- |
| `Runtime.evaluate("1")` | direct | 170 us | 189 us | 134 to 175 us |
| `Runtime.evaluate("1")` | gateway | 243 us | 285 us | 219 to 251 us |

The gateway adds 73 us at the median, and the block medians of the two paths do
not overlap.

Input to pixel: an RFB pointer press or key tap to the update that repaints the
probe pixel, 140 trials per cell (7 blocks of 20). The page counted 560 toggles for
560 trials sent; none timed out.

| Input | Path | p50 | p95 | Block medians |
| --- | --- | --- | --- | --- |
| pointer | direct | 21.7 ms | 23.9 ms | 18.8 to 22.6 ms |
| pointer | gateway | 23.6 ms | 25.8 ms | 22.3 to 24.5 ms |
| key | direct | 21.6 ms | 23.9 ms | 19.0 to 22.6 ms |
| key | gateway | 23.4 ms | 25.1 ms | 22.4 to 24.0 ms |

The gateway adds about 2 ms at the median. Helium presents the change between
one and two 60 Hz frames after the input; the Alpine Chromium reference image on
an earlier tree presented it in 6.7 to 7.7 ms, inside one frame. Which part of
the Helium image adds the frame was not investigated.

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

### Browser (Speedometer 3.1, headed, `iterationCount=3`, 5 rounds, order rotated)

| Variant | Runs that finished | Score, median (min to max) |
| --- | --- | --- |
| Guest, `helium` | 5 of 5 | 31.35 (30.91 to 32.22) |
| Container, Linux arm64 | 5 of 5 | 28.78 (27.86 to 29.22) |

The score is the mean of the iteration scores of one run; the spread is across
runs. The guest scores 9 percent above the container and the ranges do not
overlap, but the two run different browsers (Helium 0.20.3.1 and Chromium 142),
so this does not compare the VM with the container alone.

Page load of the `news-next` site, 25 loads per variant (5 batches of 5), median
(minimum to maximum):

| Variant | `responseEnd` | `loadEventEnd` | Largest contentful paint |
| --- | --- | --- | --- |
| Guest, `helium` | 14.7 ms (5.0 to 25.4) | 72.6 ms (52.2 to 107.3) | 140 ms (92 to 160) |
| Container, Linux arm64 | 2.0 ms (1.6 to 5.8) | 22.1 ms (20.6 to 43.4) | 88 ms (76 to 124) |

The guest receives the document 12.7 ms later at the median, which is the guest's
network path to the host through SmolVM's relay; the gap grows to 50 ms by the
load event, because the site's scripts and assets take the same path. Largest
contentful paint differs by 52 ms.

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

## Findings

- **The first action after a navigation sometimes waits about 200 ms.** In 4 of
  7 TodoMVC rounds the first `fill` after `goto` took 215 to 222 ms against 11 to
  22 ms for every other fill, which makes the workflow bimodal. The step matches
  the automation's 200 ms quiet wait (`MAX_QUIET` in `crates/automation`), as if
  the action's first check missed the field and then waited for a page change
  that had already happened; this was not confirmed.
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
