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

Accepted: none. `guest/tinyland` and `guest/seatd` are vendored and pass
`cargo check --locked --target aarch64-unknown-linux-musl --all-targets`.
Next: P0, P1, and T0 (parallel).
Resume: `implement P0 from docs/CAMPAIGN-ROADMAP.md`.

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
- Prescribed: bctl builds its own SmolVM host runtime. It pins the same
  upstreams as niceforge's `third_party/UPSTREAM.lock` (smolvm `v1.31.4`
  `e5e57cbe`, libkrun `62059976`, libkrunfw `8ba5fe22`) and builds libkrun with
  `blk,net,gpu,input`. Niceforge's headless `blk,net` build stays as it is.
  Port `scripts/build_smolvm_host.py` rather than parameterizing niceforge's
  copy: the two products upgrade on different schedules. Reversal: a third
  consumer appears, at which point extract a shared runtime-build repo.
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
  Navigation and extraction are ordinary Shadowdriver code.
- Prescribed: Rust for CLI, daemon, and gateway; SQLite for durable state;
  Solid 2 with `@laputa-systems/ui-dashboard` 0.5.0 (light only) for the
  dashboard, built to static assets embedded in the `bctl` binary. No Node at
  runtime. The live view uses noVNC's core RFB client behind bctl's own page.
- Prescribed: SmolVM's VNC has no authentication (`vnc.rs:9`, `:480-482`). Its
  listener binds `127.0.0.1` on an ephemeral port and only bctl's gateway
  connects to it. The browser-facing stream requires a dashboard session and
  an Origin match. CDP is forwarded to loopback only. In v1 any local process
  of the same user can reach it; H2 puts it behind the gateway.
- Delegated: profile storage. Prefer a per-profile SmolVM disk (`--storage`)
  over a host directory volume. Pick the host volume only if a browser profile
  on it survives P3's acceptance (login cookie persists across `down`/`up`, no
  SQLite lock errors in `chrome_debug.log` over 20 restarts).
- Delegated: guest base. Prefer an Alpine aarch64 rootfs carrying SmolVM's
  static musl agent. Fall back to niceforge's Debian 13 agent rootfs if SmolVM
  rejects a non-Debian base, and record which in `docs/image-contract.md`.
- Delegated: Rust crates (CLI parsing, HTTP, SQLite, WebSocket). Each needs a
  one-line reason in its commit body and must fit H1's binary-size gate.

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

- **P0 Display-capable runtime.** Owns `runtime/UPSTREAM.lock`,
  `runtime/build.py`, `runtime/README.md`. No prereq. Port niceforge's
  `build_smolvm_host.py` (pin checks, arm64 and ABI checks, hypervisor
  entitlement signing) with libkrun features `blk,net,gpu,input`. Start from
  the unpatched upstream pins. Niceforge carries 20 SmolVM patches
  (`third_party/UPSTREAM.lock` `patches=`), mostly for its CI execution
  authority and checkpoint ledger. Adopt one only when a bctl acceptance
  check fails without it, and name that check in the commit body. Output goes
  to `.work/runtime/<lock-digest>/`. Acceptance: the build succeeds from
  clean; `nm` on the built libkrun shows `krun_add_display` and
  `krun_add_input_device`; a second build with an unchanged lock does no
  compile work; `smolvm --version` matches the lock. Parallel-safe with P1.
- **P1 Reference appliance image.** Owns `images/ref-alpine-chromium/`,
  `images/build.sh`, `docs/image-contract.md`. No prereq. An Alpine aarch64
  rootfs with SmolVM's agent, seatd, Sway (`WLR_RENDERER=pixman`, XWayland off,
  one output), and the Alpine `chromium` APK. Also a guest entrypoint
  `bctl-guest` that starts seatd, then Sway, then Chromium with
  `--ozone-platform=wayland --remote-debugging-port=9222
  --remote-debugging-address=127.0.0.1 --user-data-dir=/profile`. Write down
  the image contract: entrypoint, readiness signals, CDP port, profile mount,
  output dir, and required guest devices. Settle the guest-base delegated
  decision here. Acceptance: `images/build.sh ref-alpine-chromium` produces an
  image SmolVM boots; inside it, `bctl-guest --check` reports seatd, compositor,
  and Chromium ready, and renderers show `NoNewPrivs=1` and `Seccomp=2`.
  Parallel-safe with P0.
- **P2 Helium arm64 product image.** Owns `images/helium/`. Prereq: P1 and the
  arm64 artifact. Install the archive with chromium-build's
  `install-runtime.py --verify-extract`. Refuse an archive whose sidecars do not
  bind its digest, product, and `aarch64`. Resolve the musl-loader question.
  Acceptance: same checks as P1, and the image's receipt records the Helium
  version string and archive SHA-256.

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
  It never freezes the VM. Acceptance: a fixture crawl requires a login, enters
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
  Acceptance: a Playwright test where a second viewer's input does not reach
  the guest while the first holds the lease; the takeover and resume flow of
  J3 driven entirely from the dashboard.

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

- **H1 Gates and baselines.** Owns `docs/performance.md`,
  `tests/bench/`. Prereq: M3. Measure and record: cold `up` to ready, first
  visible frame, input-to-pixel latency through the gateway, RSS per session,
  idle host CPU per session, viewer overhead, `down` latency, and `bctl`
  binary size. Set the gates from these with their observed spread. Set the
  log, artifact, and disk bounds. Time gates follow the `perf-gate-ready`
  rule.
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
- **X2 Alternative backends.** A unikernel-style or other VMM backend
  (Unikraft-style library OS, Firecracker/Cloud Hypervisor on Linux) must beat
  SmolVM on H1's measures while passing the full acceptance suite unchanged.
- **X3 Remote contexts.** A remote `bctl` daemon on a Linux KVM host using the
  amd64 Helium artifact, with the same API and ownership model. Jobs reach its
  sessions through its gateway's CDP endpoint, or run on that host when H1's
  round-trip measurements say the distance costs too much.
- **X4 Site access evidence.** Run outcome categories feed a per-site record of
  which configuration (profile warmth, egress, flags) passed or was
  challenged, with sample counts and dates. This measures task completion,
  not page loads. It covers only sites the operator is entitled to automate.

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
  lease.
- A7 Stream security: no unauthenticated or cross-origin path reaches VNC;
  input on a view-only runtime is refused, not dropped silently.
- A8 Tinyland: the same suite passes on `helium-tinyland` and on the Sway
  diagnostic image.
- A9 Pure-Rust userland: R0's census of the default image reports no gap, and
  every exception is named in `docs/userland.md` with its reason. Failure: an
  unattributed file, or a C library outside the named exceptions.
- A10 Port-back: `guest/tinyland` and `guest/seatd` contain no bctl-specific
  code, and their test suites pass without bctl's host crates.

## Needs environment

Owner-provided; never blocks other items.

- The Linux arm64 Helium artifact (P2, T0, and everything after M3 on the
  product image).
- A Linux KVM host for X3's remote context and amd64 parity.

## Out of scope

WebRTC or a new video pipeline (SmolVM's `/video` H.264 endpoint is evaluated
in H1 only if RFB fails the latency gate), recording, a workflow language,
stealth or fingerprint patching in bctl (that belongs in chromium-build and
Shadowdriver), and a general-purpose guest agent or init system.
