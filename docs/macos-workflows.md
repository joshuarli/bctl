# macOS workflow performance

The macOS goal is working Shadowdriver automation with lower latency and
resource cost than stock native Chromium. Both interactive automation and Grab
crawls count. Agent-browser must also benefit from its bctl provider on at least
two typical workflows. Functional compatibility alone does not close this goal.

The owner set this objective on 2026-11-12, after the earlier
[functional qualification](handoffs/2026-11-12-macos-results.md).
The earlier performance waiver remains part of that historical qualification.
It does not waive the workflow targets below.

## Targets

These are targets, not demonstrated results. Ratios compare the same completed
work with the fastest valid native configuration measured on the same host.
The native comparison includes a software-rendering control.

| Measure | First milestone | Acceptance target | Stretch target |
| --- | --- | --- | --- |
| Complete Shadowdriver interactive task, including fresh browser startup | Native parity | At least 20% lower median latency | 1.5× faster |
| Warm agent-browser form and news extraction | Native parity on both | At least 20% lower median latency on both | 1.5× faster on both |
| Complete Grab crawl, including startup, assets, persistence, and offline verification | Native parity | At least 20% lower median latency | 2× completed pages per second |
| Host physical footprint for the complete process tree | Below tuned native | At least 50% less | At least 2× concurrent crawl throughput per GiB |
| Fresh browser startup to usable CDP | Native parity | At most 300 ms and no slower than native | At most 150 ms |
| CPU seconds per completed task | Establish measurement | No increase over native | At least 25% less |

Acceptance requires at least five alternating clean samples per side, the same
outputs, and no new failures. A gap inside the observed spread is inconclusive.
Latency and resource passes use separate fresh profiles. Host load checks must
pass before and after every retained latency sample.
The load check uses a one-minute average and can miss shorter contention.

The current four-page Grab fixture is a bounded preliminary workload.
General crawl acceptance also requires a larger site with dynamic assets and
a sustained concurrent crawl. Throughput per GiB requires completed work under
a shared memory budget. Dividing two isolated memory readings is insufficient.

## Functional contracts

The benchmark uses the actual Shadowdriver SDKs, Grab CLI, and agent-browser
provider. Every workload checks application output within its measured region.

| Workload | Required completed work |
| --- | --- |
| Shadowdriver TypeScript | Connect to the supplied browser, navigate, find an accessible textbox, insert text, dispatch mouse input, observe form submission, capture a 1280×720 PNG, close the tab, and detach |
| Grab Python | Capture four linked pages with four browser slots, check page markers and exact stylesheet bytes, persist state, verify the recorded browser product, and pass `grab check` |
| agent-browser form | Launch through the selected provider, open a page, snapshot, fill, click, wait for submission, capture a screenshot, and read the submitted text |
| agent-browser news | Launch through the selected provider, render the pinned React news site, snapshot, navigate from Business to World, extract 30 articles, persist the extraction, and capture a screenshot |

News extraction must have the same content hash on both sides. The fixture is
Speedometer 3.1's `news-next` build at commit
`1386415be8fef2f6b6bbdbe1828872471c5d802a`.

The form and crawl use identical IPv4 loopback URLs on both sides. Nanos sends
browser loopback requests through the host egress policy. The benchmark permits
only `127.0.0.1/32`. Default sessions still deny host loopback.
The regression in `crates/bctl/tests/egress.rs` covers the default denial and
explicit allowance, including IPv4-mapped IPv6 loopback.

## Measurement contract

`tests/bench/macos_workflows.py` runs on Apple Silicon macOS with Nanos/HVF.
The harness uses release bctl, the pinned interoperability agent-browser binary,
and the sibling Shadowdriver checkout. Shadowdriver's TypeScript build and
Python extension must match that checkout. Its Node requirement comes from
`ts/package.json`.

Both sides use fresh profiles. The OS and image caches remain warm.
Nanos uses the production defaults of two vCPUs and 4096 MiB.
Native Chromium can use the host's available CPUs.
Complete-task timing starts before browser launch and ends after output checks.
Trial directory preparation and browser shutdown happen outside that region.
Warm agent-browser timing starts after its viewport setup.

The native SDK and Grab endpoint launches headless stock Chromium with
`--no-first-run`, `--no-default-browser-check`,
`--disable-background-networking`, and `--remote-debugging-port=0`.
Agent-browser uses its own production native launcher.
`--native-arg=--disable-gpu` supplies the software-rendering control for both
native launchers. Nanos uses its production headless software renderer.

The resource pass samples macOS `footprint` every 250 ms on a best-effort basis.
It uses the deduplicated `Summary Footprint`, including compressed pages.
The process set includes the driver, fixture server, clients, daemon,
supervisor, and VMM or native browser tree. Partial process sets are rejected.
The reported sampled peak can miss a shorter peak. It is neither summed RSS nor
the guest's configured memory limit.

The JSON receipt contains attempts, load decisions, outputs, raw sample values,
browser versions, source revisions, and executable hashes. The harness rejects
changes to the recorded artifacts or Shadowdriver HEAD during a run.
Screenshots, mirrors, state databases, and peak footprint tool output remain in
the artifact directories named by the receipt.

## Commands

The default command runs five latency and five resource rounds per side for all
four workloads:

```sh
python3 tests/bench/macos_workflows.py --out .work/tmp/macos-workflows.json
python3 tests/bench/macos_workflows.py --native-arg=--disable-gpu \
  --out .work/tmp/macos-workflows-software.json
python3 -m unittest discover -s tests/bench -p test_macos_workflows.py
```

`node`, `perf-gate-ready`, `footprint`, and `lsof` must be available on PATH.
The sibling SDK builds and `.work/interop/agent-browser/bin/agent-browser`
must already exist. The Nanos image and runtime use the normal `.work` paths.
`--workloads`, `--rounds`, and `--resource-rounds` select subsets and counts.
`--bctl-image` selects an image file and its adjacent `.profile.img` template.
The default remains `.work/images/nanos-helium.img`.
Fewer than five samples produce no median claim.

## Optimization order

Startup is the first interactive target. The initial comparison placed most of
the complete-task gap before the browser became usable. A separate startup
profile must distinguish daemon discovery, disk preparation, VMM startup,
guest initialization, browser initialization, and readiness polling.

Grab's persisted stage timings show substantial settling, scrolling, and
readiness waits. Kernel changes cannot remove those required waits.
Changes to capture policy must preserve the same output checks and apply to
both browser backends.

The native GPU process accounts for much of the initial apparent memory gap.
A comparison against hardware-rendering defaults cannot establish a kernel
memory advantage. The software-rendering control determines the resource gap
that optimization must close.

A separate five-round [startup profile](../tests/bench/results/2026-11-13-macos-workflows/startup-profile.json)
places the first visible guest-readiness stage at 823 ms (658–888) after `up`.
Cold `up` takes 923 ms (868–949), and daemon startup takes 140 ms (125–150).
These are medians from five clean samples. The profile polls status every
20 ms and adds work. Its times are not interchangeable with the workflow table.
The coarse stages cannot separate disk preparation, HVF creation, firmware,
kernel initialization, and Chromium initialization before the first probe.
That portion needs instrumentation before choosing a kernel optimization.

A [latency-tier experiment](../tests/bench/results/2026-11-13-macos-workflows/qos-experiment.json)
tests macOS timer-coalescing as a source of warm interaction latency.
Only the owned Nanos VMM receives `taskpolicy -l 0`, after viewport setup.
Fresh trials use ABBA ordering, with five samples per candidate condition and
workload. Native Chromium retains `--disable-gpu` and its unchanged process policy.
All 40 workflows pass the output and load checks.

| Nanos warm workflow | Unchanged policy, ms | Latency tier 0, ms |
| --- | --- | --- |
| agent-browser form | 182 (157–264) | 277 (182–279) |
| agent-browser news | 283 (261–296) | 251 (238–279) |

These are medians with minimum to maximum. The experiment establishes no
reliable improvement across both workflows. Runtime policy remains unchanged.
CPU cost of this setting is unmeasured. The saved hook reproduces the policy
placement. Cold task totals include its setup and are excluded from this claim.

## Preliminary results, 2026-11-12

The refreshed run uses Shadowdriver
`3a5aeb5c58e72080a5b3dd898627c8fc5c5728ab`, Node 26.13.0, pnpm 12.18.0,
effect 4.1.2, and the rebuilt Python 3.15 extension with the `dist` profile.
The host is Mac17,7, Apple M5 Max, 18 CPUs, 48 GiB, macOS 27.0.1.
Stock Chromium is 155.0.8059.84. Nanos runs Helium's Chromium 156.0.8078.79.
Agent-browser is 0.42.0 at `e7519957b9067c541825afa21a792fbe1f483047`.

The Nanos image is SHA-256
`cb9bac6eae6abbe671a26c339a193bcdc32100eb73d82631bc45c12396901246`.
It includes the explicit browser loopback routing repair at bctl `cbcf5d8` and
Hydrogen `5fe102e8a055a4c675467baae0cc90c28c8024f9`.

### Native defaults

Nanos has higher complete-task median latency in all four workloads.
The run completes 80 of 80 workflows: five latency and five resource passes
per side and workload. All 40 latency samples pass both host-load checks.
No recorded artifact or Shadowdriver HEAD changes during measurement.
Each HTTP-fixture crawl records four expected HTTPS-upgrade failures with
`ERR_SSL_PROTOCOL_ERROR`, followed by four successful HTTP captures.
Those fallback attempts remain inside the measured task and in the receipt.

Values are medians with minimum to maximum in parentheses. Each cell has five
samples. Complete-task latency includes browser startup and output verification.

| Workload | Native complete task, ms | Nanos complete task, ms | Native physical footprint, MiB | Nanos physical footprint, MiB |
| --- | --- | --- | --- | --- |
| Shadowdriver form | 565 (531–669) | 1151 (1074–1280) | 711.8 (667.2–740.7) | 417.5 (408.0–424.4) |
| Grab, four pages | 15734 (15471–16170) | 16263 (16247–16626) | 770.4 (767.6–775.0) | 395.6 (395.5–396.3) |
| agent-browser form | 1138 (1034–1195) | 1315 (1197–1381) | 774.0 (768.9–786.1) | 354.1 (348.6–355.0) |
| agent-browser news | 1188 (1093–1256) | 1343 (1285–1443) | 868.8 (861.1–871.2) | 380.6 (379.2–381.2) |

Startup to usable CDP is 292 ms (264–384) for the native SDK endpoint and
840 ms (766–937) for Nanos in the form workload. The ready-browser form task is
284 ms (266–301) and 315 ms (308–343), respectively.
Ready-browser Grab medians are 15488 ms (15207–15892) native and
15451 ms (15430–15865) Nanos. Their spreads overlap.

Warm agent-browser form medians are 210 ms (185–223) native and
276 ms (195–288) Nanos. Warm news medians are 196 ms (175–223) native and
244 ms (241–293) Nanos. No speed win is established.

The memory reductions against native hardware-rendering defaults are
configuration results. They do not establish a kernel advantage.
The software-rendering native control is required before assessing the
50% memory target.

The [full default receipt](../tests/bench/results/2026-11-13-macos-workflows/default.json)
contains every attempt and executable hash. Adjacent `default-*-footprint.txt`
files retain the complete footprint tool output for each median memory trial.
The UTC receipt date is 2026-11-13. The local measurement date is 2026-11-12.

The earlier Shadowdriver `0460bac` comparison is superseded after the owner's
SDK update. The interrupted native software control from that SDK version is
incomplete. Routing probes that used different host URLs or unreachable host
interfaces are invalid comparisons. Those receipts remain in `.work/tmp` and
are excluded from the tables above.

### Native software-rendering control

The second run adds `--disable-gpu` to native Chromium and agent-browser's
native launch. Nanos retains the same production configuration. This run also
passes 80 of 80 workflows and all 40 latency load checks. Inputs remain unchanged.
Each cell contains five samples and reports the median with minimum to maximum.

| Workload | Native complete task, ms | Nanos complete task, ms | Native physical footprint, MiB | Nanos physical footprint, MiB |
| --- | --- | --- | --- | --- |
| Shadowdriver form | 561 (505–646) | 1132 (1089–1258) | 354.5 (316.4–384.6) | 417.2 (399.0–426.5) |
| Grab, four pages | 15602 (15519–15781) | 16381 (16253–16624) | 428.6 (427.3–430.0) | 396.2 (394.5–396.9) |
| agent-browser form | 940 (901–1014) | 1283 (1215–1314) | 444.3 (437.9–449.4) | 355.1 (349.4–356.2) |
| agent-browser news | 966 (927–1031) | 1323 (1297–1357) | 531.6 (500.7–533.2) | 379.3 (373.6–381.1) |

Nanos's sampled footprint medians are 7.6% lower for Grab, 20.1% lower for
agent-browser form, and 28.6% lower for agent-browser news. The Shadowdriver
form median is 17.7% higher. The 50% memory target is unmet.
All complete-task latency medians remain higher on Nanos.

Warm agent-browser form medians are 221 ms (200–245) native and
267 ms (170–283) Nanos. Warm news medians are 178 ms (163–199) native and
280 ms (248–302) Nanos. The warm speed targets remain unmet.

The [full software-rendering receipt](../tests/bench/results/2026-11-13-macos-workflows/software.json)
contains every attempt. Adjacent `software-*-footprint.txt` files retain the
complete footprint tool output for each median memory trial.

Both resource runs retain sampling failures, including processes exiting
during collection. The default run also encounters non-UTF-8 arguments in an
unrelated process. Valid complete samples still cover every trial.
The default run has at least four complete samples per short workflow and
36 per Grab crawl. These remain sampled peaks, rather than exact peak claims.
The recorded harness is preserved at bctl `8f4662f`. The later collector repair
preserves arbitrary OS argument bytes when reading `ps`. It prevents unrelated
non-UTF-8 arguments from rejecting a whole process-table sample.
The repair passes a real-process regression and four native and Nanos workflow
resource trials with a foreign process holding a non-UTF-8 argument throughout.
Receipts also update atomically, preserving the previous complete snapshot if
serialization fails. The final functional receipt is
`.work/tmp/macos-workflows-sampler-final.json`.

## Open qualification work

### Startup and cost instrumentation

`BCTL_STARTUP_TRACE=1` enables host supervisor, runner configuration, and
readiness-probe timestamps. Host records include wall-clock microseconds.
Supervisor and readiness records also include process-local elapsed time.
`LIBKRUN_TIMING=1` enables the pinned runtime's existing VMM timings.
The production image remains unchanged.

The guest supervisor also recognizes `BCTL_STARTUP_TRACE=1` in its image
environment. Its records use `CLOCK_MONOTONIC_RAW` microseconds.
Hydrogen's `HYDROGEN_BOOT_PROFILE` build flag adds kernel boundaries.
Host and guest clock origins differ. Only intervals within one clock domain
are comparable.

`tests/bench/macos_startup.py` records these diagnostics separately from
acceptance timings. `--image` selects a diagnostic image with a matching
`.profile.img` template. Console observations poll every millisecond.
The receipt retains the image and executable hashes, raw stage records,
console observation times, browser identity, and host-load decisions.

Five isolated instrumented boots place kernel clock registration through
guest supervisor entry at a median 10.965 ms (10.301–13.091).
Chromium execution through the successful CDP probe takes a median
548.681 ms (490.935–695.132). These intervals include instrumentation overhead.
The latter includes readiness polling and HTTP request handling.
It does not isolate Chromium work from kernel services or scheduling.
The diagnostic startup total is 848.5 ms (802.4–1040.8).
These observations do not establish a release speedup.
The [startup receipt](../tests/bench/results/2026-11-13-macos-costs/startup.json)
and adjacent serial and supervisor logs retain all five samples.

`--cpu-rounds` on `macos_workflows.py` adds separate fresh-profile CPU passes.
The collector uses libproc's live-process and reaped-child counters.
Mach timebase conversion produces CPU seconds on Apple Silicon.
The process set includes the driver, fixture, clients, daemon, supervisor,
and VMM or native browser. Process discovery is included. Cleanup is excluded.
CPU passes require host-load checks before and after every retained sample.
Snapshots are not atomic. A process disappearing during collection rejects
the attempt. Tests cover live descendants, reaped children, overlapping roots,
orphaned runtimes, and a child that consumes a known quantity of CPU.

`macos_crawl.py --cpu-rounds` measures the complete capture-and-replay pipeline
in separate passes. Workers retain live-process and reaped-child counters
through replay. The coordinator adds its own CPU, including the fixture server.
Worker interpreter startup and browser cleanup are excluded.
The CPU collector's smoke test passes two concurrent four-page crawls per
backend, including all 16 captures and 16 offline replays.

```sh
python3.15 tests/bench/macos_workflows.py --rounds 0 --resource-rounds 0 \
  --cpu-rounds 5 --native-arg=--disable-gpu --out .work/tmp/macos-cpu.json
python3.15 tests/bench/macos_startup.py --rounds 5 \
  --image .work/tmp/startup-profile.img --out .work/tmp/macos-startup.json
```

The [CPU receipt](../tests/bench/results/2026-11-13-macos-costs/cpu.json)
completes 40 workflows with five alternating samples per side and workload.
All 80 host-load checks pass. No recorded artifact or Shadowdriver HEAD changes.
The native side uses the software-rendering control.
Values below are CPU seconds, median with minimum to maximum.

| Complete task | Native CPU seconds | Nanos CPU seconds |
| --- | --- | --- |
| Shadowdriver form | 1.621 (1.378–2.008) | 1.950 (1.740–2.092) |
| Grab, four pages and `grab check` | 2.830 (2.548–3.041) | 3.315 (3.208–3.637) |
| agent-browser form | 1.542 (1.456–1.666) | 1.747 (1.495–2.119) |
| agent-browser news | 1.739 (1.655–1.911) | 2.133 (1.941–2.374) |

Nanos uses more CPU in Grab and news. Their observed ranges do not overlap.
The form-workload ranges overlap, so those comparisons are inconclusive.
The no-increase CPU target is unmet. The four-page Grab CPU measurement excludes
offline replay. The sustained crawl below measures the full pipeline separately.

`tests/bench/macos_crawl.py` adds a sustained workload with 16 linked pages
per crawl and two concurrent independent crawls by default.
Each page fetches dynamic content, injects CSS, and loads a lazy image.
Every worker checks all page markers, paragraph sections, and exact asset bytes.
The worker then runs `grab check` and fresh native `grab verify` on every page.
Replay browser provenance remains in the receipt.
Capture browsers stay alive through replay on both backends.
The coordinator's elapsed time includes worker setup and cleanup.

Latency and footprint use separate passes. Resource passes aggregate the
coordinator, fixture, workers, and their owned process trees.
The footprint budget is sampled and checked after each trial.
It is not an operating-system memory cap or a measurement of maximum supported
concurrency. Exceeding the budget rejects the resource trial.
The default budget is 2048 MiB.

```sh
python3.15 tests/bench/macos_crawl.py --native-arg=--disable-gpu \
  --out .work/tmp/macos-crawl.json
python3.15 -m unittest discover -s tests/bench -p 'test_macos*.py'
```

Benchmark cancellation stops command process groups and lets crawl workers
close their browsers and VMs before forced termination.
Timeout and termination regressions cover child delivery, cleanup, and signal
exit status. The four-page dynamic fixture passes capture and offline replay
on both backends. A second smoke run passes two concurrent crawls per backend.
Smoke timings are excluded from performance claims.

### Sustained crawl results

The [full crawl receipt](../tests/bench/results/2026-11-13-macos-costs/crawl.json)
completes all 20 trials. Each trial runs two concurrent independent 16-page
crawls with four browser slots each, followed by fresh native offline replay.
All 640 page captures and all 640 offline replays pass.
The five latency samples per backend pass all 20 host-load checks.
Recorded input hashes remain unchanged. Shadowdriver remains clean at `3a5aeb5`.

Each crawl records 16 expected HTTPS-upgrade retries with
`ERR_SSL_PROTOCOL_ERROR`, then 16 successful HTTP captures.
The [capture outcome summary](../tests/bench/results/2026-11-13-macos-costs/crawl-outcomes.json)
retains every worker's counts. Retries remain in the measured task.

Values are medians with minimum to maximum. Each cell has five samples.
The footprint includes capture browsers, fresh replay browsers, workers,
coordinator, fixture, and the owned daemon and VMM trees.

| Measure, 32 captured and replayed pages | Native | Nanos |
| --- | --- | --- |
| Complete task, seconds | 134.45 (133.74–137.54) | 136.68 (134.76–141.35) |
| Completed pages per second | 0.2380 (0.2327–0.2393) | 0.2341 (0.2264–0.2375) |
| Sampled peak footprint, MiB | 1538.2 (1532.9–1546.5) | 1651.4 (1637.9–1657.3) |

The latency and throughput ranges overlap. No measurable speed advantage is
established. Nanos uses 7.4% more sampled physical memory for this full pipeline.
The 20% latency and 50% memory targets remain unmet.

Every resource trial stays below the 2048 MiB sampled budget.
The corresponding rates per budget GiB are 0.1190 pages/s native and
0.1171 pages/s Nanos. These measure the chosen two-job workload.
They do not measure maximum concurrency or establish the 2× density target.

Each resource trial has at least 155 complete samples.
The collector rejects 224 samples whose process set changes during collection.
Those errors remain in the receipt. Adjacent `crawl-*-footprint.txt` files
retain every trial's peak tool output. These are sampled peaks.

Fresh native offline replay takes about 80 seconds per worker on both sides.
The native verifier therefore limits how much startup-only changes can improve
this complete task. Capture settling, scrolling, preload, and readiness waits
also remain in the task.
Representative [native replay](../tests/bench/results/2026-11-13-macos-costs/crawl-native-offline.txt)
and [Nanos replay](../tests/bench/results/2026-11-13-macos-costs/crawl-nanos-offline.txt)
logs retain all 16 page checks from one worker each.
Every mirror and state database remains at its receipt's artifact path.

The separate [sustained CPU receipt](../tests/bench/results/2026-11-13-macos-costs/crawl-cpu.json)
completes ten trials with five alternating samples per backend.
All 320 captures and 320 fresh native offline replays pass.
All 20 host-load checks pass. Recorded input hashes and Shadowdriver HEAD
remain unchanged.

| CPU seconds, 32 captured and replayed pages | Native | Nanos |
| --- | --- | --- |
| Median (minimum–maximum) | 24.242 (23.867–29.562) | 27.911 (25.802–31.982) |

The observed ranges overlap, so the CPU comparison is inconclusive.
No CPU improvement is established. The complete workload still includes the
same fresh native verifier on both sides. Worker interpreter startup and
browser cleanup remain outside the CPU snapshots.

```sh
python3.15 tests/bench/macos_crawl.py --pages 16 --jobs 2 --slots 4 \
  --budget-mib 2048 --rounds 0 --resource-rounds 0 --cpu-rounds 5 \
  --native-arg=--disable-gpu --out .work/tmp/crawl-cpu-full.json
```

### Anonymous page-size experiment

The [page-size experiment](../tests/bench/results/2026-11-13-macos-costs/page-size.json)
compares the default Nanos image with `transparent_hugepage: "never"`.
The candidate uses the same kernel, rootfs manifest, and profile template.
Only its root tuple changes the transparent-hugepage policy.
The retained [image tuple](../tests/bench/results/2026-11-13-macos-costs/page-size-image-tuple.txt)
records that setting.

The complete Shadowdriver form passes all 30 workflows.
Each image has five alternating fresh-profile latency, CPU, and footprint
samples. All 40 host-load checks pass. Recorded inputs remain unchanged.

| Nanos form task | Default | Small pages |
| --- | --- | --- |
| Complete task, ms | 1150 (1138–1178) | 1155 (1050–1202) |
| CPU seconds | 1.607 (1.595–1.670) | 1.602 (1.589–1.677) |
| Sampled peak footprint, MiB | 401.6 (396.7–420.6) | 426.9 (397.8–446.3) |

Values are medians with minimum to maximum. The ranges overlap.
The experiment establishes no latency, CPU, or memory benefit.
The small-page setting is rejected. The production image retains its policy.
The [reproducer](../tests/bench/results/2026-11-13-macos-costs/page-size-experiment.py)
runs from the bctl repository after both named images exist.
Adjacent `page-size-*-footprint.txt` files retain every resource trial's peak.

### Daemon discovery interval

The client now checks daemon readiness every 10 ms during startup, instead of
sleeping 100 ms between attempts. The 15-second deadline, daemon lock ownership,
and request-delivery retry rules remain unchanged.
All three daemon restart and request-delivery regressions pass.

The [comparison receipt](../tests/bench/results/2026-11-13-macos-costs/daemon-poll.json)
uses the saved release binary from `77b392b` and a binary with only the
[poll interval change](../tests/bench/results/2026-11-13-macos-costs/daemon-poll.diff).
Five alternating samples per binary measure fresh daemon discovery, complete
Shadowdriver forms, and CPU cost in separate passes.
All 30 form workflows and 10 daemon checks pass. All 60 host-load checks pass.
Recorded inputs remain unchanged. Release binary size remains 5,658,944 bytes.

| Measure | Previous interval | Current interval |
| --- | --- | --- |
| Fresh daemon readiness, ms | 116.7 (114.4–118.1) | 21.0 (18.5–23.3) |
| Complete form task, ms | 1234 (1199–1336) | 1102 (1059–1142) |
| Startup through CDP discovery, ms | 830.8 (806.7–857.0) | 710.5 (685.7–742.0) |
| CPU seconds | 1.655 (1.609–1.706) | 1.658 (1.637–1.686) |

Values are medians with minimum to maximum.
Complete-task latency falls 10.7%, with disjoint ranges.
The CPU ranges overlap, so no CPU change is established.
This comparison establishes an improvement over the previous Nanos binary.
It does not establish the 20% target against native Chromium or a warm-workflow
speedup. The pinned VM image and kernel remain unchanged.

The initial footprint pass collects only six or seven complete samples per
trial and catches different portions of Node's short lifetime.
Its apparent memory regression is not retained as a comparison.
The [dense footprint repeat](../tests/bench/results/2026-11-13-macos-costs/daemon-poll-dense.json)
removes the sampler's 250 ms pause between collections.
It passes ten more forms with 23–25 complete samples each.
Sampled peak footprint is 432.0 MiB (430.5–434.2) for the previous interval and
432.0 MiB (427.1–433.7) for the current interval.
The overlapping ranges establish no memory change.
Sampling still cannot prove an absolute peak. Earlier short-task footprint
results also retain this sampling limitation.

The [comparison reproducer](../tests/bench/results/2026-11-13-macos-costs/daemon-poll-experiment.py)
and [dense-pass reproducer](../tests/bench/results/2026-11-13-macos-costs/daemon-poll-dense-experiment.py)
run from the bctl repository with the named release binaries available.
Adjacent `daemon-poll-*-footprint.txt` files preserve every resource trial's peak.

### Chromium startup traces

`BCTL_TRACE_CHROMIUM=1` in a diagnostic image enables Chromium's
[startup tracing switches](https://raw.githubusercontent.com/chromium/chromium/main/components/tracing/common/tracing_switches.cc).
The browser records the `startup,toplevel` categories for two seconds as JSON
at `/home/browser/Downloads/chromium-startup.json`.
The trace therefore appears in the session's host files directory.
The default image does not enable tracing.

`macos_startup.py --chromium-trace` waits for the exported JSON after recording
startup and retains its event count, path, and digest.
Five isolated Nanos boots export valid traces with unchanged recorded inputs.
Five native software-rendering launches provide a diagnostic control.
All 20 startup host-load checks pass.
These are instrumented profiles. They do not establish release timing wins.

The [stage summary](../tests/bench/results/2026-11-13-macos-costs/chromium-stages.json)
reports five samples per backend. Values are medians, in milliseconds.
Elapsed and CPU values describe the named trace span.

| Trace span | Native elapsed | Nanos elapsed | Native reported main-thread CPU | Nanos reported main-thread CPU |
| --- | --- | --- | --- | --- |
| `BrowserMainRunnerImpl::Initialize` | 164.1 | 332.2 | 120.7 | 91.9 |
| `ChromeBrowserMainParts::CreateProfile` | 26.9 | 111.7 | 16.0 | 31.3 |
| `StartupBrowserCreator::Start` | 121.4 | 115.4 | 89.5 | 35.0 |

The summary retains minimum and maximum values.
These spans nest and must not be added together.
Reported thread CPU excludes other threads and is not the host process-tree
CPU measure. Elapsed minus reported CPU does not distinguish I/O, locks,
delegated work, or scheduling.
Profile creation is a concrete interval for further investigation.

The [Nanos receipt](../tests/bench/results/2026-11-13-macos-costs/chromium-nanos.json)
and [native receipt](../tests/bench/results/2026-11-13-macos-costs/chromium-native.json)
retain every profile path and digest. Adjacent `chromium-*.json.gz` files retain
all ten traces. The [native reproducer](../tests/bench/results/2026-11-13-macos-costs/native-startup-trace.py)
runs from the bctl repository.

One [traced form trial](../tests/bench/results/2026-11-13-macos-costs/chromium-form-timeout.json)
times out while run beside concurrent validation.
The subsequent debug attempt and all five sequential
[traced form repeats](../tests/bench/results/2026-11-13-macos-costs/chromium-form-repeat.json)
pass. No cause or fix is established for that timeout.
Startup tracing is qualified for isolated diagnostics. Concurrent automation
while tracing remains unqualified. Traced form timings are excluded from
performance comparisons.

After both performance runs, the current Nanos and SmolVM images each pass all
113 interoperability checks in separate runs. Nanos's three egress tests pass,
including default loopback denial and explicit IPv4 and mapped-IPv6 allowance.
Receipts are `.work/tmp/macos-workflows-{nanos,smolvm}-interop.log` and
`.work/tmp/macos-workflows-egress.log`. The current SmolVM image hash is
`9a195deed99cd84c27193b41e72c5e0d89eb2a8d7461348f099f8ea616b8b051`.

The performance targets above remain unmet.
The bounded dynamic-asset and concurrent-crawl checks above pass.
Chromium build versions differ
between stock macOS and the pinned Linux Helium archive. These are product
comparisons. They do not isolate kernel performance.
Representative screenshots were visually checked on both backends. Native
fonts and scrollbar layouts differ. News article data must match exactly.

The preliminary crawl runs `grab check` and exact artifact checks.
After measurement, copies of the native and Nanos mirrors each pass all four
pages through offline replay with stock native Chromium 155.0.8059.84.
The [native mirror replay](../tests/bench/results/2026-11-13-macos-workflows/verify-native.txt)
and [Nanos mirror replay](../tests/bench/results/2026-11-13-macos-workflows/verify-nanos.txt)
receipts record those checks. Replay cost is outside the preliminary timings.
The verifier currently creates a fresh native browser with `control_url=None`.
The full crawl target must include that browser's latency and footprint, or
provide fresh isolated replay through bctl. Reusing a capture profile would
change the verifier's clean-profile contract.
Shadowdriver tracks a loaded-Linux replay failure as
[SHD-30](https://github.com/joshuarli/shadowdriver/blob/3a5aeb5c58e72080a5b3dd898627c8fc5c5728ab/TODO.md).
Its macOS behavior under contention remains unqualified.

The independent Carbon review remains unavailable. Hydrogen's pending-init
hot-unplug case and HYD-3 and HYD-4 remain open in its qualification report.
