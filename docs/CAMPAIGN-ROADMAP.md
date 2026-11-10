# Campaign: bctl, from first VM to first useful release

## Status

Goal: `bctl` (browserctl) runs chromium-build's Helium browser headed inside
a SmolVM Linux arm64 guest on an Apple Silicon Mac. It shows each browser live
in a local dashboard and accepts real keyboard and pointer input there. It
runs Grab crawls and Shadowdriver programs against those browsers as named
jobs, and it hands control to a human and back when a site needs a login. The first
useful release closes when M3 below holds end to end. Along the way the guest
proves a pure-Rust userland (M4R) whose crates port back into laputa.

Baseline 2026-11-08: the repo has no commits. Facts below were checked against
the sibling repos on that date; see **Baseline**.

Accepted: P0 (`feac477`), P1 (`6ad2878`), B1 (`c38e9b7`: `cargo run -p
bctl-proof -- m1 --runs 10`, 10/10 at about 4 s per boot-to-teardown). `guest/tinyland` and `guest/seatd`
are vendored and pass `cargo check --locked --target
aarch64-unknown-linux-musl --all-targets`.
C1 (`f539b0f`), C2 (`e1ac445`), C3 (`4979a9d`), C4 (`b1e3de2`), each proved by
its VM-backed test (`cargo test -p <crate> --test <name> -- --ignored`).
X2 Nanos lane (`248a1db`): not viable; see `docs/research/unikernel-nanos.md`.
D1 and `bctl profile login` (`0f3b8c1`), D2's input lease (`59fa9c8`), proved by
`pnpm --dir web exec playwright test` against a real VM. T2 (`cb8dccd`,
`docs/compositor-contract.md`). J2 (`ac9b005`, lane). T0 (`8f3e21f`, lane:
`sh guest/harness/smolvm-proof.sh`).
J1 (shadowdriver `6568107`). T1 (`7418ce2`). R0 (`8ea5874`, on
`ref-alpine-chromium` only: 207 gaps, `docs/userland.md`). H2's CDP gateway
(`c931e2f`, `cargo test -p bctl --test cdp -- --ignored`): capabilities,
revocation, and refusal of new attachments while a person holds the lease.
H2's socket and egress (`60d977c`): CDP reaches the guest only over a Unix
socket, and guests launch with `--block-private` (`--test egress`); a session
re-opens a private range only with `bctl up --allow-private CIDR`, which is
H2's explicit allow. T3 (`28544a6`: B1 with pointer `<select>` and
`window.open` focus on `ref-alpine-tinyland`). N1 (`21b26c7`, `crates/cdp`).
J3 (`92c4b1b`, `cargo test -p bctl --test runs -- --ignored` with `BCTL_GRAB`
set: exec outcomes, cancel, kill -9 to Interrupted, login wall to
NeedsInteraction to resume and a verified mirror, resume from `state.db`).
X2 Unikraft (`c2e9501`): not viable, not self-hostable and no sandbox.
J3 open: Grab has no pause or login classifier (pause is cancel at a page
boundary plus a new attempt; `login-required` needs Grab to report it); Grab's
events file and own log are not capped by bctl; the guest Chromium exits when
its last tab closes (H3).
N2 (`286a96b`, `cargo test -p bctl --test tabs -- --ignored`): `bctl tab`
list/new/use/close, the selected target per generation, `TARGET_GONE`, and
refusal of new/close under a person's lease. Its `2021087` clears Chromium's
stale profile lock at start, which had stalled every second `up` of a profile.
D2 (`8743285`, `BCTL_GRAB=… pnpm --dir web exec playwright test`, 6/6): run
pages, bounded logs, download-only artifacts, and takeover and resume of a
login-walled crawl through the live view. D2 closed (`e7f651d`, dashboard
suite 3/3 on `helium` and 1/1 on `ref-alpine-tinyland`): noVNC sends a shifted
symbol without a Shift event, so SmolVM's VNC input now holds Shift around one
(`smolvm-0003-vnc-shifted-keysyms.patch`), and the specs wait on conditions
instead of fixed sleeps.
N3 (`7a763bf`, `cargo test -p bctl --test snapshots -- --ignored`): bulk
accessibility snapshots, refs scoped to generation, target, frame, and
document, `STALE_REF`, both iframe kinds; a warm form snapshot costs 3 CDP
calls and 317 bytes. N3 open: node liveness runs in the page's main world
(N4 should use an isolated world); a node moved between same-process frames
is not detected; one huge DOM read can exceed the 32 MiB message cap.
N4 (`be0fc95`, `cargo test -p bctl --test actions -- --ignored`): locators
(ref, CSS, role and name, label, placeholder, test id, `--nth`), actions with
real input, hit-tested clicks that confirm the press landed, isolated-world
helpers, dialogs, and refusal under a person's lease. Actions that send input
bring the selected tab to the front (a hidden tab stalls input); the binding
never moves. N4 open: actions fail at once rather than waiting for
actionability; popups an action opens are not reported; a `wait` holds the
session's page lock; the lease is checked only at command start (N6).
N5 (`87a6c93`, `--test batches`): `bctl page batch`, the documented
`bctl-automation` library with `examples/drive.rs`, and `docs/automation.md`.
N6 (`774a538`, `--test authority` and the dashboard spec, 7/7): one in-memory
`Automation | Yielding | Human` per session shared by native commands, the
input lease, the CDP gateway, and run resume; a takeover ends a running
`wait`, `goto`, or batch with `TAKEN_OVER`. M3N open: runs do not cooperate
with a takeover (Grab and exec cannot pause without ending the attempt);
a host program's own library connection is not interrupted; authority resets
to Automation on daemon restart. N7 (`--test actions --test authority`,
fixtures adapted from Playwright `84e2bdc` `page-click.spec.ts`): actions wait
up to `--timeout` (5 s) for visible, enabled, editable, stable, and hit, and a
timeout names the last failing check; input is sent once and a lost reply is
`UNKNOWN_OUTCOME`; a takeover ends preparation in every phase and cannot race
a dispatch, because the last authority check and the send share the lock that
leaves Automation. Chromium drops input to an unpainted page for about 500 ms
after load, and preparation waits for paint. N7 open: a scroll already sent
can land just after Human begins; iframe changes are seen only by the 200 ms
recheck; a warm click is 18 CDP calls.
R1 (`94b122e`, B1 3/3 and T0 on `ref-scratch-chromium`, `FROM scratch` with
a Rust `bctl-guest` supervising seatd, tinyland, and the browser and relaying
CDP): census gaps are the browser's 141-library closure plus `libxkbcommon`
(R3) and `xkb-data`. Tinyland now carries a built-in US keymap (`2698564`)
and reads xkb-data only when present, but the browser's own libxkbcommon
still needs the rules (without them it logs `Couldn't look up rules 'evdev'`
and crashes when it connects), so xkb-data is part of the libxkbcommon gap
and goes away with R3's kbvm-backed library. `bctl-guest` exits non-zero when a
supervised child dies on its own. Mesa ships but is never loaded (its GBM
backend is looked for only at `/usr/lib/gbm`); P2 decides with Helium.
R3 (kbvm `c21a050`, bctl `1deae2c`; B1 3/3 and T0 on `ref-scratch-chromium`,
census with no `libxkbcommon` or `xkb-data` unit): the fork's
`kbvm-xkbcommon` exports the 33 `xkb_*` symbols the stand-in closure imports
(20 from `chromium`, 13 more from GTK's `libgdk-3`), with a differential
suite against libxkbcommon 1.9.2 for each. Without rules,
`xkb_keymap_new_from_names` serves a built-in evdev/pc105/us keymap and logs
it; GDK hits that path at start. Tinyland pins the same kbvm revision. B1 now
checks shifted symbols, Caps Lock, Alt and Ctrl chords, key repeat, and the
Ctrl+Shift+U compose sequence; the US layout has no dead keys, so no dead-key
sequence was tested. R3 open: no Helium arm64 archive exists to check its
imports against the 33.
H1 baselines (`docs/performance.md`, `tests/bench/`): every suite now
measures the `helium` r6 image on an idle host (load 0.11 to 0.22 per core),
50 of 50 gates pass. Cold `up` 1.47 s, warm 1.45 s, `down` 0.11 s, `pause`
1.82 s, `resume` 2.25 s with the page kept 7 of 7; a TodoMVC workflow through
`bctl page` 277 ms after `759b308` stopped the first action after a navigation
from waiting out a 200 ms quiet interval (450.6 ms before); CDP round trip 0.26
ms through the gateway; input to pixel switches between about 9 and 22 ms with
the same build, cause open; 458 MiB VMM footprint and 3.6% of a core idle; Speedometer 3.1 at 31.4;
`news-next` load event 71.9 ms and largest contentful paint 140 ms. The browser
suite measures Helium alone; the container comparison is gone.
H1 findings: `smolvm machine pause`
refused any machine with an exposed socket, image layers, or `--gpu`, which was
every session; the vendored SmolVM now pauses 2D-GPU sessions in place
(`52557a1`) and `bctl pause`/`bctl resume` drive it, with page state, tabs and
the VNC frame intact (provisional, load 3.9: pause 1.9 to 2.6 s, resume to
ready 2.9 to 3.1 s over 5 cycles); X1's portable checkpoint restore is still
open; the guest Speedometer stalls (7 of 16 runs) were a macOS socket
content filter on this host stalling TCP to the host's LAN address, not the
guest: the bench now reaches the host at SmolVM's gateway `100.96.0.1`
(`f38e3a4`, `--test host_http`), 24 of 24 runs finish that way, and
`docs/jobs.md` says to use it; SmolVM's relay tracks at most 512 guest TCP
connections and holds host-closed ones for a 10 s TIME-WAIT, so more than
about 50 short connections a second are refused (not hit by Chromium's
keep-alive pool; a crawl of close-per-request servers could). SmolVM's main
tip, vendored in `third_party/smolvm` and built from source by
`runtime/build.sh` (`1ae1c06`, B1 10/10 and the VM suites pass), already
tracks 65,536 flows and reaps TIME-WAIT at once, which closes that limit.
Its Cargo graph links `ring` and `aws-lc-rs` through rustls and rcgen, and
its guest agent is built with Debian's musl-gcc: both break campaign rules
and are being fixed in the vendored copy. The dashboard lease race on viewer reconnect
was not retested after N6.
T5 (`f386cb1`, `guest/tinyland/harness/wlcs/run.sh --sweep`,
`docs/research/tinyland-wlcs.md`): WLCS v1.8.1 drives tinyland through a
test-only headless feature and musl adapter; 79 pass, 767 unsupported, 379
fail, mostly because WLCS commits buffers before acking the first configure
(which xdg-shell forbids and tinyland rejects) and because tinyland maximizes
every toplevel; one real bug fixed (a shrunk shm file now raises
`wl_shm.invalid_fd`). Decided: patch WLCS's clients to ack configures rather
than loosen tinyland, and stop reusing `wl_surface` error 0 for unrelated
client errors.
P2 (lane `lane/p2`; `images/helium`, `BCTL_BROWSER_ARCHIVE=DIR images/build.sh
helium`): Helium 0.20.3.1 (`helium-0.20.3.1-r2`, SHA-256 `9f02f520…`) FROM
scratch with the Rust `bctl-guest`, tinyland, seatd, kbvm's libxkbcommon,
and musl 1.2.6 from the Alpine 3.24.1 base chromium-build builds on (the
musl-loader question). The build refuses an archive whose sidecars do not
bind its name, digest, product, and arm64, runs chromium-build's
`install-runtime.py --verify-extract` at a pinned revision, and fails unless
every ELF file resolves its interpreter, `DT_NEEDED` closure, and imported
symbols; `/etc/bctl/receipt.json` records `Helium 0.20.3.1 (Chromium
156.0.8078.79)` and the archive SHA-256. Image archive 256 MB, rootfs
694 MiB. P1's checks, the daemon suites, and the dashboard suite (7/7) pass
with `BCTL_TEST_IMAGE=helium`. B1 `--runs 10` passed in 7 of 10 invocations
(113 of 118 runs): 5 runs stopped when a CDP command (`Runtime.evaluate`,
`Page.enable`) exceeded its deadline just as a new renderer started (popup,
cross-site navigation, new tab); 30 runs of `ref-scratch-chromium` under the
same host load all passed, and the cause is not found, so the default image
is not switched yet. Mesa's GL stack is not shipped (206 MiB): with the
appliance's flags the GPU process cannot `dlopen` `libEGL.so.1` (musl keeps
`LD_LIBRARY_PATH` as a pointer Chromium's process title overwrites), and even
when it can, Chromium's blocklist leaves WebGL off on llvmpipe
(`docs/userland.md`). Census: 87 gaps, 74.0 MiB, all browser closure (FFmpeg
unbundled 65.3 MiB with libstdc++/libgcc_s/libgomp; ATK, AT-SPI, D-Bus, GLib
6.2 MiB; X11/XCB 1.7 MiB). Helium imports 20 `xkb_*` symbols, all in kbvm's
33. Helium segfaults at start when `$HOME` does not exist; the image has
`/home/browser` (cause, `1efde7a`: Helium skips Crashpad's handler when its
database path cannot be created, then stores through the null signal handler;
chromium-build's `helium-crashpad-null-signal-handler.patch` is in r6). P2 open: the B1 stall; kbvm's cdylib links Alpine GCC's
`crtbeginS.o`; T2's census needs a shell and a second Chromium, so it does
not run on a scratch image; Chimera's recipe was not compared.
chromium-build `e9809cb` closes the arm64 closure: FFmpeg, ANGLE and
SwiftShader come from the Chromium tree, GLib, GIO, udev, system minigbm and
VA-API are off, and NSS, NSPR, SQLite, expat, zlib, libffi and xkbcommon are
built with the musl clang. Its `helium-0.20.3.1-r3` arm64 archive (138.4 MB,
sha256 `a5dcb350…5b60`) bundles 13 libraries, `check-purity.sh` reports 0
impure across its 23 ELF files and archives, and its acceptance sidecar
passes all ten checks, headless included. P2 moves to r3.
P2b (lane `lane/p2b`): the B1 stall was renderers dying with SIGSYS, not bctl.
A fresh CDP client could not evaluate in the stuck target either, so the
relay, the gateway, and h12tiny were out; `Target.targetCrashed` events showed
renderers exiting with SIGSYS (31), uBlock Origin's background page several
times a minute and, intermittently, the renderer of the page B1 drives, whose
pending DevTools commands then never answer. gdb in a diagnostic image found
two seccomp kills, both musl 1.2.6 against Chromium's sandbox: `pwrite` is
`pwritev2` (syscall 287), which only the GPU policy allowed (renderers died
writing parked images to disk), and `pthread_getschedparam`, which
`absl::Mutex` calls on contended locks, blocks all signals around
`sched_getparam` on a thread id, so the sandbox's SIGSYS rewrite of that call
killed the process instead of running (hence the load dependence). Fixed in
chromium-build master (`bb20f58`: allow `pwritev2` wherever `pwrite64` is
allowed; `4275db8`: allow `sched_getparam` and `sched_getscheduler` for any
target); the rebuilt archive is
`helium-0.20.3.1-r5` (SHA-256 `35858f46…`). On r5, B1 passed 150 of 150 runs
under three concurrent loops with no renderer crash, then `--runs 10` three
times (30/30), with `ref-scratch-chromium` 10/10. `bctl up` now defaults to
`helium` (`IMAGE_REQUIRED` is gone); the ref images stay selectable
diagnostics, and the VM suites default to `BCTL_TEST_IMAGE=helium`. The image
moved to chromium-build `e9809cb`'s pure-musl closure (census: one gap,
`libexpat`, 191 KiB; check-purity 0 impure across 27 ELF files), and
`tinyland`, `seatd`, `bctl-guest`, and kbvm's `libxkbcommon.so.0` are linked by
llvm-prebuilt-musl's LLVM (`llvm-musl-23.1.5-3afbb74`) with `std` rebuilt, so
none carries a GCC-compiled object. Archives older than r5 still stall. WebAudio
(`cb0a98d`, `--test media`): audio worklets under load crash no renderer;
musl answers `sched_setscheduler` with ENOSYS without a syscall, so realtime
audio priority is never applied and the blocked-signal kill is not reached.
H3 (`a3b3308`, `--test failures`, 8/8): a 1 s health watch ends a session as
`failed` when the guest, browser or CDP dies or the last tab closes, runs end
as `browser-crashed` or `job-error`, run output is capped at 256 MiB and all
runs at 2 GiB. The `runs` login test's flake was Shadowdriver silently
launching a host Chromium when the attached guest browser died; shadowdriver
`d42c8f9` fails the attached pool instead, and `runs` passed 5/5 in a row.
Why the guest browser died then is not known.
chromium-build `5eda85c` and `4f0d95f` move both arches to
`llvm-musl-23.1.5-dc0d9b9`, whose lld marks GNU_STACK itself.
The shared runtime under `.work/runtime` had been the pause lane's build with
a GCC-built guest agent (219 of 231 files impure); it is rebuilt from main
(check-purity: 0 impure), and VM results before that rebuild ran on the
impure agent.
X1 (`fc484eb`, `--test restore` in `bctl-runtime-smolvm`,
`docs/research/x1-template-restore.md`): a session restored from a checkpoint
taken at the appliance's pre-browser forkpoint reaches ready with the sandbox
intact, 1,137 ms against 1,445 ms cold at the median of 10 interleaved runtime
launches (load 14). Restored sessions have no guest network and no profile
mount, so `bctl up` does not offer them; X1 open: networked restore, profile
remount, then `bctl up` using the checkpoint.
Helium r6 (chromium-build `410801d`, SHA-256 `7289b25b…`, 0 impure, 11/11
acceptance): starts without `$HOME` and reports `Helium/0.20.3.1` over
DevTools; the image now refuses archives before r6. On r6 the restore,
sessions, failures, runs, cdp and dashboard suites pass.
Performance pass (2026-11-09): the browser suite measures Helium alone
(`89cf4d8`); the first action after a navigation no longer waits out a 200 ms
quiet interval (`759b308`, TodoMVC workflow 450.6 to 277.2 ms, gate 470 ms);
n3's snapshot no longer fails when an iframe moves process mid-read
(`b895525`, 8 of 8 against about half). Stopped for a noisy host and the
Helium-on-Nanos comparison on a Linux host.
M7N L0 and L1 (Helium on Nanos, Linux amd64): `runtime/build-nanos.sh` and
`images/nanos-helium/build.sh` boot Helium r5 headless with its sandbox on;
through the vsock Unix socket `/json/version` answers `Helium/0.20.3.1` and
the check port reports `browser ok`, `sandbox ok`, `input absent`. Getting
there fixed hydrogen's unprivileged `gid_map` check (it compared the group
with the euid, so Chromium's namespace probe failed for `browser`, 1000:27).
L2 (`crates/runtime-nanos`, `cargo test -p bctl-runtime-nanos --test
lifecycle -- --ignored`, 10/10): a per-machine `bctl-nanos-vm` supervisor
runs vhost-device-vsock and QEMU on a qcow2 overlay, serves the host CDP
socket by relaying to guest vsock port 9222, and stops QEMU over QMP;
readiness is machine, guest check, browser, sandbox, then `/json/version`
through the host socket. Launch to ready 1.2 to 5.8 s, median 1.8 s
(unloaded host not checked). It found a second hydrogen bug: a host vsock
connect during early boot asserted the guest kernel before the socket layer
existed. L3 and L4 (`BCTL_TEST_RUNTIME=nanos`, the default on Linux): on Nanos the
`snapshots` (2/2), `batches` (1/1), `native_cdp` (1/1), `host_http` (1/1)
suites pass, plus `egress`'s browser-level test (host loopback, the host's
LAN address, RFC 1918, link-local and unique-local IPv6 refused and never
reached, the public internet reached); `tabs`, `cdp` and `actions` pass
except the tests that need a person to take input (headless has none).
`native_cdp` had asserted a `Chrome/` product, stale since Helium reports
`Helium/`. uBlock Origin held requests and reloaded tabs until its first-run list
compile finished, which broke `actions`' navigations right after `up`; a
`filters` ready stage waited it out, and sessions now start with uBlock
disabled instead (`docs/performance.md`, Results on Nanos). Open on
Nanos: `actions`'
lost-input test sees `Input.dispatchMouseEvent` answer although the page
busy-waits 13 s (the guest clock is exact; a headless input difference is
suspected, not shown); the guest-shell `egress` test and the `failures`
suite need `machine exec`, which Nanos has no equivalent for; the profile
lives on the machine's disk overlay and does not outlive the session; a start occasionally never gets
past the guest stage (twice in about 60 boots, both under host load); one
Speedometer run in seven stalls at 0 tests right after `up`. L5 (2026-11-10):
`docs/performance.md` has the Nanos column. hydrogen `2c46223e` fixed a
loopback TCP stall that made every page action 300 to 1000 ms; TodoMVC is
144 ms against the Mac's 277, cold `up` 5.2 s against 1.5 s, VMM RSS 1.7 GiB. It found a third
hydrogen bug: guests with more than 2 GiB asserted at boot, so the
default 4 GiB session never started. M7N is the active milestone; its slices are
under **Sequence**. M7H, headed Helium on Nanos, follows it; its NH0
investigation may run as a lane alongside. After it, the Mac performance queue: (1) on an idle host, find what switches input to pixel between
9 and 22 ms; the timer-coalescing test is in `docs/performance.md` Findings;
(2) pause 1.8 s and resume 2.3 s against a 1.5 s cold `up`, where the time goes
is unprofiled; (3) guest page loads trail the host by 13 ms to first byte and
50 ms to the load event through SmolVM's relay; (4) X1's networked restore.
Every perf claim needs `PERF_GATE_MAX_LOAD=0.25`, which this host meets only
when nothing else runs.
New host: on Linux amd64 (Alpine), install `sqlite-static` (rusqlite links
SQLite statically on musl) and build the dashboard with `pnpm --dir web
install && pnpm --dir web build` before any `cargo` command; then `cargo test
--workspace` passes (L0, 2026-11-10). Nothing under `.work/` is needed except
the browser archive. Rebuild
the rest: `sh runtime/build.sh` (vendored SmolVM and libkrun), then
`BCTL_BROWSER_ARCHIVE=DIR images/build.sh helium`, the Speedometer checkout
named in `docs/performance.md`, and `cargo build --release -p bctl -p
bctl-bench`. The Helium r6 arm64 archive
(`helium-0.20.3.1-r6-linux-aarch64-musl.tar.xz`, SHA-256
`7289b25b30fa86d41928e63a4210b3006f43ba8da35d9cf5c874b05152215b7a`, with its
`.sha256` and `.acceptance.json`) exists only on the old Mac and is not
published. Copy that directory, or rebuild it from chromium-build `410801d` on
an arm64 builder; that r6 had `helium-devtools-product.patch` applied, which
chromium-build's `metadata/patches.json` does not list, so a rebuild must
check that `/json/version` reports `Helium/`. Unmerged lane branches
(`lane/j2-jobs`, `lane/t0-decouple`, `lane/t1-rust-seat`,
`lane/x1-wip-backup`) are pushed to origin.
`lane/x1-wip-backup` is an unmerged backup branch of X1, kept.
Resume: `continue the campaign from docs/CAMPAIGN-ROADMAP.md Status`.

M8C (external clients) is active: E1 to E3 passed on the Mac with SmolVM and
Nanos; their Linux x86_64 Nanos runs are pending, and E4 is next.

## Charter

- Decide and keep going. Record a decision only when a later item depends on
  it. Within the boundary below, proceed without approval; park anything that
  needs authority outside it.
- Hard stops park the item: an irreversible or outside-the-machine action,
  disabling the browser sandbox, substituting a different browser for the
  product image, or a loop that cannot check anything.
- Items marked **cross-repo** change a sibling repository. They commit there,
  follow that repo's conventions, and keep its existing behavior unchanged
  unless the item says otherwise.
- Every commit is a Conventional Commit with the workstream as scope (`runtime`,
  `image`, `core`, `cli`, `server`, `web`, `jobs`) and the item ID in the body.
  Push verified commits to `origin`. Code comments never cite this doc.
- Worktrees live at `.work/wt/<item>`, scratch at `.work/tmp`; `.work/` is
  ignored through `.git/info/exclude`.
- Correctness before speed. No perf work until H1 has recorded baselines.

## Decisions

- Owner 2026-11-08: SmolVM first, unikernel deferred. A unikernel or other VMM
  is evaluated only in X2, against the same appliance acceptance suite.
- Owner 2026-11-08: a prebuilt Linux arm64 musl Helium will exist. This
  campaign consumes it as `helium-<ver>-linux-aarch64-musl.tar.xz` with the
  same `.sha256` and `.acceptance.json` sidecars and archive layout as the
  published amd64 artifact. As of chromium-build `d23a9e4` the build still
  refuses non-amd64 (`chromium-build:199`, `Dockerfile:72`), so P2 blocks on
  the artifact, not on any bctl work.
- Prescribed: the Mac guest is native Linux arm64. Rosetta is not a path: SmolVM
  rejects `--rosetta` together with `--gpu` (`third_party/smolvm/src/cli/machine.rs:8401`
  in niceforge), and the display needs GPU.
- Prescribed: until the arm64 artifact lands, display and controller work use a
  reference image `ref-alpine-chromium` (Alpine aarch64 `chromium` APK). It is
  test scaffolding. It is never the default and never selected implicitly. A
  run on it records `image=ref-alpine-chromium` in its receipt.
- Prescribed: headed Helium on a virtual display, never `--headless`. The
  renderer sandbox stays on. An image whose renderers lack `NoNewPrivs=1` and
  `Seccomp=2` fails qualification (the same check as chromium-build's
  `helium_test_fast.py:150-152`).
- Owner 2026-11-08: bctl owns its Wayland stack and is the proving ground for
  a pure-Rust userland that is later ported back into laputa. `tinyland` and
  `seatd-rs` were copied from laputa `3aa94e30` into `guest/tinyland` and
  `guest/seatd`. They keep laputa's names and markers. What ports back is a
  crate's source and its tests, so bctl-specific glue lives in
  `guest/appliance`, never inside those crates. Harnesses belong to their
  host project. `mdevd-rs` stays behind, because the SmolVM guest's device
  set is fixed and devtmpfs plus ownership set by the guest init covers it.
  Reversal: the appliance needs hotplug.
- Prescribed: "pure-Rust userland" means every file in the product image is
  Rust-owned or one of these named exceptions: the Linux kernel, musl (both
  retained by laputa LAP-51); Mesa, ALSA, and Mozilla NSS (Network Security
  Services, not skarnet nsss) with their own library
  closures (owner 2026-11-08); and Helium together with what it statically
  bundles (its HarfBuzz, FreeType, Skia, zlib, libwayland-client, and
  bundled Fontconfig). Every shared library Helium loads at runtime is part
  of the userland, not part of the exception. R0's census is the only
  evidence for the claim. PID 1 is libkrun's `krun-init` (Rust), followed by
  SmolVM's static musl agent (Rust). Neither needs replacing.
- Prescribed: laputa's text forks (rust-fontconfig, HarfRust, Swash) do not
  apply to the appliance, because Helium statically bundles its own
  shaping, raster, and discovery. LAP-52 also rules out reproducing the
  `Fc*` ABI. They stay laputa's track for Ghostty and Play.
- Prescribed: the lean appliance (M4L) removes only machinery web content
  cannot observe: crash reporting, the DevTools frontend, D-Bus, and
  background services. Any surface a page can observe, such as audio and
  video devices, print, WebGL strings, fonts, or EME key systems, is either
  kept or faked to a persona. Persona values come from Shadowdriver's
  per-profile identity, never from constants baked into the build. Lean
  changes apply to the linux/arm64 Helium build only; amd64's resolved GN
  args stay unchanged.
- Owner 2026-11-08: the persona starts honest. It claims arm64 Linux, the
  WebGL vendor and renderer strings stay the real llvmpipe ones, and the
  absence of Widevine stays honest unless L6 finds that real arm64 Linux
  Chrome has it. Reversal: L6's measurements show the honest persona is
  what gets sites to reject or challenge access, and the owner picks a
  replacement.
- Prescribed: `guest/` is Linux-only and builds separately from the host
  crates, for `aarch64-unknown-linux-musl` on the shared
  `nightly-2026-09-15` toolchain.
- Prescribed: bctl drives a pinned `smolvm` executable through a typed process
  adapter. It does not link SmolVM or libkrun. The display and input path is
  SmolVM's own: virtio-gpu scanout to host framebuffer to RFB and WebSocket
  (`src/agent/vnc.rs`), with input through virtio-input (`src/agent/input.rs`).
  The guest runs no VNC server.
- Owner 2026-11-09: vendor and fix anything that blocks the campaign; this is
  pre-approved. Fixes land in bctl's vendored copy (or the laputa-systems fork
  that owns the code) before anything is reported upstream. The host's macOS
  content filter is MDM-managed and out of reach, so bctl routes around it:
  guests reach host services at SmolVM's gateway `100.96.0.1`, never the
  host's LAN address.
- Owner 2026-11-09: bctl vendors SmolVM at the upstream tip and builds it
  from source, so it can fix SmolVM itself (the 512-connection relay limit,
  pause and checkpoint for bctl's sessions) before anything is reported
  upstream. Nothing is reported upstream until bctl's fix exists. This
  supersedes the next entry once the vendored build passes the VM suites.
- Prescribed (legacy, until the vendored build lands): the runtime is the official SmolVM 1.31.4 macOS bundle, taken by
  digest, with only `libkrun.dylib` rebuilt (`runtime/build.sh`,
  `runtime/runtime.toml`). The bundle already enables display and input, but
  its venus-only virglrenderer rejects `RESOURCE_CREATE_2D` with `EINVAL`, so
  no CPU-rendered scanout reaches the host. bctl's one libkrun patch keeps
  rutabaga's 2D component beside the default one. Tinyland's dumb buffers
  need it too. Reversal: upstream libkrun accepts 2D resources on macOS, at
  which point drop the patch and the source build. Upstreaming it is the
  owner's call.
- Prescribed: CDP is the only automation boundary. The guest is a browser
  appliance (agent, compositor, Helium) and runs no automation code. Grab,
  Shadowdriver, and `exec` jobs are CDP clients outside the VM, attaching
  through `control_url`. Shadowdriver's attach path launches nothing and never
  terminates an attached browser (`src/crawler.rs:358-386`, `:610`), and Grab
  takes `--control-url` (`grab/src/grab/cli.py:128`). Run output and Grab's
  resume state therefore live on the host. A job is unchanged between a local
  and a remote session; only the CDP URL differs. Consequence: `exec` jobs
  have the user's host authority, the same as running the script by hand.
  When CDP round trips to a remote host cost too much, the job runs on that
  host beside its VMs, never inside a guest.
- Prescribed: concepts are Session (a running or stopped browser environment),
  Profile (persistent browser identity and storage), Job (a reusable workload
  definition), Run (one execution, with attempts, logs, artifacts), and Image
  (an immutable appliance). VM identifiers appear only in `bctl inspect`.
- Prescribed: one VM per session, one writable session per profile, one active
  run per session (multiple tabs inside it).
- Prescribed: two job kinds, `grab` and `exec`. There is no workflow language.
  Multi-step navigation and extraction are ordinary user programs
  (Shadowdriver code or a Rust program on bctl's API).
- Owner 2026-11-08 (from an external review): bctl gets its own native
  automation layer (M3N). The daemon owns a Rust CDP client per session and
  offers page-level actions (tabs, accessibility snapshots with element refs,
  semantic locators, actionable clicks, fill, waits) through the CLI, the
  daemon socket, and a Rust library API. It still runs on the host and
  speaks CDP, so the boundary above is unchanged. agent-browser
  (vercel-labs) is a reference for behavior and failure semantics only: no
  code, dependency, daemon model, launcher, or profile handling comes from
  it. No Tokio: the transport uses h12tiny's websocket on the daemon's
  existing smol-family executor, improving h12tiny where it falls short.
  Grab and Shadowdriver stay supported; nothing migrates off them in this
  campaign.
- Prescribed: control authority is one explicit per-session state, not the
  RFB input lease alone: `Automation | Yielding | Human`. `Human` is entered
  only after `Yielding` resolves (no native command in flight; a cooperating
  run has reached an event boundary or `needs_interaction`). The RFB lease
  is held exactly in `Human`. Native commands that mutate the page are
  refused outside `Automation`. External CDP clients are gated by H2's
  gateway: while `Human`, the gateway refuses new attachments; a run's
  existing connection is cooperative through Grab's control protocol. Until
  H2 lands, raw loopback CDP bypasses the gate and takeover is exclusive
  only against bctl's own automation and RFB viewers.
- Prescribed: Rust for CLI, daemon, and gateway; SQLite for durable state;
  Solid 2 with `@laputa-systems/ui-dashboard` 0.5.0 (light only) for the
  dashboard, built to static assets embedded in the `bctl` binary. No Node at
  runtime. The live view uses noVNC's core RFB client behind bctl's own page.
- Prescribed: SmolVM's VNC has no authentication (`vnc.rs:9`, `:480-482`). Its
  listener binds `127.0.0.1` on an ephemeral port and only bctl's gateway
  connects to it. The browser-facing stream requires a dashboard session and
  an Origin match. CDP is forwarded to loopback only. In v1 any local process
  of the same user can reach it; H2 puts it behind the gateway.
- Settled 2026-11-08 (C4): profile storage is a host directory,
  `<BCTL_HOME>/profiles/<profile>`, mounted at `/profile` over virtiofs. The
  preferred per-profile disk is unusable: smolvm loses its packed-layers disk
  when `--disk` is attached to a machine booted from a local image archive.
  The host directory passed the criterion: a cookie persisted across 20
  `down`/`up` cycles with no SQLite lock or corruption messages.
- Delegated: guest base. Prefer an Alpine aarch64 rootfs carrying SmolVM's
  static musl agent. Fall back to niceforge's Debian 13 agent rootfs if SmolVM
  rejects a non-Debian base, and record which in `docs/image-contract.md`.
- Owner 2026-11-08: Rust cryptography uses graviola. Never `aws-lc-rs`,
  `aws-lc-sys`, or `ring`, directly or through a dependency's default
  features; a crate that needs a TLS or crypto provider gets graviola's.
- Delegated: Rust crates (CLI parsing, HTTP, SQLite, WebSocket). Each needs a
  one-line reason in its commit body and must fit H1's binary-size gate.

- Owner 2026-11-09: extend bctl's *consumer compatibility* to released
  browser-use 1.0.0 and agent-browser v0.42.0, through existing CDP
  attachment and an optional agent-browser provider plugin (M8C). This
  does **not** change the earlier M3N rule that agent-browser is behavior
  reference only for bctl's native implementation: no code, dependency,
  launcher, or daemon is copied. Client automation runs on the host; bctl
  remains the single owner of Helium, profile, VM and run state. Release
  pins and proof gates live in `docs/research/external-clients.md`.
  During human control, existing external CDP connections are **not**
  currently gated; do not claim exclusive takeover until M8C's
  controller ownership tests pass. External bctl clients remain
  unqualified until a real pinned-client test passes.
- Owner 2026-11-10: templates get no public CLI. Once X1's networked restore
  and profile remount pass, `bctl up` restores from the image's pre-browser
  forkpoint checkpoint when one exists for that image and runtime, and boots
  cold otherwise; the checkpoint is a cache the daemon makes and discards.

## Baseline

Checked 2026-11-08, read-only, nothing built or run.

- **SmolVM (niceforge vendored, `v1.31.4`).** `SMOLVM_DISPLAY=WxH` adds a
  virtio-gpu scanout and host framebuffer, but only when GPU is enabled
  (`src/agent/launcher.rs:~955-1000`). `SMOLVM_VNC=<port|host:port>` serves the
  raw RFB 3.8 protocol, an HTML client at `/`, RFB-over-WebSocket at
  `/websockify`, and H.264 at `/video` (`vnc.rs:299-340`). Input attaches only
  if libkrun exports `krun_add_input_device`; otherwise the session is
  view-only (`launcher.rs:1063-1076`). `krun_add_display` returns `-ENOTSUP`
  without the `gpu` feature (`libkrun/src/libkrun/src/lib.rs:4183-4186`), so
  niceforge's current build cannot show a display. A DRM compositor is required
  in the guest. CLI: `smolvm machine run|create|start|stop|exec|cp|status|ls|
  logs|checkpoint|branch|delete`, with `-p` port forwarding, `--storage`, and
  `-v`. The upstream headless-browser example is not vendored.
- **chromium-build (`d23a9e4`).** Helium 0.20.3.1 on Chromium 156.0.8078.79,
  `helium-0.20.3.1-linux-x86_64-musl.tar.xz`, Ozone Wayland only, no X11 or
  headless ozone platform, `use_dbus=false`, setuid `chrome_sandbox` shipped,
  Mesa llvmpipe, GTK3, fonts and fontconfig bundled. Consumers verify it with
  `scripts/install-runtime.py --archive … --verify-extract DIR`. Acceptance runs
  Sway with `WLR_RENDERER=pixman` and no XWayland, and checks product
  identity, Wayland window, page rendering, renderer sandbox, and clean
  shutdown. Open: whether the archive bundles the musl loader
  (`/lib/ld-musl-*.so.1`), whether X11 libs are compiled out, and libdbus
  arriving through GTK (CHB-4).
- **Shadowdriver.** Attach with `ChromiumBackendOptions.control_url` (Rust,
  Python `Pool.create`, TS `Browser.connect`). It needs `/json/version` on a
  loopback HTTP endpoint. Managed launch uses `--remote-debugging-port` on
  `127.0.0.1`; no pipe mode. Linux campaign: accepted none.
- **Grab (Python and TS).** It has `--control-url`, `--headful`, `-o`, `-j`,
  `-p`, `-d`, `--max-runtime`, `--robots`, and `--no-resume`. Resume state is
  SQLite at `<mirror>/_grab/state.db`. Progress is unstructured text on stderr
  (`cloner::progress` lines). Cancellation is SIGINT/SIGTERM only. On a
  challenge it tells the user to rerun `--single --headful`
  (`cloner.py:1912-1916`). It has no interactive-login pause.
- **Tinyland (`guest/tinyland`, copied from laputa `3aa94e30`).** A
  single-fullscreen kiosk. Output is DRM
  only (`/dev/dri/card0`, atomic with legacy fallback), which suits a
  virtio-gpu scanout. Globals: wl_compositor 4, wl_subcompositor, wl_shm,
  wl_output 2, wl_seat 5 keyboard-only, wl_data_device_manager 3, xdg_wm_base.
  `GetPointer` posts MissingCapability. `xdg_popup` requests are no-ops and
  `xdg_positioner` only validates. There is no linux-dmabuf, xdg-output,
  viewporter, fractional-scale, primary-selection, or text-input. Input is
  evdev and kbvm through libseat/seatd. It is qualified only with
  Ghostty/foot on a QEMU Alpine 3.20.3 guest. It has never run Chromium.
- **Dashboard library.** `@laputa-systems/ui-dashboard` 0.5.0, Solid
  `>=2.0.0-rc.18` peer, light scheme only.

## Sequence

### M0: platform contracts

- **P0 Display-capable runtime.** Accepted `feac477`. Owns `runtime/`.
  `runtime/build.sh` verifies the release digest, builds libkrun at the
  pinned revision with `runtime/patches/`, swaps in `libkrun.dylib`, and
  checks for `krun_add_display`, `krun_add_input_device`, and
  `krun_set_display_backend`, for no Homebrew paths, and for the expected
  `smolvm --version`. A rerun recompiles only the patched crates. Build
  needs: Homebrew `libkrun/krun/virglrenderer-krun`, the Xcode Command Line
  Tools, and the `aarch64-unknown-linux-musl` target on the init toolchain.
  None of niceforge's 20 SmolVM patches were needed.
- **P1 Reference appliance image.** Accepted `6ad2878`. Owns `images/`,
  `docs/image-contract.md`. An Alpine 3.22 container image (the SmolVM agent
  rootfs is Alpine 3.19 and is not bctl's), with Sway on pixman, seatd with
  `SEATD_VTBOUND=0`, libudev-zero, and the Alpine `chromium` APK, started by
  `images/bctl-guest`. Accepted on SmolVM: `bctl-guest check` reports `seat`,
  `compositor`, `browser`, and `sandbox` ok; host `/json/version` through
  `-p 19222:9222` returns Chrome/142 and a `webSocketDebuggerUrl` on the
  host port; VNC frames show Chromium's UI.
- **P2 Helium arm64 product image.** Owns `images/helium/`. Prereq: P1 and the
  arm64 artifact. Install the archive with chromium-build's
  `install-runtime.py --verify-extract`. Refuse an archive whose sidecars do not
  bind its digest, product, and `aarch64`. Resolve the musl-loader question.
  Acceptance: same checks as P1, and the image's receipt records the Helium
  version string and archive SHA-256. Qualification also covers every
  shipped ELF (interpreter, transitive `DT_NEEDED`, RPATH/RUNPATH, the
  `dlopen`s R0 traces) and the provenance of statically linked compiler
  support: no dynamic dependency does not prove no static GNU runtime, and a
  `libgcc_s.so.1` rebuilt from compiler-rt and libunwind (Chimera's approach)
  does not meet the no-libgcc rule. The Helium archive gets its own
  imported-symbol census against the kbvm libxkbcommon ABI; the Alpine
  stand-in's 33 symbols include GTK's and do not qualify Helium. Before P2,
  compare Chimera Linux's musl/aarch64 Chromium recipe
  (`chimera-linux/cports` `main/chromium`, pinned) with chromium-build's
  Helium patches: a matrix of covered, not applicable, proposed with a
  failing repro, or rejected; its GTK/Qt/Pulse/PipeWire desktop profile is
  out.

### M1: one observable browser (scripts, no daemon)

- **B1 End-to-end proof.** Owns `tests/acceptance/m1.sh`,
  `tests/fixtures/input.html`. Prereq: P0, P1. Boot one VM with
  `SMOLVM_DISPLAY=1280x800`, `SMOLVM_VNC=127.0.0.1:0`, and CDP forwarded to a
  host loopback port. Then attach Shadowdriver through `control_url`, navigate
  to the fixture, and drive it twice: once through CDP and once through RFB
  input injected by a scripted RFB client. Acceptance: the fixture records
  typed text, a click, a scroll, a `<select>` choice, and a `window.open` popup
  from RFB input; a framebuffer diff shows changed pixels after each; CDP
  navigation works; `smolvm machine stop` leaves no host process and the
  ports closed. Run ten times consecutively without a failure. Rerun on P2's
  image when it exists.

### M2: `bctl up/down/status/serve`

- **C1 State and lifecycle core.** Owns `crates/core/`. Prereq: B1. SQLite
  schema for sessions, profiles, runs, attempts, leases, and artifacts, with
  explicit states: Session `Stopped | Starting | Ready | Stopping | Failed`;
  Run `Queued | Starting | Running | NeedsInteraction | Succeeded | Failed |
  Cancelled | Interrupted`. Input control is a separate lease. Before any
  external effect, record the intent with a generation token. VMs are named
  `bctl-<session>-<generation>`. Reconciliation on start compares records
  with `smolvm machine ls`. Decision logic takes the runtime as a trait so it
  is tested without VMs. Acceptance: unit tests prove reconciliation adopts an
  owned VM, marks an unknown-outcome attempt `Interrupted` and never retries
  it, refuses a second writable session on one profile, and never adopts a
  VM whose generation does not match.
- **C2 SmolVM adapter.** Owns `crates/runtime-smolvm/`. Prereq: C1. Typed
  wrapper over the pinned `smolvm` binary. It produces a capability receipt
  (arch, display, input, checkpoint support) at startup and maps each `smolvm`
  failure to a stable error code. Readiness proceeds machine, then guest
  agent, then compositor, then CDP `/json/version`, then the first non-black
  framebuffer. An open port alone is not ready. Acceptance: integration test
  (needs a VM) for start, ready, stop, and restart; a runtime without input
  reports `view-only` and the gateway refuses input for it.
- **C3 Daemon and CLI.** Owns `crates/bctl/`, `crates/server/` (API only).
  Prereq: C2. A per-user daemon, started lazily by the CLI, on a 0600 Unix
  socket under `~/Library/Application Support/bctl/`. Commands: `up NAME
  [--profile P] [--image I]` (idempotent for the same resolved config;
  conflicting config fails with a remedy), `down NAME` (keeps profile and
  artifacts), `rm NAME`, `status`, `inspect NAME`, `profile list`, and
  `doctor` (runtime receipt, image identities, why not ready). Every read
  command takes `--json`. Errors carry stable codes. Closing the CLI never
  stops a session. Acceptance: two sessions run concurrently with isolated
  profiles; `kill -9` of the daemon followed by `bctl status` reconnects to
  both without duplicates; `up` twice is a no-op; `status` names the stage a
  starting session is stuck in.
- **C4 Profiles.** Owns profile storage in `crates/core/` and
  `crates/runtime-smolvm/`. Prereq: C3. Settle the profile-storage decision.
  Add `bctl profile login P`, which brings up a session on P and opens its
  view. Acceptance: log into a fixture site, `down`, `up`, and the session
  cookie persists. 20 restarts produce no profile corruption.
- **D1 Gateway and session dashboard.** Owns `crates/server/` (HTTP, gateway),
  `web/`. Prereq: C3. `bctl serve [--open]` enables the dashboard on the
  existing daemon. It does not start a second owner. It binds
  `127.0.0.1:<port>`. `--open` hands the browser a one-time token that becomes
  an HttpOnly SameSite=Strict cookie. The gateway proxies RFB WebSocket to the
  session's private VNC port after checking session cookie and Origin. Views:
  a sessions list (name, profile, state, current URL from CDP, last activity,
  low-rate thumbnail) and a session page dominated by the live view, with tab
  list, focus tab, reconnect viewer, and stop. Only the selected session
  streams at full rate. A dropped stream shows as a stream problem, not a
  browser crash. Acceptance: a Playwright test against `bctl serve` sees live
  pixels change after CDP navigation, types into the fixture through the
  dashboard, and gets 401 on the stream without the cookie and 403 with a
  foreign Origin; reopening the page reconnects.

### M3: jobs and human takeover (first useful release)

- **J1 Grab structured events. Cross-repo (shadowdriver `grab/`).** Prereq:
  none; can run from M1 on. Add `--events PATH` writing JSONL records
  (`started`, `progress {pages_done, pages_queued, assets_done}`,
  `needs_interaction {reason, url}`, `finished {outcome}`), plus a control
  input (`--control PATH`, a FIFO, accepting `resume` and `cancel`). With
  `--control-url` and `--events`, a challenge or login wall emits
  `needs_interaction` and waits for `resume` instead of telling the user to
  rerun `--headful`. Existing text output and behavior without the flags are
  unchanged. Acceptance: Grab's test suite plus new tests for the event
  schema, resume after interaction, and cancel at a page boundary leaving a
  resumable `state.db`. The schema is a public contract: document it in
  `grab/docs/`.
- **J2 Job definitions.** Owns `crates/core/jobs/`, `docs/jobs.md`. Prereq:
  C3. `bctl.toml` (version 1; unknown fields rejected) defines `[browsers.*]`
  (image, viewport, cpus, memory_mib) and `[jobs.*]` of kind `grab` (urls,
  max_pages, jobs, robots, max_runtime, profile) or `exec` (argv, cwd,
  timeout). Each run records the resolved image identity, browser version,
  config, input digest, and profile. `exec` gets `BCTL_CDP_URL` and
  `BCTL_OUTPUT_DIR` in its environment and runs with no shell interpolation.
  Acceptance: config parse tests including rejection cases; a run's
  `inspect --json` shows every resolved field.
- **J3 Runs.** Owns `crates/core/runs/`, CLI `grab`, `run`, `runs`, `logs`,
  `pause`, `resume`, `cancel`. Prereq: J1, J2, C4. `bctl grab URL --session S`
  is a one-off `grab` job. Outcomes are categorized: `browser-crashed`,
  `site-rejected`, `login-required`, `verification-failed`, `job-error`,
  `cancelled`, `interrupted`. Logs and artifacts are bounded per run (H1 sets
  the numbers). Pause stops issuing automation at the next event boundary.
  It never freezes the VM. A run's session launches with the resources its
  `[browsers.*]` entry resolves to, replacing the daemon's hardcoded launch
  values. Acceptance: a fixture crawl requires a login, enters
  `NeedsInteraction`, the operator logs in through the dashboard, it resumes
  in the same browser and produces a verified mirror; killing the daemon
  mid-crawl marks the attempt `Interrupted`, and `bctl resume` continues from
  Grab's `state.db`.
- **D2 Takeover and run views.** Owns `web/`, gateway input lease. Prereq: J3,
  D1. "Take control" acquires an exclusive input lease on the server. Without
  it, the gateway drops RFB input messages. Observers stay read-only.
  "Resume automation" releases the lease and sends `resume`. The run page
  shows inputs, resolved config, progress from events, attempts, logs, the
  outcome category, and artifacts. Captured HTML is served only as a download
  or from a separate sandboxed origin, never inline on the dashboard origin.
  "Take control" moves the session's authority (Decisions) to `Yielding`
  and acquires the lease only on `Human`; the page shows `Yielding` until
  then. Acceptance: a Playwright test where a second viewer's input does not reach
  the guest while the first holds the lease; the takeover and resume flow of
  J3 driven entirely from the dashboard.

### M3N: native automation

bctl's own CDP automation, so a coding agent can drive a session with only
`bctl` or its Rust API. It does not gate the first useful release. N1 has no
bctl prereq; N2 onward edit the daemon and start after J3 merges, to avoid a
second scheduler or conflicting daemon changes. Behavior references:
agent-browser's `cli/src/native/{cdp/client,snapshot,element,interaction,tab_binding}.rs`.
Correctness tests use local fixtures only, never live sites. Measurements
(warm command p50/p95, CDP calls per snapshot and action, snapshot bytes,
daemon idle CPU and RSS, memory after repeated snapshots) go into H1.

- **N1 CDP transport.** Owns `crates/cdp/`. Prereq: none. One persistent
  connection to the browser websocket: correlated request ids, event
  dispatch including target and frame lifecycle, flattened target sessions,
  typed structs for only the methods used, bounded queues, response sizes,
  and deadlines, ping/pong, and clean shutdown. A timed-out or cancelled
  request leaves no pending entry. A mutating command dispatched without an
  observed reply fails as `UnknownOutcome` and is never replayed after a
  reconnect. The crate takes an endpoint, knows nothing of SmolVM, and is
  owned per session generation, closing when the generation stops or is
  replaced. Acceptance: mock-CDP tests for reordered replies, interleaved
  events, dropped sockets, delayed and malformed messages, concurrent
  requests, and shutdown; one VM test attaching to a ready session.
- **N2 Targets.** Owns `crates/automation/`. Prereq: N1, J3 merged. Session,
  then target (CDP `targetId`, the tab identity), then frame and document,
  then element ref. List, create, close, and select page targets. The
  selected target is stored in SQLite with the session generation, reattached
  after a daemon restart if it still exists, and invalidated by a new
  generation. A vanished target fails `TARGET_GONE`; a popup or an externally
  activated tab never moves the binding. Stored diagnostics never keep URL
  userinfo, query, or fragment.
- **N3 Snapshots and refs.** Prereq: N2. Accessibility snapshots fetched in
  bulk (`Accessibility.getFullAXTree` plus DOM backend ids, never one round
  trip per node): interactive-only by default, full on request, structural
  pruning, depth limit, CSS subtree scope, role, name, value, and
  disabled/checked/selected/expanded, bounded text and JSON output, password
  values redacted. A ref is scoped to generation, target, frame, and
  document, maps to a backend node, survives same-document updates only
  while that node survives, and is never recycled. Anything else fails
  `STALE_REF`; refs are never re-resolved by similar text. Same-process and
  out-of-process iframes are both modelled.
- **N4 Locators and actions.** Prereq: N3. Locators: ref, CSS, role plus
  accessible name, label, placeholder, test id, from browser-computed
  accessibility. An ambiguous locator fails unless an occurrence is chosen.
  Actions: navigate, click, double-click, fill, type, press, hover, scroll,
  select, check, uncheck, wait (selector, text, URL, readiness; monotonic
  deadline; network idle is never the default), get text/title/URL/attribute,
  screenshot, and an explicit `eval`. A click resolves the node, checks
  visibility and enabled state, scrolls into view, finds a hit point, rejects
  covered or detached targets, and dispatches real `Input` events; it never
  falls back to `element.click()`. Dialogs, popups, detachment, and frame
  replacement never corrupt another target's state.
- **N5 CLI, socket, and batches.** Owns `crates/bctl/` additions. Prereq: N4.
  `bctl tab list|new|use|close` and `bctl page goto|snapshot|click|fill|press|wait|screenshot|eval`,
  with stable JSON results and error codes carrying the target id. A failed
  connection never starts a second browser. A batch is a JSON list run in
  order over one connection, stopping at the first failure and returning
  per-step results and timings; it is not a transaction and promises no
  rollback or exactly-once effects. The Rust API is public for future native
  jobs; no Python or TypeScript bindings.
- **N6 Authority.** Prereq: N5, D2. Implements the authority state from
  Decisions across native commands, the RFB lease, and J3's
  `NeedsInteraction`, with every allowed and refused transition tested.
  Taking control mid-command waits for the in-flight command to settle,
  keeps the browser, tab, and profile, and resumes cleanly.

- **N7 Actionability.** Prereq: N6. Actions wait, with a finite deadline,
  for the target to become actionable instead of failing at once, and a
  timeout names the condition still false. Preparing (resolve, check, scroll,
  hit-test) is retried until the deadline; dispatching input never is, and a
  lost reply after dispatch stays `UNKNOWN_OUTCOME`. A takeover during a
  wait, locator evaluation, scroll, or batch preparation ends it promptly
  without holding the page lock and can never race a new dispatch.
  Regression fixtures are adapted from Playwright's independently authored
  click tests (`microsoft/playwright` `tests/page/page-click.spec.ts`, pinned
  revision and test names, its notice kept): late layout, an overlay that
  disappears, fractional boxes and non-unit scale, a target detached or moved
  across documents during preparation, navigation or a popup after dispatch,
  and transport loss after input. Playwright is a fixture source, never a
  dependency. Acceptance: fixture observations prove the intended target got
  the input within the bound, with no fixed sleeps standing in for readiness.

Acceptance for M3N, on real VMs: snapshot and click a button; role and name
matching with implicit ARIA roles; fill with Unicode, select, check, and read
text back; duplicate names fail as ambiguous; hidden, disabled, and covered
elements fail; DOM replacement and navigation invalidate refs; a ref from
one tab cannot act in another; a closed selected tab never switches
silently; a popup leaves the binding alone; same- and cross-origin iframes
work; a dropped socket causes no duplicate side effect; native commands are
refused during `Human`; a daemon restart reuses no stale ref; `down` closes
the connection; and the D1, D2, and Shadowdriver attach tests still pass.
Deferred until measured (M6): snapshot deltas, conditional screenshots.

### M4: Tinyland as the appliance compositor

T0 and T1 have no bctl prereq and may start at any time, in parallel with
M1 to M3.

- **T0 Decouple from laputa.** Owns `guest/tinyland/`, `guest/seatd/`.
  No prereq. Keep laputa's names and markers. Add an
  `aarch64-unknown-linux-musl` section to `.cargo/config.toml`. Replace the
  QEMU/KVM Alpine x86_64 proof harnesses with one SmolVM arm64 harness under
  `guest/harness/`, then delete the QEMU scripts; laputa keeps its own.
  Acceptance: both crates' test suites pass in a Linux arm64 guest; the
  existing proof (foot renders real pixels, keyboard input changes them,
  unprivileged with direct device opens denied) passes on SmolVM.
- **T1 Pure-Rust seat.** Owns `guest/`. Prereq: T0. Replace the C libseat
  FFI in `tinyland/src/seat.rs` (its only `unsafe` module, linked
  dynamically) with a native seatd protocol client. Share the protocol types
  with `guest/seatd` through one crate, and make `guest/` a single Cargo
  workspace. Ship `seatd` from `guest/seatd` in the appliance image instead
  of Alpine's C seatd. Acceptance: the tinyland binary is static (no ELF
  interpreter, no `DT_NEEDED`); T0's proof passes against the vendored
  seatd; seatd's differential and adversarial suites stay green.
- **T2 Protocol census.** Owns `docs/compositor-contract.md`. Prereq: P1
  (rerun on P2 when it exists). Run Chromium under Sway with
  `WAYLAND_DEBUG=1` through the B1 fixture and a menu/select/dialog fixture.
  Record each global and request Chromium binds and uses, and which buffer
  path it presents through (wl_shm or linux-dmabuf). That settles the minimum
  Tinyland gap from the Baseline list. Acceptance: the contract lists
  required protocols with the evidence line for each.
- **T3 Tinyland browser support.** Owns `guest/tinyland/`. Prereq: T1, T2.
  Implement only what T2 requires. The known minimum is wl_pointer with
  cursor surfaces and absolute motion, buttons, axis, and drag; real
  xdg_positioner placement and xdg_popup grab and dismissal for menus and
  `<select>`; and one primary toplevel plus transient surfaces, with extra
  toplevels handled deterministically. Keep T0's proof green. Acceptance:
  B1 and D2 acceptance on a `helium-tinyland` image.
- **T4 Default switch.** Owns `images/`. Prereq: T3, R1. `helium-tinyland`
  becomes the default image. The Sway image stays as a diagnostic image.
  Acceptance: the full acceptance suite passes on both images.
- **T5 Compositor conformance.** Owns `guest/tinyland/` test tooling only.
  Prereq: T3. A bounded spike: can tinyland's event loop and virtual-output
  test mode be driven through WLCS's integration ABI (`canonical/wlcs`,
  `wlcs_server_integration`) without a compositor refactor? If so, run the
  applicable core-surface, xdg-shell and popup, focus, pointer, and
  buffer-lifetime tests, recording unsupported protocols apart from failures
  in claimed ones; if not, report why and port a smaller fixture set. WLCS is
  test infrastructure and never ships in the guest; its C++ runner does not
  justify glibc in product output. Input regressions to cover: shifted
  punctuation, modifier chords, key release on disconnect and takeover,
  repeat, popup grab and focus, fractional coordinates, surface destruction
  with input queued. References only: `quinnjr/kiosk-rs` (Smithay kiosk
  policy) and `patrickjaja/wlroots-bridge` (evdev/XKB offsets, modifier
  release on disconnect, logical vs physical pixels); neither replaces
  tinyland or kbvm.

### M4R: pure-Rust userland

Runs alongside M4. The Helium archive ships these system libraries:
NSS (`libsoftokn3`, `libfreebl3`, `libnssckbi`), GTK3 and its closure (glib,
pango, cairo, atk, libffi through `use_system_libffi=true`, libdbus per
CHB-4), Mesa (`libEGL`, `libGLESv2`, `libgbm`, `libgallium-*`,
`libwayland-egl`), `libxkbcommon` (`use_xkbcommon=true`), and ALSA
(`use_alsa=true`). `copy_system_closure` (`scripts/stage-runtime.py`) stages
the `SYSTEM_SEEDS` list in addition to the browser's `DT_NEEDED`, so being
shipped does not prove a library loads. Mesa, ALSA, and NSS are named
exceptions and GTK is being removed in chromium-build. The expected
remaining gaps are `libxkbcommon` and whatever outlives GTK.

- **R0 Closure census.** Owns `guest/audit/`, `docs/userland.md`. Prereq: P1.
  Walk the image and attribute every file to one of: Rust-owned (crate and
  source), an exception from the Decisions list, or a gap. For ELF files,
  record `DT_NEEDED`, the interpreter, and whether the file is static; also
  record what Helium `dlopen`s, from a traced run of the B1 fixture.
  Reuse chromium-build's recursive ELF audit (`scripts/linux_runtime.py`)
  rather than writing a second one. Acceptance: running the census on any
  image lists every gap with its bytes and its consumer; an unattributed
  file fails the run.
- **R1 Scratch rootfs.** Owns `images/`, `guest/appliance/`. Prereq: R0, T1.
  The product image has no Alpine base, busybox, or shell. It contains
  `krun-init`, the SmolVM agent, `guest/seatd`, `tinyland`, a Rust
  `bctl-guest` that supervises seatd, then tinyland, then the browser, and
  reports readiness, plus the Helium archive, its fonts, and musl. The Sway
  diagnostic image keeps busybox for debugging. Acceptance: R0 on the
  product image shows gaps only inside the Helium runtime closure; T0's
  proof and B1 pass.
- **R2 GTK leftovers.** Prereq: R0 on a GTK-free Helium archive. Owner
  2026-11-08 removed GTK from every Helium Linux build, handed to the
  chromium-build arm64 work. File chooser, print, audio, and GPU
  rasterization are accepted losses. R2 reports what survived through
  Chromium's own `use_glib`, `use_atk`, and `use_system_libffi` (glib, atk,
  at-spi, libdbus, libffi). It does not change those settings without an
  owner decision. Acceptance: R0's gap list for the archive contains only
  `libxkbcommon` and the leftovers R2 named, each with the GN argument that
  brings it in.
- **R3 libxkbcommon on kbvm. Cross-repo (laputa-systems/kbvm).** Prereq:
  R0. The fork matches upstream `mahkoh/kbvm` today, and its `libxkbcommon`
  directory is upstream C, kept as a test oracle. Add a crate to the fork's
  workspace that builds `libxkbcommon.so.0` with the C ABI, limited to the
  symbols Helium imports (from `nm -D` on the browser plus R0's dlopen
  trace). Under Wayland that is mainly keymap-from-string, state update, and
  keysym and UTF-8 lookup, plus compose if Helium imports it. Point
  `guest/tinyland`'s `kbvm` dependency at the same pinned fork revision, so
  the compositor that serializes the keymap and Helium, which parses it,
  share one implementation. Acceptance: the fork's tests plus a differential
  suite against the oracle submodule for every exported symbol; Helium in B1
  maps the kbvm library, and the fixture records correct text for shifted
  keys, modifiers, key repeat, and a dead-key compose; R0 reports no
  `libxkbcommon` gap.
- **R4 Remaining gaps.** Prereq: R2, R3. For each gap still reported, ship a
  Rust provider with the exact C ABI (zlib-rs as `libz.so.1` if a dynamic
  zlib consumer remains), or bring the owner a named exception with its
  reason. Acceptance: R0 reports zero gaps, and the `docs/userland.md`
  exception list matches Decisions.

### M4L: lean appliance Helium (cross-repo, chromium-build, linux/arm64)

The release profile already sets `is_official_build`, `symbol_level=0`, PGO,
ThinLTO, `enable_widevine=false`, and `enable_remoting=false`, and Helium
already carries `helium-no-dbus.patch` and `helium-no-devtools-frontend.patch`.
The headless Chromium profile's `config/patches/headless-no-*.patch` set
(crashpad, printing, v4l2, rankers, domain reliability, and others) is a
tested catalog to port from. Every L item reports its measured delta against
L0; keep a change only if it moves a number by more than the run-to-run spread.

- **L0 Lean baseline.** Owns `docs/performance.md` (Helium section). Prereq:
  the arm64 archive. Record archive bytes by component (browser binary,
  `.pak` resources, locales, fonts, `lib/` by library), cold launch to first
  frame, idle browser RSS on `about:blank` and on the B1 fixture, and the
  process count. Use the H1 method and the `perf-gate-ready` rule.
- **L1 Audio off, faked.** Prereq: L0. This is the owner's handoff of
  2026-11-08. Set `use_alsa=false`, so no `libasound` and no backend ship.
  Chromium's fake audio manager reports output parameters and an
  `enumerateDevices()` list taken from a command-line switch. A patch gated
  behind a new GN arg, enabled only on arm64, adds that. The defaults are
  measured from a real Linux Chrome 156 on PipeWire or Pulse. Labels never
  reveal the fake backend, and a configured input device captures silence.
  Acceptance: the five package checks; no `libasound` or `libpulse` in the
  audit; a fixture where `<video>` with audio plays to `ended` in real time to
  within 5%, `AudioContext` reaches `running`, and `sampleRate`,
  `baseLatency`, `outputLatency`, `maxChannelCount`, and the device
  kinds and counts match the switch exactly and the defaults without it; an
  empty amd64 `gn args --list --short` diff.
- **L2 Media persona in Shadowdriver. Cross-repo (shadowdriver).** Prereq:
  L1. The per-profile identity carries an audio persona (sample rate, frames
  per buffer, channel layout, input and output device counts), and launch
  options render it as L1's switch. Shadowdriver's launch-args fixture
  (`tests/fixtures/chromium_launch_args.json`) pins the result. bctl passes
  the persona through `bctl.toml` `[browsers.*]` and records it in the run
  receipt. Video devices join this persona in L3. Acceptance: the fixture
  test; L1's fixture passes for two different personas from one binary.
- **L3 Web-invisible removals.** Prereq: L0. Port from the headless catalog:
  crashpad, the rankers, domain reliability, and remaining background
  services. Also `enable_printing=false`, an accepted loss, with
  `window.print()` still present as a no-op. And v4l2 off, with video-input
  devices faked through L1's mechanism and L2's persona. Each removal is its
  own commit with its L0 delta. Acceptance: the five package checks and B1
  pass after each removal, and `window.print` still exists.
- **L4 Resource trim.** Prereq: L0. Ship only the locales the supported
  personas use. Drop `.pak` resources the browser never loads under the
  appliance's flags, judged from a traced run, not by name. Keep fonts
  persona-complete, not minimal: a short font list is itself a fingerprint.
  Acceptance: archive bytes reduced; B1 and D2 pass; the locale fixture for
  each supported persona renders the browser's own UI strings.
- **L5 Shared base image.** Owns `images/`, `crates/runtime-smolvm/`.
  Prereq: R1, H1. Ship the appliance as one read-only base, such as erofs,
  shared by every session, plus a small writable layer per session and the
  per-profile disk. Acceptance: two sessions on one host, with their summed
  RSS and disk use measured against separate full images; no session can
  write to the base.
- **L6 Honest persona baseline.** Owns `docs/persona.md`. Prereq: L1, L3.
  Find out whether Google's Chrome 156 for arm64 Linux ships Widevine. If
  it does, record the gap. Inside the appliance, capture the
  page-observable surfaces Shadowdriver's identity layer covers, plus
  audio, media devices, WebGL strings, fonts, and EME key systems. Check
  them for internal consistency (UA platform, Client Hints, CPU count,
  screen, and timezone all agree). Then record pass, challenge, or block on
  a fixed set of public fingerprint and bot-detection test pages, using
  X4's outcome categories. Acceptance: `docs/persona.md` lists every
  surface with its value and source, no inconsistency stays unexplained,
  and the test-page results are recorded with their dates as the baseline
  any later persona change must beat.

### M5: hardening and measurement

- **H1 Gates and baselines.** Owns `docs/performance.md`, `tests/bench/`,
  and a `bctl-bench` runner. Prereq: none for the harness and the lifecycle
  and browser measures (owner 2026-11-08: track performance as the POC
  matures, competitive with other browser runtimes); the run and takeover
  measures follow M3. One command runs a named suite and writes a dated
  JSON result with the host, image identity, browser version, and commit;
  `docs/performance.md` holds the latest table and the gates. Measures:
  - Lifecycle: cold `up` to ready, split by readiness stage (machine, agent,
    compositor, CDP, first non-black frame); warm `up` (image cached,
    profile warm); `down` latency; `smolvm machine pause` and `resume`
    latency and the page state after resume (same tab, same JS heap, CDP
    reattaches); checkpoint restore once X1 exists.
  - Browser: Speedometer 3.x run headed in the guest (score and spread),
    plus JetStream and MotionMark as secondary scores; page-load timing of
    a local fixture from the Navigation Timing API. The comparison is Linux
    arm64 Chromium in a container on the same Mac, the container-browser
    class bctl competes with; a native macOS browser is not a baseline. Rerun on Helium (P2) and on each image (Sway, Tinyland).
  - Interaction: input-to-pixel latency through the gateway (RFB input to a
    changed framebuffer), viewer frame rate and bandwidth, CDP round trip
    through the capability gateway versus inside the guest.
  - Automation: `crates/cdp` transport counters modelled on Shadowdriver's
    `src/cdp/metrics.rs` (commands started, completed, timed out,
    cancelled; pending high-water; bytes and messages each way; largest
    message; JSON parse time; late responses; subscription overflows), M3N's
    warm command p50/p95 and CDP calls and bytes per snapshot and action,
    and Grab pages per second on a local fixture site.
  - Resources: host RSS and idle CPU per session (VMM plus guest), daemon
    RSS, profile disk growth, image size, and `bctl` binary size.
  Gates come from the observed spread; time gates follow the
  `perf-gate-ready` rule with interleaved baseline and candidate runs, and
  memory and size gates always run. Also set the log, artifact, and disk
  bounds. External numbers (for example Kernel's published 30 ms
  snapshot resume on Unikraft Cloud) are cited as targets, never as
  measurements of bctl.
- **H2 Authority.** Prereq: M3. Put CDP behind the gateway with a per-run
  capability; the gateway rewrites `/json/version`'s `webSocketDebuggerUrl`
  so `control_url` attachment works unchanged. Restrict guest egress to the public internet by default, with an
  explicit allow for private ranges. Confirm no host home, SSH agent, or
  keychain reaches the guest. Acceptance: tests that a job without the
  capability cannot attach, and that the guest cannot reach a loopback-bound
  host service or an RFC 1918 address unless allowed.
- **H3 Failure injection.** Prereq: H1. Cover killing the browser, the
  compositor, the VM, and the daemon; filling the disk; and dropping the
  viewer, each during a run. Every case ends in a named state with no leaked
  VM, port, or lease. Add retained-failure expiry and storage GC.

### M6: deliberate later work (each starts with a measured question)

- **X1 Template restore.** Can a credential-free appliance checkpointed before
  Chromium starts (`smolvm machine checkpoint`/`branch`) cut cold-ready time
  past H1's spread without weakening the sandbox? Restoring a running browser
  is a separate later question. Existing CDP connections are never promised
  to survive a restore.
  Blocked today (H1, source-read 2026-11-09): SmolVM's one capture gate,
  `validate_capture_profile` (`src/portable_checkpoint.rs:3944-3994` in
  niceforge's vendored 1.31.4), refuses exposed sockets, local-archive image
  layers, and `--gpu`, which every session uses. The GPU refusal is a real gap:
  libkrun's virtio-gpu saves only transport state, and rutabaga snapshots 2D
  only, while SmolVM's `--gpu` enables virgl and Venus. The other two are mostly
  policy: virtio-fs and block devices already save and restore, and vsock saves
  its queues but drops live connections. The bounded path, if X1 is taken up:
  run the display as a 2D-only virtio-gpu (bctl renders on the CPU anyway),
  save and restore rutabaga's 2D resources and scanout in a libkrun patch, and
  relax the gate for 2D GPUs and idle exposed sockets, which means building
  SmolVM from source instead of taking the release bundle by digest (an owner
  decision). virtio-input has no save arm and is silently skipped. Branch
  (`FORK`) bypasses the gate and is unverified.
- **X2 Alternative backends.** A unikernel-style or other VMM backend
  (Unikraft-style library OS, Firecracker/Cloud Hypervisor on Linux) must beat
  SmolVM on H1's measures while passing the full acceptance suite unchanged.
  Stock Nanos, assessed 2026-11-08 (`docs/research/unikernel-nanos.md`), was
  not viable: no process creation, namespaces, seccomp, DRM, or evdev. The
  hydrogen fork (`~/d/hydrogen`) adds processes, namespaces and seccomp and
  runs Helium headless with its sandbox on; M7N integrates it. Unikraft, assessed 2026-11-08 (`docs/research/unikernel-unikraft.md`), is not viable: Chromium examples need the unpublished `base-compat` Cloud kernel, and stock Unikraft has vfork/execve only, with no fork, namespaces, seccomp, DRM/KMS or evdev.
- **X3 Remote contexts.** A remote `bctl` daemon on a Linux KVM host using the
  amd64 Helium artifact, with the same API and ownership model. Jobs reach its
  sessions through its gateway's CDP endpoint, or run on that host when H1's
  round-trip measurements say the distance costs too much.
- **X4 Site access evidence.** Run outcome categories feed a per-site record of
  which configuration (profile warmth, egress, flags) passed or was
  challenged, with sample counts and dates. This measures task completion,
  not page loads. It covers only sites the operator is entitled to automate.

### M7N: Helium on Nanos (Linux amd64, headless)

Owner 2026-11-10: the agent owns both bctl and hydrogen; the first delivery
is a Nanos runtime backend, so `bctl up --runtime nanos` boots hydrogen's
Nanos with Helium r5 amd64 headless on a Linux KVM host and every headless
surface works against it (CDP gateway, `bctl page`, tabs, runs) and the
bench suites can compare it with SmolVM. No live view, VNC input or human
takeover on Nanos: guest graphics are a hydrogen non-goal, so Nanos sessions
are automation-only and `bctl view` and the input lease refuse them with
`UNSUPPORTED_BY_RUNTIME`. (Superseded 2026-11-10 by the owner: headed Nanos is
wanted; M7H plans it, and until it lands these refusals stand.)

Decisions (prescribed; the owner may overrule):

- **CDP transport.** virtio-vsock through rust-vmm's `vhost-device-vsock`
  (0.3.0 from crates.io, checksum-pinned, built with `--locked` and
  `runtime/patches/vhost-device-vsock-0001-forward-guest-half-close.patch`,
  without which a guest's `shutdown(SHUT_WR)` never reaches the host stream
  and a WebSocket close through the gateway never completes), attached as QEMU
  `vhost-user-vsock-pci`. Its host side is a Unix socket in the session's
  private directory, so H2's rule that only a Unix socket reaches CDP holds
  and no host kernel module is needed. Spike 2026-11-10 against hydrogen
  `aaa0475a`: hybrid `CONNECT` handshake, 35 µs median round trip, repeat
  connections, 16 MiB at 14 MiB/s.
- **Kernel and tools.** `runtime/build-nanos.sh` exports hydrogen at the
  commit it pins (`git archive`, never a write into hydrogen) and builds the
  kernel, boot image and `mkfs` into `.work/runtime/nanos`, plus
  `vhost-device-vsock`. QEMU is the host's.
- **Guest program.** `bctl-guest run-headless` (the Rust `guest/appliance`
  crate) starts Helium with `--headless=new`, the sandbox on, the profile at
  `/profile`, and relays vsock port 9222 to the browser's loopback CDP port;
  `bctl-guest check` answers on vsock port 9223 with `browser`, `sandbox` and
  `cdp` stages and `input absent`.
- **Image.** `images/nanos-helium/build.sh` builds a Nanos disk image from
  the r5 `linux-x86_64-musl` archive with the same archive checks as the
  arm64 image (sidecars, chromium-build's installer, ELF audit for x86-64).
- **Profile.** virtio-9p (`-virtfs`, tag `profile`) mounted at `/profile`;
  if hydrogen's 9p cannot carry a Chromium profile, the profile lives on the
  image's disk and does not outlive the session, recorded as a gap.
- **Egress.** The guest has no network device. Helium uses
  `--proxy-server=socks5://127.0.0.1:1080`; `bctl-guest` relays that port
  over vsock to the host, where the machine's supervisor answers SOCKS5
  CONNECT, resolves names itself, maps `100.96.0.1` to host loopback as
  SmolVM's gateway does, and refuses every non-public address (loopback,
  RFC 1918, link-local, CGNAT, unique-local IPv6 and the rest) unless an
  `--allow-private` range covers it, which keeps H2's egress rule.
  (Superseded 2026-11-10: QEMU's user network with `guestfwd` would have
  needed a per-connection process and an HTTP proxy that tracks keep-alive
  requests; SOCKS5 over vsock is one tunnel per connection and leaves the
  guest no NIC.)
- **Selection.** A host runs one runtime: `BCTL_RUNTIME=smolvm|nanos`, else
  SmolVM on macOS and Nanos elsewhere. No `--runtime` flag and no store
  migration until SmolVM runs on Linux too (superseded 2026-11-10: a
  per-session flag had no host that could use both). Pause, resume and
  templates answer `UNSUPPORTED_BY_RUNTIME` on Nanos; the viewer answers
  that a headless session has no display. On Linux the default `BCTL_HOME`
  is `$XDG_STATE_HOME/bctl` or `~/.local/state/bctl`, and the Nanos runtime
  lives in `$BCTL_HOME/runtime/nanos` (`kernel.img`, `vhost-device-vsock`);
  `bctl` itself is the supervisor (`bctl nanos-vm`), and the default image
  is `nanos-helium` (`$BCTL_HOME/images/nanos-helium.img`).

Slices, in order, one writer:

- **L0 Linux host build.** `cargo test --workspace` and the dashboard build
  pass on Linux amd64; the new-host checklist names `sqlite-static`.
- **L1 Runtime and image.** `runtime/build-nanos.sh` and
  `images/nanos-helium/build.sh`; acceptance: the image boots under QEMU and
  `/json/version` answers `Helium/0.20.3.1` through the vsock Unix socket.
- **L2 Adapter.** `crates/runtime-nanos` launches, observes, checks readiness
  (machine, guest, browser, sandbox, cdp) and tears down;
  acceptance: its `--ignored` lifecycle test, 10/10.
- **L3 Daemon.** Runtime selection, the store migration, and the
  unsupported-operation refusals; acceptance: the `sessions`, `tabs`,
  `actions`, `snapshots` and `failures` suites pass with
  `BCTL_TEST_RUNTIME=nanos`.
- **L4 Egress.** The proxy and `--allow-private`; acceptance: the `egress`
  and `host_http` suites pass on Nanos.
- **L5 Measure.** `bctl-bench lifecycle|interaction|browser|resources
  --runtime nanos` with the same measure names, Speedometer run headless and
  labelled so, and a Nanos column in `docs/performance.md`. The SmolVM
  numbers are from the Mac until SmolVM runs on Linux, and the table says so.

### M7H: headed Helium on Nanos (Linux amd64)

Owner 2026-11-10: headless Nanos is a step, not the end state; Nanos
sessions should get the live view, VNC input and human takeover that SmolVM
sessions have. Hydrogen's "no guest graphics" non-goal is lifted for this
milestone. Nothing on the Nanos path draws pixels today: `run-headless`
starts no compositor, and hydrogen has no virtio-gpu, virtio-input, DRM or
evdev support. SmolVM's headed path relies on all four through the Linux
guest kernel (libkrun's virtio-gpu and virtio-input, tinyland on DRM and
libinput through seatd).

Slices, in order, one writer:

- **NH0 Investigation (a lane may start now).** Owns a design note in
  `docs/`, no code. Compare at least these routes and measure what each
  needs:
  1. Linux parity: a Nanos virtio-gpu 2D driver behind the DRM KMS
     dumb-buffer ioctls tinyland uses, and a virtio-input driver behind
     evdev for seatd/libinput, so the SmolVM guest stack runs unchanged. It
     is the largest kernel surface; census the exact ioctls tinyland, seatd
     and libinput issue on SmolVM first.
  2. A remote output for tinyland: tinyland keeps Helium on Wayland but
     sends damaged regions of its output over vsock to the host encoder,
     and the host sends input back over vsock, so the guest needs no
     graphics or input devices. The kernel only needs what Chromium's
     Wayland client already uses (AF_UNIX with `SCM_RIGHTS`, memfd, shared
     mmap). Settle whether the host VNC encoder can take frames from vsock
     instead of a virtio-gpu scanout.
  3. CDP screencast as a stopgap: `Page.startScreencast` and
     `Input.dispatch*` on headless Helium, with no guest change. It shows
     page content only (no browser UI, no native popups such as `<select>`)
     and sends JPEG per frame; record its latency against H1's gate so the
     comparison is honest.
  Acceptance: the note picks one route with evidence (syscall or ioctl
  census, an estimate of kernel lines, a throwaway spike for the riskiest
  step), names the input-to-pixel and idle-CPU budgets from H1 and
  `docs/performance.md` that the route must meet, and splits the work into
  NH slices.
- **NH1 onward.** Written from NH0's note. Acceptance for the milestone: the
  viewer and input-lease suites pass with `BCTL_TEST_RUNTIME=nanos`, and
  `bctl-bench viewer` gives Nanos a column in `docs/performance.md`.

### M7M: Nanos on Apple Silicon (macOS arm64, Hypervisor.framework)

Owner 2026-11-10: Nanos sessions should run on macOS too, at hardware speed.
Hypervisor.framework only runs guests of the host's architecture, so this
means an arm64 hydrogen kernel and an arm64 musl Helium. x86-64 Nanos under
emulation (QEMU TCG) is not a candidate for Chromium. The VMM is libkrun,
which bctl already vendors for SmolVM (`third_party/smolvm/libkrun/`); QEMU
with `-accel hvf` is the fallback if libkrun's device model blocks a slice.
The product contract stays the same as on Linux (CDP only over a Unix
socket, the egress policy, disposable disks, readiness stages); the binaries
and the VMM differ.

Prior art: `tsirysndr/bsdkrun` boots a hello-world Nanos to userspace under
libkrun on M-series Macs (three consecutive boots reported 2026-08-12).
Stock Nanos 0.1.55 fails. Its fork `tsirysndr/nanos` branch
`fix/aarch64-libkrun-boot` (5 commits) fixes, in order: virtio-mmio
enumeration from the DSDT (`platform/virt/service.c`), DC ZVA before the
MMU is on (`src/runtime/memops.c`), dirty caches handed over by the EFI
loader (`src/aarch64/uefi.c`), GICv2/v3 selection when HVF does not
virtualize `ID_AA64PFR0_EL1.GIC` (`src/aarch64/gic.c`), and virtio-mmio IRQs
routed to the DSDT's SPI (`src/virtio/virtio_mmio.c`). Its libkrun fork
(branch `feat/pvh-boot`) also lowers the in-kernel GIC's level SPI once the
guest acknowledges the interrupt; without that the guest takes about 35k
empty interrupts a second. The boot implementation is
`bsdkrun/core/src/nanos.rs`.

Slices, in order, one writer, each gated on the one before:

- **MM0, 2026-11-10: passed with an additional kernel fix.** On an Apple M5 Max, macOS 27.0.1
  (26A434), bsdkrun `b31ff8ebed68d0ea9cba88345f4a99dc13c01fb7`,
  Nanos `96d7959482110b5715f0762d806f308994f72d12`, and libkrun
  `c9a695a88d6240214fafbfbd438bfc2851d24ccc` produced one hello line
  under Hypervisor.framework. The next two boots failed. The kernel enabled
  its MMU before translation invalidation completed, causing an instruction
  permission fault. Adding the missing completion barrier produced ten
  consecutive hello boots without diagnostic instrumentation.
  The arm64 kernel and EFI loader were built in Debian bookworm on Docker;
  the hello disk used Nanos `mkfs` and the bsdkrun example's static hello.
  The fork's `mkfs` reads `SCTLR_EL1` from userspace and faults on arm64.
  For this attempt only, its scratch build assumed the MMU was enabled;
  the guest kernel and loader used the unmodified fork. libkrun was built
  with the `efi` feature, and bsdkrun used libkrun's `KRUN_EFI.silent.fd`
  and `--no-net --mem 1024`. The local arm64 musl Helium archive has an acceptance
  record, but has not been tested with hydrogen on arm64.
- **MM1, 2026-11-10: passed.** Hydrogen `0702048a` contains the kernel boot fixes.
  The x86-64 KVM harness passed on Lisa, and an arm64 seccomp probe passed
  under QEMU with PCI and modern MMIO disks. Three HVF boots of that probe
  passed with bctl's patched libkrun and the reference EFI firmware. The SPI
  de-assert is `runtime/patches/libkrun-0003-arm-mmio-interrupts.patch`.
  `runtime/build-nanos-krun.sh` bundles the pinned EFI firmware, and
  `runtime/build-nanos-arm64.sh` builds the pinned kernel. Known limitation:
  under QEMU with `-accel hvf`, a virtio-PCI disk reaches the EFI loader and
  stalls; the same image boots with virtio-PCI under TCG and with modern
  virtio-mmio under HVF. bctl's libkrun path uses virtio-mmio.
- **MM2, 2026-11-10: passed.** Hydrogen `c19d26f0` and `25b59205` add
  arm64 fork and exec; `134bef82` fixes seccomp trap delivery. The arm64
  Chromium probes passed 55 and 6 checks on native Linux and on HVF with
  one, two and four configured vCPUs. A diagnostic image booted Helium
  0.20.3.1-r6 on HVF and passed all 10 Hydrogen Helium checks, including
  renderer seccomp, PID namespaces, confinement and CDP navigation.
  Hydrogen `f5098a77` makes `dev/test.sh` run on the Mac: builds and native
  oracles run in an arm64 Linux container, and images boot through bctl's
  libkrun under HVF. There, `harness` passed all 85 checks, `probe` passed
  55 and 6 checks natively and at one, two and four vCPUs, and `helium` and
  `helium-native` passed all 10 records with Helium 0.20.3.1-r6, including
  `sandbox_confinement`. On threadripper the x86 `harness`, `probe` and
  `helium` suites passed at the same commit; `helium-native` fails there on
  `main` as well (`cdp_evaluate`, `cdp_dom`, `sandbox_confinement`).
- **MM3, 2026-11-10: passed.** Nanos profiles on macOS persist on a
  per-profile disk that the VM locks, and stopping a session waits for the
  guest to flush it. Persistent boots stalled intermittently: two threads
  faulting the same writable program page left the page cache's node lock
  held. Hydrogen `12600ce1` fixes this and adds `private_page_race`, which
  hung every boot before the fix and passed 15 HVF boots at one, two and four
  vCPUs after it. Hydrogen's x86 harness passed on threadripper. On an Apple
  M5 Max with that kernel, `BCTL_TEST_RUNTIME=nanos` passed the sessions,
  tabs, actions, snapshots, failures, egress, host_http and profiles suites
  (24 tests) and the runtime-nanos lifecycle suite. The owner waived the
  `docs/performance.md` column. M7M is complete.
- **MM0 Native boot (needs the Mac).** Reproduce bsdkrun's `nanos-hello`
  on Apple Silicon with acceleration confirmed (HVF, not TCG) before any
  port. Acceptance: three consecutive boots print the hello line, and the
  record names the host, macOS version and the commits used.
- **MM1 Boot fixes in hydrogen.** Port the five kernel fixes into
  hydrogen's fork instead of keeping a second Nanos, and the SPI de-assert
  into bctl's libkrun patches (`runtime/patches/`). Hydrogen's x86-64
  harness must stay green, and the image must still boot on QEMU arm64
  with both virtio-PCI and modern virtio-mmio disks, as bsdkrun checks.
  Acceptance: hydrogen's own arm64 boot of a probe image under bctl's
  libkrun on the Mac.
- **MM2 Hydrogen's Chromium support on arm64.** The large slice. Fork,
  execve, address-space isolation, seccomp, namespaces and the rest of
  hydrogen's G1 to G4 work are proven only on x86-64 (for example
  `src/x86_64/unix_machine.c`); each needs its arm64 half and hydrogen's
  native-oracle and harness cases on arm64. Nothing about the renderer
  sandbox may be relaxed for arm64. Acceptance: hydrogen's harness and
  Helium suites on arm64, with an arm64 musl Helium (P2's arm64 archive
  from chromium-build) reaching `sandbox ok`.
- **MM3 bctl backend.** A Nanos-on-libkrun runtime beside SmolVM on macOS.
  The Linux backend's CDP and egress run over QEMU's vhost-user-vsock;
  libkrun's vsock with its Unix-socket port mapping is the likely
  replacement, settled in this slice. Acceptance: the M7N suites with
  `BCTL_TEST_RUNTIME=nanos` on the Mac, plus a Nanos-on-macOS column in
  `docs/performance.md`.

Headed sessions (M7H) stay out of this milestone until headless arm64
Helium works.

### M8C: external browser clients (browser-use 1.0.0 and agent-browser v0.42.0)

**Status: E1 to E3 passed on macOS arm64, 2026-11-10; Linux x86_64 unverified.** This
extends bctl's *consumer* surface; it does not replace M3N's native automation,
Shadowdriver, Grab, or the active M7N work. See
[the pinned integration research](research/external-clients.md). The initial
release target is compatible **external CDP attachment**, not upstream feature
parity, a substitute browser, a new agent framework, or a hosted service. A
source-reviewed integration is not "supported" until its real-client tests pass.

**Ownership boundary.** bctl alone owns Helium, the VM, profiles, readiness,
CDP capabilities, run records, retained artifacts, and reconciliation.
browser-use and agent-browser own their automation, REPL/model loops, page
bindings, selectors, and element references. They run on the host, not inside
the appliance. `exec` runs have the **host user's authority**; a sandboxed
browser does not sandbox model-written Python or another host-side agent.
Nothing from these clients is linked into the Rust daemon or guest image.

**Fixed upstream pins for first qualification:**

| Consumer | Revision | Connection |
| --- | --- | --- |
| browser-use 1.0.0 | `070be98109c03f04a95f704533e8f6e9868d0525` | `Browser(cdp_url=...)`, explicitly passed to `Agent` |
| browser-harness 0.1.13 | pinned by browser-use 1.0.0 | owns the client-side persistent CDP/REPL daemon |
| agent-browser v0.42.0 | `e7519957b9067c541825afa21a792fbe1f483047` | generic `--cdp`, then optional `browser.provider` plugin |

Upgrade pins only as deliberate, separately evidenced compatibility work.
Tests use Helium, with reference Chrome permitted only as a labelled diagnostic
control. M8C starts once M7N's L5 lands, one writer at a time; its VM proofs
never overlap a timed bench on the same host. The clients are host-side test
tooling under `.work/interop/`, never product dependencies, so the guest purity
rules do not apply to them: browser-use 1.0.0 in a virtualenv, agent-browser
built from its pinned tag with `cargo`. Linux x86_64 with Nanos qualifies first;
the macOS arm64 and SmolVM leg follows on the Apple Silicon Mac.

**E0 — Contract census (done).** `docs/research/external-clients.md` at
`bc6462a` is the census: pins, call sites, and the source-verified matrix. E1
moves rows from source-verified to runtime-verified or blocked as it runs them.
Python on this host resolves `<token>.localhost` to both `127.0.0.1` and `::1`
(checked 2026-11-10); the clients' own resolvers are still E1's to test.

**E1 — Released-client endpoint proof.** Prereq: the existing H2 gateway.
Own `tests/interop/` (one `run.sh` that installs the pinned clients under
`.work/interop/`, starts a session, runs both clients and prints the receipts)
and only the gateway fixes required by
failing tests. Run each released client against a real bctl session; exercise
`/json/version` HTTP discovery, the advertised browser WS URL, access to
`Browser.getVersion`/Target APIs, tab navigation, a fixture form fill/click,
accessibility, actual screenshot bytes, browser dialog, popup, same- and
cross-origin iframe, and client detach followed by another attach. For
browser-use run both `mode="default"` and `mode="ultrafast"`; its domain policy
adds a second CDP connection, which must also attach and detach. Prefer the
pinned upstream scripted Pydantic-AI `FunctionModel` test technique to avoid
paid model calls. Agent-browser runs deterministic CLI commands without an LLM.
Do not infer interoperability from bctl's own `Endpoint::dial_target`:
its special `*.localhost` mapping may not exist in Python or upstream Rust.
Verify DNS, proxy variables/NO_PROXY, Host header, Origin rejection and
capability revocation on each supported host. If resolution fails, add the
smallest safe loopback CDP URL representation that works with both consumers
(e.g. a scoped path/query token). Preserve revocation; never expose guest CDP
or silently disable auth. HTTP-discovery path mangling is not repaired by
adding only a browser-WS endpoint, and a WS endpoint does not cure DNS.
**Acceptance:** exact bctl/image/runtime/upstream version receipts; 10/10
repeated connections and reconnects per consumer on qualified runtime,
consistent successful real-page operations, and an invalid/revoked endpoint
cannot attach. A missing test host is "unverified", not "passed". Command:
`tests/interop/run.sh`, which exits non-zero on any failed check.
**E1 result, 2026-11-10:** on the Apple M5 Max, all 55 checks passed on SmolVM
and on Nanos, with no gateway change needed; the findings are in
[the census](research/external-clients.md#runtime-results-e1). The Linux
x86_64 Nanos run is unverified.

**E2 — External controller leases and lifecycle.** Prereq E1. Own
`crates/core/`, `crates/bctl/src/{gateway,authority,runs}.rs`, and corresponding
tests. Start with one active *logical controlling owner* per session; allow
multiple CDP WebSockets for the same owner (browser-use's domain guard needs
one). The owner is the capability: every socket opened through one `cdp-url`
token belongs to one controller, and a run's `BCTL_CDP_URL` is its own token.
Do not confuse a capability with an owned session: distinguish
`borrowed/attach-existing` (client disconnect leaves the browser alive)
from `managed-ephemeral` (bctl owns and eventually reclaims the exact
session generation). Track client/attempt/generation, capability issuance,
release and expiry; serialize competing native mutations and external clients.
Today human takeover only blocks **new** CDP attachments and cannot stop
commands on pre-existing sockets; until a correct cooperative protocol exists,
**refuse takeover while an external controller owns a live lease**, with a
remedy to release/cancel it. Existing untracked CDP attachments must be
documented as unsafe for exclusive handoff, not silently treated as lease
holders. Forced revocation, if implemented, shuts *all* of the owner's CDP
sockets, reports interrupted/unknown-outcome effects, and never retries
dispatched input. Noncooperative `exec` is not pausable. Client failure, host
job cancellation, daemon `kill -9`, VM crash, double close and stale cleanup
must never kill a newer generation or leak a permanent lease. Do not rely
on agent-browser's best-effort `browser.close` plugin callback alone.
**Acceptance:** deterministic ownership/authority transition tests, two
client sockets under one lease, competing client refused, human takeover
refused until release, idempotent cleanup, no browser termination for a borrowed
session, and proper reconciliation after crashes.
**E2 result, 2026-11-10:** the session's authority holds at most one
external controller, named by its capability token, from its first gateway
socket to its last (`docs/automation.md`, External controllers). Another
URL's attach and a dashboard takeover get 409 while it is attached. Native
commands still run: refusing them broke existing users of an observing CDP
client, and bctl cannot order them against an opaque socket anyway.
`authority::tests` cover the transitions. `cdp::e2_one_controller_holds_the_session_across_its_sockets_until_the_last_closes`
passed on SmolVM and Nanos on the Mac. It covers two sockets under one lease,
a refused competitor and takeover, release on the last close, revocation,
and a `kill -9` daemon restart with the browser kept. The interop suite
still passes all 55 checks. `authority` n7 now watches inputs through the page
instead of an outside CDP client, which would hold the session. Only the
borrowed kind exists: a managed-ephemeral session arrives with its first user
(E3 or E5). Linux x86_64 is unverified.

**E3 — Host-side managed recipes and run evidence.** Prereq E1 (managed
ownership uses E2). Own `docs/jobs.md`, focused `examples/` or `tests/interop/`,
and only justified `exec` adapter changes. Reuse `BCTL_CDP_URL`,
`BCTL_OUTPUT_DIR`, `BCTL_RUN`, ordinary programs, and run outcomes; do not
invent a workflow DSL, Python binding to bctl, new scheduler, or second job DB.
Browser-use requires explicit `Browser(cdp_url=...)`, not the default cloud
`Browser()`. Test whether its `Agent` actually runs without provisioned
Browser Use Cloud credentials using a scripted model. Agent-browser uses a
unique client-side session per run and an explicit CDP endpoint. A connection
failure cannot fall back to Chrome, a different profile, or a paid provider.
For both, report exact image/runtime provenance and ensure closing the host
client is not equivalent to `bctl down`. Keep endpoints and tokens out of logs,
artifacts, and UI. bctl's existing run-output bounds apply.
**Acceptance:** each pinned client completes a deterministic bctl `exec`
job, gets a typed result/correct output and cleanup evidence; interrupted
clients do not restart side-effectful operations automatically.
**E3 result, 2026-11-10:** `tests/interop/recipes/` holds a browser-use and an
agent-browser exec program, documented in `docs/jobs.md`. On the Mac,
`tests/interop/run.sh` runs each pinned client as a `bctl run` job against the
fixture, with every proxy pointed at a closed port and no Browser Use key.
Both runs succeeded with a typed `result.json`, one fixture submission, a
record naming the image and Helium version, and no capability URL in the
log, record or files. Afterwards the session stayed `ready`, the controller
was released, and no client process was left. `bctl cancel` and a `kill -9`
of the daemon after the side effect each ended the attempt once, with no
retry and the side effect seen once. With an unreachable URL, each recipe
failed without starting a browser and redacted the URL. agent-browser's
daemon detaches from the job's process group, so its recipe closes it on exit
and on `SIGTERM`. The suite has 91 checks. Linux x86_64 is unverified.

**E4 — Files, profiles, and capability receipts.** Prereq E1; profile work
may depend on M7N. Host workspace and browser filesystem are distinct.
Screenshots returned over CDP are already host-side bytes; browser downloads
are guest-side files and do not automatically appear under
`BCTL_OUTPUT_DIR`. browser-use's `remote_upload.py` already sends bytes
through the page as a fallback: verify before reinventing. agent-browser
currently passes local file paths to `DOM.setFileInputFiles`, so an upload
needs guest-readable staging or an explicit transfer. Plan narrow per-run
file APIs with byte bounds, path and symlink validation, content checksums,
collision handling, partial-file cleanup, and completion before teardown.
Never mount the user's home or add a generic guest shell. Record Nanos's
headless, no-input, no-VM-pause and currently nonpersistent-profile behavior
as independent capability fields; CDP screenshots remain viable without
VNC. Do not claim persisted identity because the config has a profile name.
**Acceptance:** upload a nonempty binary and verify SHA-256 on the fixture
server; collect a downloaded binary with verified digest and no path escape;
assert correct unsupported errors for missing runtime transport/profile
capabilities; no accidental profile sharing.

**E5 — Optional agent-browser provider.** Prereq E1–E2. Keep it a separate,
tiny Rust executable/subcommand implementing upstream
`agent-browser.plugin.v1`: `plugin.manifest`, `browser.launch`, and
`browser.close` with a single JSON stdin/stdout exchange. Return a reachable
browser-level `cdpUrl`, `directPage:false`, nonsecret metadata and an opaque
cleanup handle. Only the matching owned generation may be destroyed.
A borrowed lease is released, never treated as owned. Reject unsupported
launch options (including non-Helium engines and unsupported headed requests)
rather than promising their effects. Upstream plugin providers cannot supply
headers in the plugin result, and their launch/close calls have timeouts;
record these limits. Use an explicit trusted plugin config, not automatic
installation or a dependency on npm at bctl runtime.
**Acceptance:** pinned plugin manifest, launch and cleanup protocol fixtures,
real `--provider bctl` navigation, failed-launch cleanup, repeated close and
owner-death reclamation. Generic CDP attachment remains usable without plugin.

**E6 — Cross-runtime regression and measurement.** Prereq E1–E5 according
to each feature. Add interop to the existing acceptance/bench discipline:
per-client connection startup p50/p95, screenshot/AX/navigation latency,
CDP call and byte counts where available, external process resource use,
cleanup time, and completed fixture results; no unverified comparisons.
Exercise headed SmolVM/macOS and headless Nanos/Linux separately, never
claim equivalent display or profile behavior. Upgrade gates are pinned to
client releases, not their moving main branches. Store version receipts,
failures and any reason a test was skipped.
**Acceptance:** an explicit supported-feature matrix for each verified
(client, version, host, runtime, Helium image) tuple with reproducible
commands, failing cases, and a maintained compatibility suite.

**Stop conditions:** uncertain authority, unexpected automatic browser
provisioning or billing, sandbox being disabled, unbounded file access,
capability leakage, arbitrary profile deletion, or a misleading "passed"
result must stop that slice. Publish supported claims only after actual
released-client integration tests pass.

## Acceptance

- A1 Observable browser: on an Apple Silicon Mac, `bctl up s && bctl serve
  --open` shows Helium live in the dashboard, with typed and clicked input
  landing in the page. Failure: any frame comes from a non-product image
  without that image named in the receipt.
- A2 Sandbox: every product image's renderers run with `NoNewPrivs=1` and
  `Seccomp=2`. Failure: any launch path adds `--no-sandbox`.
- A3 Ownership: daemon restart never duplicates or orphans a session, and an
  unknown-outcome attempt is never silently retried.
- A4 Profiles: one writable session per profile; login state survives
  `down`/`up`. Failure: two browser processes on one profile.
- A5 Jobs: `bctl grab` and `bctl run` produce categorized outcomes and
  verified Grab mirrors; interrupted crawls resume from Grab's own state.
- A6 Takeover: `NeedsInteraction` to manual login to `Running` in the same
  browser, from CLI or dashboard, with a server-enforced exclusive input
  lease. Exclusive means against RFB viewers, bctl's native automation, and
  (after H2) new CDP attachments; until H2 the receipt says raw loopback CDP
  was not gated.
- A7 Stream security: no unauthenticated or cross-origin path reaches VNC;
  input on a view-only runtime is refused, not dropped silently.
- A8 Tinyland: the same suite passes on `helium-tinyland` and on the Sway
  diagnostic image.
- A9 Pure-Rust userland: R0's census of the default image reports no gap, and
  every exception is named in `docs/userland.md` with its reason. Failure: an
  unattributed file, or a C library outside the named exceptions.
- A10 Port-back: `guest/tinyland` and `guest/seatd` contain no bctl-specific
  code, and their test suites pass without bctl's host crates.
- A11 Native automation: a coding agent drives a session with only `bctl`
  or its Rust API (no agent-browser, Playwright, or Shadowdriver), while the
  same headed browser stays observable and controllable in the dashboard.
  Failure: an action lands on a different tab, element, or document than
  the one named, or a stale ref is re-resolved instead of failing.

- A12 External client CDP: browser-use 1.0.0 and agent-browser v0.42.0
  individually attach through bctl's authenticated, revocable gateway to
  the **same** Helium session, perform the E1 fixture suite and detach
  without shutting down the browser. No cloud fallback, raw guest CDP,
  or silently substituted browser. Record verified host/runtime tuple.
- A13 External ownership: two CDP connections belonging to one logical
  controller are allowed, conflicting controllers refused; dashboard
  takeover cannot grant misleading exclusivity while an external
  controller continues sending commands. Revocation/cleanup cannot kill
  a newer generation; an unknown-outcome action is never retried by bctl.
- A14 External artifacts: uploads and downloads are verified byte-for-byte,
  bounded and stored under the correct run, not guessed from the host
  workspace. Unsupported guest file paths/transport fail visibly.
- A15 Provider and claims: the optional agent-browser plugin implements
  its exact pinned protocol and the managed/borrowed cleanup distinction.
  Client support is labelled "unverified" unless real pinned-client
  acceptance passed on that runtime; Nanos does not claim headed input,
  VM pause/resume or persistent profiles without new evidence.

## Needs environment

Owner-provided; never blocks other items.

- The Linux arm64 Helium artifact (P2, T0, and everything after M3 on the
  product image).
- A Linux KVM host for X3's remote context and amd64 parity.
- The Apple Silicon Mac is available (Apple M5 Max, macOS 27.0.1); M8C's
  macOS and SmolVM leg (E1 to E6 on headed Helium) runs on it when M8C
  reaches it.

## Out of scope

WebRTC or a new video pipeline until H1 shows the VNC path inadequate; if
it does, `algesten/str0m` (sans-I/O WebRTC, host side only) is the first
candidate, but its rust-crypto provider pulls `aws-lc-rs` through `dimpl`'s
`rcgen` feature and it has no video encoder, so a spike must prove a pure-Rust
closure first, with graviola as the crypto provider (SmolVM's `/video` H.264 endpoint is evaluated
in H1 only if RFB fails the latency gate), recording, a workflow language,
stealth or fingerprint patching in bctl (that belongs in chromium-build and
Shadowdriver), and a general-purpose guest agent or init system. From the
agent-browser comparison: feature parity, annotated screenshots, HAR
recording, network interception, agent chat, cloud browser providers, mobile
emulation, and a JavaScript execution server (`page eval` is enough).
