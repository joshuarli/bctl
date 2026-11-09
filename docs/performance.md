# Performance

`bctl-bench` (`crates/bench`) runs named suites against real appliance VMs and
writes one dated JSON result per run to `tests/bench/results/`. This page holds
the method, the latest results with their spread and conditions, the gates, and
what is deferred. Gates live in `tests/bench/gates.json`.

Nothing here is a claim about unikernels, other VMMs, or other products. External
numbers are targets, never measurements of bctl.

## Running a suite

Prerequisites: `runtime/build.sh`, `images/build.sh ref-alpine-chromium`,
`images/build.sh ref-alpine-tinyland`, Docker, and a release build:

```sh
cargo build --release -p bctl -p bctl-bench
target/release/bctl-bench lifecycle   [--rounds N] [--quiet-wait SECS] [--out DIR]
target/release/bctl-bench interaction [--rounds N] [--quiet-wait SECS] [--out DIR]
target/release/bctl-bench browser     [--rounds N] [--iterations N] [--quiet-wait SECS] [--out DIR]
target/release/bctl-bench resources   [--rounds N] [--quiet-wait SECS] [--out DIR]
target/release/bctl-bench gate [RESULT.json...]
target/release/bctl-bench serve
```

Every suite creates its own `BCTL_HOME` under `.work/tmp` with its own daemon and
names sessions `h1-...`, so it never touches the user's daemon. A suite that is
killed leaves its machines behind; stop them with `bctl down` under that home.
`serve` hosts the pinned Speedometer and the page-load fixture for a manual run.

The browser suite needs a Speedometer checkout and builds its own container
image:

- Speedometer 3.1: `git clone --depth 1 -b release/3.1 https://github.com/WebKit/Speedometer .work/dl/speedometer/src`
  (commit `1386415be8fef2f6b6bbdbe1828872471c5d802a`). It is served locally to
  every browser, so no run depends on the public site.
- The container is `tests/bench/chromium-container`: Alpine 3.22 (same digest as
  the reference images), the same `chromium` package, Xvfb 1280x800, sandbox on, limited to 2 CPUs and
  4 GiB like the guest.

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
  of the variant order each round (Speedometer on each image and the container).
- **Result file.** `suite`, `started` (UTC), `host` (model, chip, CPUs, memory,
  macOS), `commit` and `dirty`, `smolvm`, `images` (tar size and config digest),
  `browser_versions`, `measures`, and `notes`.

### What each suite times

| Suite | Timed region | Source of truth |
| --- | --- | --- |
| `lifecycle` | `bctl up` from invocation to exit (it blocks until ready), `bctl down`, daemon start (first command on an empty home to answer). Stage timeline: a poller runs `bctl status` every 20 ms and records when `stage` first names each stage. | CLI wall time |
| `lifecycle` pause | `smolvm machine pause` and `resume` wall time on a daemon-owned machine, then on a bare machine | smolvm CLI |
| `browser` | Speedometer 3.1 `Score` per iteration from `benchmarkClient._metrics`, headed, `iterationCount` per run (default 5), one fresh page load per run; fixture load from the Navigation Timing API of the fixture page, 5 loads per batch, cache disabled | page JS |
| `interaction` CDP | one sequential `Browser.getVersion` or `Runtime.evaluate` at a time on one WebSocket, 10 warm-up calls, blocks of 100 | client wall time |
| `interaction` input | RFB pointer press or key tap written, until an incremental update repaints a probe pixel of a full-viewport color toggle; 400 ms between trials | client wall time |
| `resources` | `ps` RSS and CPU time, `footprint` for the macOS physical footprint, `du` for disk | host tools |

Checks that the timed work happened: a lifecycle sample requires the session to
be `ready`; a Speedometer run requires `iterationCount` scores; a fixture load
requires `loadEventEnd` for the load's own URL; the input suite reads the page's
own toggle counter over CDP afterwards and reports it against the trials sent.

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
- The container reaches the host benchmark server as `host.docker.internal`; the
  guest reaches it at `100.96.0.1`, SmolVM's gateway to the host loopback,
  through `--allow-private 100.96.0.0/30`. Both paths add their own network
  stack to the fixture load time.

## Conditions of the latest results

| | |
| --- | --- |
| Host | Mac17,7, Apple M5 Max, 18 CPUs, 48 GiB, macOS 27.0.1 (26A434) |
| SmolVM | 1.31.4 |
| Guests | `ref-alpine-chromium` (config `0213b5c8...`, tar 305,999,872 bytes), `ref-alpine-tinyland` (config `d3a9543c...`, tar 301,190,144 bytes); 2 CPUs, 4096 MiB, 1280x800 |
| Browser | Chromium 142.0.7444.59 (Alpine 3.22 `chromium`) in both guests and the container |
| Container | Linux arm64 on OrbStack, `--cpus=2 --memory=4g`, Xvfb 1280x800, renderer `NoNewPrivs: 1`, `Seccomp: 2` |
| Tree | `lane/h1` on main `117562a`; `bctl` release build |
| Load | The host shared its cores with a Chromium build in Docker (up to 17 cores). The latest results were taken while the one-minute load average was 9.6 to 14.7 against the gate's limit of 12.6, so the gate admitted them but the host was not idle. |

An earlier pass of the same suites, taken at a load average of 4 to 9 on a tree
from before the authority change, measured the direct `Browser.getVersion` CDP
median at 142 us where the pass below measured 219 us. Its files were not kept,
because a harness fix (lease retake) and the rebase made them unrepeatable. The
difference says the default gate admits windows that differ by 50 percent. For a
baseline that will be compared later, set `PERF_GATE_MAX_LOAD` low, for example
`PERF_GATE_MAX_LOAD=0.25`, so `perf-gate-ready` admits only a near-idle host.

## Results

All time results passed the quiet-host check before and after each sample;
discarded samples are in the files. "Block" is one alternation unit; the block
medians show run-to-run spread where samples within a block are not independent.

### Lifecycle (`ref-alpine-chromium`, 7 rounds, median and minimum to maximum)

| Measure | Result |
| --- | --- |
| Daemon start (first command on an empty home) | 109.8 ms (106.9 to 111.8) |
| Cold `up` to ready | 2101 ms (1964 to 2485; quartiles 2051, 2313) |
| Warm `up` to ready (same session and profile after `down`) | 1783 ms (1764 to 2116) |
| `down` | 304.9 ms (301.2 to 329.5), 14 samples |
| Cold `up`: request to first failed readiness probe | 1041 ms (1023 to 1096) |
| Cold `up`: first failed probe to ready | 1063 ms (925 to 1442) |
| Warm `up`: request to first failed readiness probe | 1030 ms (1010 to 1055) |
| Warm `up`: first failed probe to ready | 754 ms (742 to 1086) |

Stage timeline: the daemon named `compositor` as the first missing stage in 7 of
7 cold and 7 of 7 warm `up`s, at the time of the first failed probe. It named
`browser` in 3 of 7 cold and 1 of 7 warm, around 1.4 s, and `display` in 2 of 7
cold, around 1.8 s. It never named `machine`, `guest`, `seat`, `sandbox`, or
`cdp`: they passed between its probes. So VM create, start, boot, guest agent,
and seat take about 1.0 s together; compositor to browser to display take the
remaining 0.8 to 1.0 s. A finer split needs a finer probe in the daemon, not a
faster poll here.

Cold and warm differ by 0.3 s at the median, but their ranges overlap at the upper end
(cold 2485, warm 2116), so the cold penalty is small and not established beyond
the median.

Pause and resume on a bare machine (no image, no GPU, no socket; 4096 MiB, 2 CPUs,
idle), 7 rounds: pause 574 ms (539 to 827), resume 634 ms (497 to 662); a guest
process kept its pid and its counter kept advancing in 7 of 7 rounds. This is a
reference for SmolVM, not a measure of the appliance, which cannot be paused (see
Findings).

### Interaction (`ref-alpine-chromium`, 7 blocks per path, gateway and direct alternating)

CDP round trip, one call at a time, 700 calls per cell. `direct` is the exposed
Unix socket, which bypasses the gateway only; both include the guest relay and
Chromium.

| Call | Path | p50 | p95 | Block medians |
| --- | --- | --- | --- | --- |
| `Browser.getVersion` | direct | 219 us | 320 us | 200 to 267 us |
| `Browser.getVersion` | gateway | 294 us | 434 us | 265 to 327 us |
| `Runtime.evaluate("1")` | direct | 399 us | 560 us | 353 to 473 us |
| `Runtime.evaluate("1")` | gateway | 488 us | 658 us | 455 to 514 us |

The gateway adds 75 us (`getVersion`) and 89 us (`evaluate`) at the median, and
its block median was higher than direct in 7 of 7 paired rounds (differences 50 to
99 us and 11 to 137 us).

Input to pixel: an RFB pointer press or key tap to the update that repaints the
probe pixel, 140 trials per cell (7 blocks of 20). The page counted 560 toggles for
560 trials sent; none timed out.

| Input | Path | p50 | p95 | Block medians |
| --- | --- | --- | --- | --- |
| pointer | direct | 6.72 ms | 8.94 ms | 6.28 to 8.43 ms |
| pointer | gateway | 7.41 ms | 10.07 ms | 6.96 to 9.54 ms |
| key | direct | 6.82 ms | 8.78 ms | 6.33 to 8.22 ms |
| key | gateway | 7.68 ms | 10.00 ms | 7.33 to 9.57 ms |

The gateway adds about 0.7 to 0.9 ms at the median and was slower in 6 of 7
paired rounds (differences from -0.97 to +2.43 ms), so its cost is real but within
the block-to-block spread. Medians of 6.7 to 7.7 ms are below one 60 Hz frame (16.7 ms). The toggle count
confirms every trial moved the pixel it timed; why the pipeline presents inside a
frame was not investigated.

### Browser (Speedometer 3.1, headed, `iterationCount=5`, 8 rounds, order rotated)

| Variant | Runs that finished | Score, median (min to max) | Fixture load, `loadEventEnd` median (min to max) |
| --- | --- | --- | --- |
| Container, Linux arm64 | 8 of 8 | 23.22 (21.95 to 23.70) | 60.7 ms (43.0 to 102.8), 40 loads |
| Guest, `ref-alpine-chromium` | 5 of 8 | 18.78 (17.98 to 18.86) | 96.1 ms (66.8 to 134.2), 40 loads |
| Guest, `ref-alpine-tinyland` | 4 of 8 | 16.79 (16.65 to 17.23), unclean | 100.7 ms (75.1 to 139.1), 35 loads |

The score is the mean of the 5 iteration scores of one run; the spread is across
runs. Tinyland has 4 clean runs, fewer than the 5 required, so its score has no
gate. Against the container the guest scores 81 percent (Sway image) and 72
percent (Tinyland image) at the median, and the ranges do not overlap. Tinyland
scores 11 percent below the Sway image, with non-overlapping ranges and 4 and 5
runs. The fixture loads 1.6 times slower in the guest, which includes the guest's
network path to the host.

The guest was not tuned toward the container and the container was not tuned
toward the guest beyond matching CPUs and memory: the guest software-renders
through llvmpipe on 2 vCPUs, the container runs Xvfb with no compositor. The
limiter of the gap was not profiled, so the cause (CPU, rendering, memory, or the
virtual display) is open.

Stalls: 7 of 16 guest runs (3 of 8 on the Sway image, 4 of 8 on the Tinyland image)
stopped making progress and were aborted after 120 s; the container had none.
They stopped in different workloads (`observable-plot`, `angular-complex`,
`perf-dashboard`, `react-stockcharts`, `chartjs`, `tiptap`), so no workload is
at fault. These runs reached the server at the host's LAN address, which stalls
on this host without any VM; see Findings.

### Resources (5 boots per image, one VM per boot; minimum to maximum)

| Measure | ref-alpine-chromium | ref-alpine-tinyland |
| --- | --- | --- |
| VMM physical footprint, 30 s after ready (VMM and encoder helper) | 644.0 MiB (642.0 to 645.0) | 642.9 MiB (640.9 to 645.9) |
| VMM RSS, same moment (not gated) | 1388 MiB (1382 to 1399) | 1342 MiB (1340 to 1349) |
| VMM RSS after 60 s more idle | 1533 MiB (1498 to 1580) | 1500 MiB (1477 to 1549) |
| VMM idle CPU, 60 s window | 5.48 percent of a core (5.40 to 5.52) | 5.53 percent (5.47 to 5.57) |
| Machine data directory on disk | 792.3 MiB (791.4 to 792.3) | 785.9 MiB (785.4 to 785.9) |
| Image archive | 305,999,872 bytes | 301,190,144 bytes |

| Measure | Value |
| --- | --- |
| `bctl` release binary (embeds the dashboard) | 4,967,168 bytes |
| Daemon RSS, no session, 10 samples | 3.98 MiB (3.92 to 4.00) |
| Daemon RSS, one ready session, 10 samples | 12.4 MiB (12.4 to 12.5) |
| Daemon idle CPU, one session, 60 s windows | below 0.02 percent of a core (`ps` resolution) |

Both images carry the same Chromium; their footprints differ by less than the
spread. An idle appliance spends about 5.5 percent of a core in the VMM, the same on both
images. The cause was not profiled.

## Findings

- **The appliance cannot be paused.** `smolvm machine pause` on a daemon-owned
  session fails with `the initial portable checkpoint profile does not support
  published sockets, host-backed image layers, Vulkan GPU state`: the exposed CDP
  socket, the `--image` layers, and `--gpu`, all three requirements of the product
  launch. The session stays `ready`. A bare machine pauses and resumes (above), so
  this is a property of the launch configuration, not of SmolVM. Whether the tab,
  the JS heap, and CDP survive a resume cannot be asked until the launch
  configuration can be paused. Checkpoint restore (X1) will meet the same refusal.
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
- **Guest and container run the same browser build.** Both report Chromium
  142.0.7444.59 and both renderers run with the sandbox on, so the score gap is the
  VM and the guest stack, not the browser or its sandbox.

## Gates

`bctl-bench gate` reads `tests/bench/gates.json` and the latest result of each
suite. A `time` gate applies only to a measure whose status is `clean` and is
reported `deferred` otherwise, never `pass`; `memory` and `size` gates always
apply. The gate compares the measure's own statistic (the median, or the p95 for
`.p95` measures) with its limit. Limits are set from the worst observed sample:
latency and CPU limits sit 20 to 80 percent above the largest sample, score
floors at 80 percent of the smallest run, memory and size limits 5 to 50 percent
above the largest observation. The 50 percent spread between load windows seen
above is inside those margins.

21 of the 22 gates in the file pass on the latest results; the 22nd,
`browser.speedometer.guest-tinyland.score`, is deferred (4 clean runs).
Log, artifact, and profile-disk bounds are not set: the profile directory grew by
94 MiB in 1 of 10 first-boot samples earlier in the session, which a bound has to
allow for.

## Not covered yet

- Viewer frame rate and bandwidth, and CDP round trip inside the guest, are not
  measured. `direct` skips only the gateway.
- JetStream and MotionMark: Speedometer only.
- Helium: the suites take an image name and report the browser version, so P2
  reruns them unchanged on the arm64 Helium image.
- Checkpoint restore: blocked by the pause refusal above.
- Run and takeover measures follow M3.
- **Transport counters.** `crates/cdp` is being changed in another lane and the
  bench depends on none of it. When its counters (commands started, completed,
  timed out, cancelled; pending high-water; bytes and messages each way; largest
  message; parse time; late responses; subscription overflows) exist, add an
  `automation` suite next to `interaction`: drive the fixture through `crates/cdp`,
  read one counter snapshot per run, and push each counter into `Outcome.measures`
  with `report::fact` (monotonic counts) or `Samples::finish` (per-run
  distributions) under `automation.cdp.<counter>`. The result file, the gate
  checker, and this page need no other change. Warm command p50 and p95 use the
  same `Samples` and the quiet-host gate as the CDP round trip here.
