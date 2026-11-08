**Yes—`bctl` is a good project boundary. I would build it as a browser-appliance controller, with SmolVM underneath, Shadowdriver as the automation library, and Grab as a workload—not start by turning Chromium into a unikernel.**

The first product should make this sequence effortless:

```sh
bctl up research
bctl serve --open
bctl grab https://example.com --session research
bctl status
bctl down research
```

A managed browser appears in the dashboard; you can watch it, take control for login, return control to automation, inspect results, and stop it without losing the profile.

**The most useful discovery from inspecting your code: you already have considerably more of the VNC implementation than you thought.** Niceforge’s vendored SmolVM includes a Rust host-side VNC server, WebSocket transport, a browser viewer, and keyboard/pointer injection. Its current build simply does not enable the necessary graphics and input features.   

## 1. What the existing pieces actually give you

I inspected the current repository contents rather than relying on the previous descriptions.

| Component | What is already present | What `bctl` still needs |
|---|---|---|
| **`chromium-build`** | Linux amd64 musl builds; native macOS ARM64 builds; an accepted Helium Wayland package with runtime dependency auditing | Linux ARM64 support for a native Apple Silicon VM; a browser-appliance image |
| **Shadowdriver** | Typed CDP automation; explicit external-browser attachment through `control_url`; separation of native identity and capture instrumentation | A clean managed-session attachment contract and Linux packaging qualification |
| **Grab** | Resumable crawling, capture evidence, offline output, interactive authentication | Non-terminal interaction events, externally supplied browser sessions, structured job progress |
| **Niceforge’s SmolVM** | Pinned native sources, VM lifecycle, checkpoints, file-transfer and execution contracts; vendored display/VNC implementation | A separate graphics-capable runtime build and browser-specific integration |
| **Tinyland** | Small Rust Wayland kiosk, real pixels, keyboard input, device mediation | Chromium qualification, pointer interaction, popups/dialogs, and the relevant display lifecycle |

These distinctions matter. `chromium-build` explicitly excludes Linux ARM64 today, its Linux documentation qualifies the graphical browser using an isolated Sway fixture, and Tinyland’s current integration campaign explicitly excludes pointer support and multiwindow operation. Shadowdriver’s Rust library now documents Linux support, but its Python SDK documentation still lists macOS wheels. Those are integration gates, not details to assume away.     

### First, a correction to the Kernel analogy

Kernel’s published progression was Docker → Unikraft-based browser instances → a QEMU-based Hypeman path for GPU browsers. Their fast-start trick is substantially **starting Chromium ahead of time and restoring its VM state**, not making the entire Chromium initialization disappear. Their write-up also acknowledges that their multiprocess environment stretches the conventional definition of a unikernel. [KERNEL](https://www.kernel.sh/blog/scale)

Also, Unikraft’s library OS and the VMM running it are separate architectural layers. My previous answer should not have blurred that distinction. [Unikraft](https://unikraft.org/docs/concepts/virtualization?utm_source=chatgpt.com)

**You can obtain the management UX, isolation, live viewing, persistent profiles, and eventually snapshot-based acceleration without making a unikernel the prerequisite.**

---

## 2. The architecture I recommend

```text
                     bctl CLI
                         │
                 local authenticated API
                         │
              ┌──────────▼───────────┐
              │ bctl daemon          │
              │                      │
              │ sessions + jobs      │
              │ profiles + leases    │
              │ SQLite state         │
              │ artifacts + logs     │
              │ dashboard + gateway  │
              └───────┬───────┬─────┘
                      │       │
              SmolVM adapter  └── Solid dashboard
                      │                  │
              ┌───────▼──────────────────▼─────┐
              │ SmolVM host process             │
              │ VM lifecycle                    │
              │ framebuffer → VNC/WebSocket     │
              │ keyboard/pointer → virtio-input │
              └──────────────┬──────────────────┘
                             │
              ┌──────────────▼──────────────────┐
              │ Linux musl browser appliance     │
              │                                  │
              │ Wayland compositor               │
              │ Chromium, with its sandbox       │
              │ narrow bctl application worker   │
              │ Shadowdriver / Grab job process  │
              │ writable profile + job output    │
              └──────────────────────────────────┘
```

### Keep the responsibilities unusually clear

**`bctl` owns browser operations:** provision, start, stop, attach, observe, run jobs, handle human intervention, retain artifacts, and clean up.

**Shadowdriver owns browser automation:** CDP, tabs, navigation, capture, readiness, and browser interaction.

**Grab owns crawling and interpretation:** URL scope, crawl queues, resource capture, rewriting, resume state, and mirror verification.

**SmolVM owns machines:** virtualization, machine execution, storage mechanics, and supported checkpoint operations.

That prevents `bctl` from becoming a second browser driver, a second crawler, and a second Niceforge.

The existing attachment branch in `CrawlerSession::ensure_chromium` already connects to a supplied `control_url` without launching a child process; its shutdown logic distinguishes that externally controlled session. Start from that seam rather than replacing Shadowdriver. 

### Implementation choices

I would use **Rust for the CLI, daemon, and narrow guest worker; SQLite for durable state; Solid for the dashboard**. Embed the built dashboard assets in the host executable. No Node server at runtime.

Initially, invoke a **pinned SmolVM executable through a typed process adapter**, rather than embedding its Rust implementation into `bctl`. This keeps native library loading, entitlements, crashes, and dependency choices behind a process boundary.

Reuse Niceforge’s source-pinning, artifact verification, and native acceptance work, but **not its controller, workflow interpreter, or entire patch stack indiscriminately**. Its native integration includes CI-specific execution authority and checkpoint contracts that deserve an explicit reuse decision. 

The guest worker should remain application-specific. Reuse SmolVM’s execution and transfer mechanisms; do not invent another general-purpose guest agent or another init system.

---

## 3. Resolve the Mac architecture issue before designing around it

Your current artifacts do not yet line up:

| Host | Native SmolVM guest | Existing custom browser |
|---|---|---|
| Apple Silicon macOS | Linux ARM64 | **Missing Linux ARM64 build** |
| Linux x86-64 | Linux x86-64 | Existing musl browser |
| macOS ARM64, outside a VM | macOS ARM64 | Existing native browser, but not the proposed Linux appliance |

SmolVM documents matching host/guest CPU architectures. Its Rosetta integration can translate supported x86-64 Linux applications inside an ARM64 guest, but it does not turn that guest into an x86-64 VM or make x86-64 machine snapshots portable to ARM64. [smol machines](https://www.smolmachines.com/docs/introduction/concepts/supported-platforms)

**My recommendation: make native Linux ARM64 the intended Mac path.**

Use two parallel qualification tracks:

**Browser track:** package and validate your existing amd64 musl browser on Linux immediately. Add an explicit Linux ARM64 target to `chromium-build`, preserving its runtime, sandbox, and acceptance checks.

**Display/controller track:** use a clearly labelled reference ARM64 Alpine browser image to prove the Mac VM, display, input, and dashboard plumbing while the custom ARM64 build is being established. That reference image is test scaffolding, not a silently substituted product browser.

A Rosetta experiment is reasonable as an optional shortcut. Its acceptance test must include the real musl binary, renderer sandbox, Wayland rendering, CDP, and repeated shutdown—not just `chromium --version`. Do not let successful translation become an assumed foundation for snapshots or production support.

There is another distinction to preserve: **Niceforge’s current SmolVM agent root filesystem is Debian-based**, even though several agent binaries are statically linked with musl. Running an Alpine workload does not by itself make the entire guest musl-only. For `bctl`, qualify the agent/base filesystem as well as the browser workload image. 

---

## 4. VNC: reuse the host-side implementation first

This substantially simplifies the plan.

The SmolVM display path already provides:

```text
guest compositor
      ↓
virtio-gpu scanout
      ↓
host framebuffer
      ↓
RFB / WebSocket
      ↓
browser viewer
```

Its documented controls are `SMOLVM_DISPLAY` and `SMOLVM_VNC`; the latter serves native VNC, a browser page, and an RFB-over-WebSocket endpoint. The guest does not need a VNC server or compositor-specific screencopy support. Keyboard and pointer support require the corresponding libkrun input capability. 

**This is a particularly good match for Tinyland:** it lets the compositor concentrate on displaying Chromium and processing input, instead of simultaneously implementing remote-desktop capture protocols.

### The actual work required

Niceforge currently builds libkrun with `blk,net`. In the vendored manifest, `gpu` and `input` are distinct optional features. Create a separate `bctl` runtime variant that enables and verifies the required capabilities; leave Niceforge’s headless execution build unchanged.  

Add a runtime capability receipt covering display creation, input injection, architecture, and supported lifecycle operations. A missing input capability must produce **“view-only runtime”**, not a clickable-looking dashboard that silently discards input.

The current upstream display path requires its GPU/display configuration and a KMS-driving guest compositor. Do not assume a headless compositor backend will magically populate the host scanout. 

### Viewer integration

Use the existing viewer for the first end-to-end proof. For the finished dashboard, I would integrate **noVNC’s client library** behind `bctl`’s own session page rather than maintaining a custom RFB client. noVNC explicitly supports embedding its core JavaScript client. [noVNC](https://novnc.com/info.html)

Keep the underlying SmolVM display endpoint private. Its VNC implementation has **no authentication**, including in the source already vendored by Niceforge. `bctl` must authenticate the dashboard and authorize the proxied stream; loopback binding alone is not the complete browser-facing security design. 

Do not start with WebRTC, TURN, recording, and a new encoding pipeline. Deliver working VNC first, then measure whether compressed streaming is necessary.

### Important UX distinction: headed browser, unattended machine

The default observable appliance should run **headed Chromium on a virtual display**, not Chromium’s `--headless` mode.

A screenshot stream is not equivalent to an interactive desktop containing browser chrome, permission prompts, native dialogs, and menus. Headless mode can be a later explicit image capability.

Also, a single browser display shows the **currently visible tab/window**, not every background tab simultaneously. The dashboard should expose a tab list and “bring to front”; optional per-tab thumbnails are a separate CDP feature.

---

## 5. Tinyland should be a bounded integration project

I would not make the first VM/dashboard proof depend on completing Tinyland.

You already have a useful reference: `chromium-build`’s Helium acceptance fixture runs a private Sway session with software rendering, without XWayland or a physical GPU. Reuse that as the known-good graphical baseline, adapted to the VM’s display path. 

Then replace that baseline with Tinyland against an explicit **browser-appliance compositor contract**:

| Area | Required acceptance |
|---|---|
| Rendering | Browser content and chrome render correctly; resize and buffer release remain correct |
| Pointer | Absolute motion, buttons, scroll, cursor visibility, drag |
| Keyboard | Modifiers, repeat, focus, common layouts, no stuck keys after disconnect |
| Browser surfaces | Menus, select dropdowns, tooltips, authentication dialogs, file chooser |
| Window policy | One primary browser window plus required transient surfaces; deterministic handling of extra windows |
| Lifecycle | Chromium crash, compositor crash, viewer reconnect, VM stop/start |
| Security | Unprivileged session, mediated device access, bounded shared-memory allocations |

**One primary window is a useful simplification; refusing the popup/transient surfaces required by that window is not.**

Tinyland’s current qualification is valuable, but it proves a different workload: a kiosk with Ghostty, keyboard input, and mediated devices. Its documentation does not establish Chromium or pointer qualification. 

Keep Sway as a diagnostic/reference image, not a permanent architectural commitment. Keep Tinyland changes in Laputa’s authoritative source rather than forking a second compositor inside `bctl`.

---

## 6. Design the CLI around sessions and runs—not machines

The user-facing concepts should be:

**Session:** a running or stopped browser environment.

**Profile:** persistent browser identity and storage.

**Job:** a reusable workload definition.

**Run:** one execution of a job, with attempts, logs, and artifacts.

**Image:** the immutable appliance used to create a session.

The VM identifier belongs in `bctl inspect`, not in every everyday command.

### Proposed command surface

These are the interfaces I would implement, not commands that exist today:

```sh
# Start or reuse a named browser session.
bctl up research --profile personal
bctl status
bctl inspect research

# Start the dashboard and open it.
bctl serve --open
bctl view research

# Run a one-off crawl or a named job.
bctl grab https://example.com --session research
bctl run mirror-docs
bctl run extract-products --input urls.txt

# Observe and control a run.
bctl runs
bctl logs <run-id> --follow
bctl pause <run-id>
bctl resume <run-id>
bctl cancel <run-id>

# Manage persistent identity.
bctl profile login personal
bctl profile list

# Stop execution without deleting data.
bctl down research

# Explicit destruction.
bctl rm research
```

The semantics matter more than the number of commands:

`up` is idempotent for the same resolved configuration. A conflicting configuration requires an explicit replacement operation.

`down` stops the session and preserves its profile and results. It is not a synonym for deleting everything.

`serve` enables the dashboard on the existing controller. It must not create a second orchestration process with competing ownership.

Lifecycle commands can lazily start one per-user daemon. Closing the dashboard, disconnecting the CLI, or closing a terminal must not accidentally terminate detached jobs.

Every read command gets `--json`. Errors have stable codes and useful remediation. `status` should show **why something is not ready**, not merely whether a process exists.

---

## 7. Job definitions: typed configuration, ordinary code

I would support exactly two workload kinds initially:

**`grab`**, a built-in adapter for the existing crawler.

**`exec`**, an executable argument vector run inside the appliance, with an injected managed-browser attachment.

Do not create a browser workflow language. Navigation, extraction, branching, and application logic belong in normal Shadowdriver code.

A proposed `bctl.toml` could look like:

```toml
version = 1

[browsers.default]
image = "chromium-build:wayland"
viewport = [1280, 800]
cpus = 2
memory_mib = 2048

[jobs.mirror-docs]
kind = "grab"
browser = "default"
urls = ["https://example.com/docs/"]
max_pages = 500
page_concurrency = 2
respect_robots = true
timeout = "30m"
on_failure = "retain"

[jobs.extract-products]
kind = "exec"
browser = "default"
image = "chromium-build:wayland-python"
argv = ["python3", "/job/extract_products.py"]
source = "./jobs"
timeout = "10m"
```

Treat the resource values as starting defaults to measure, not validated sizing.

Resolve image names and dependencies into a lockfile. Each run records the resolved image digest, browser build, job source digest, input digest, profile identity, and effective configuration. Reject unknown configuration fields.

The `exec` adapter should stage a bounded, declared source bundle read-only and provide a separate output directory. No implicit shell interpolation, mounting the user’s home, or installing arbitrary dependencies during every launch.

### Minimal Shadowdriver/Grab changes

For Shadowdriver, formalize **managed launch versus attached session**. Attached code must not spawn a replacement browser or terminate `bctl`’s browser when its connection fails. The existing `control_url` path is the starting point, but local profile/provenance assumptions still need review. 

For Grab, add a structured integration entry point accepting the managed browser, output root, cancellation, and progress sink.

Replace its current “open a browser, then press Enter in the terminal” integration with a `needs_interaction` event that both CLI and dashboard can handle. Preserve the existing same-browser authentication behavior. Grab already documents interactive login followed by crawling in the same process/profile. 

**Do not import the crawler’s queue into `bctl`’s database.** Grab retains its resumable crawl state; `bctl` tracks the run containing that crawl.

---

## 8. Make human intervention part of the execution model

This is where the UX can become meaningfully better than a wrapper around scripts.

A run encountering authentication should enter:

```text
Running → NeedsInteraction → Running
```

The dashboard should explain the request:

> Login required. Automation has stopped issuing actions. Take control, complete login, then resume.

Taking control must acquire an **exclusive input lease**. Observers remain read-only. The server—not merely a disabled button in the UI—must prevent unauthorized input.

Pause means stopping automation at a documented safe boundary while leaving the browser alive. It does **not** mean freezing the VM: the user still needs working network requests, JavaScript, and input.

Do not promise transparent pause/resume for arbitrary scripts that are midway through a side-effectful CDP operation. Built-in Grab and cooperative Shadowdriver jobs can provide meaningful pause points; unmanaged external CDP clients need stricter attachment rules.

A profile should have **one writable session at a time**. No concurrent Chromium processes sharing the same profile directory, and no automatic pooling across unrelated identities.

For v1, use one VM per active session, one active job lease per session, and multiple tabs within that job. This is substantially easier to reason about than arbitrary jobs sharing one browser.

---

## 9. The dashboard should answer operational questions immediately

I would build three principal views.

### Sessions

A compact list or grid showing session name, profile, lifecycle state, current job, current URL, resource usage, and last meaningful activity.

Use low-frequency thumbnails or cached previews. Open a full live stream only for the selected session; do not keep sixteen full-rate viewers running merely to decorate the overview.

### Session detail

The live browser should dominate. Alongside it: tabs, automation/manual-control state, current job step, logs, and start/stop controls.

The useful controls are **Take control**, **Resume automation**, **Focus tab**, **Reconnect viewer**, and **Stop session**. A stream failure should not be presented as a browser crash.

### Run detail

Inputs, resolved configuration, progress, attempt history, logs, captured artifacts, and the exact failure category. “Browser crashed,” “site rejected access,” “login required,” and “output verification failed” should not all become “job failed.”

Reuse your existing visual language. There is no need to extract another design framework before this dashboard exists.

Serve captured HTML and other active artifacts from a separate, restricted origin or through a safe download path—not directly under the authenticated management origin.

---

## 10. Durability and security belong in the initial design

Use SQLite as the authoritative record of desired state, observed state, jobs, attempts, profile leases, and artifact metadata.

Keep lifecycle state explicit, for example:

```text
Session:
Stopped | Starting | Ready | Stopping | Failed

Run:
Queued | Starting | Running | NeedsInteraction |
Succeeded | Failed | Cancelled | Interrupted
```

Manual-control ownership is a separate lease, not another dozen session-state combinations.

Before creating external resources, record the operation intent. After daemon restart, reconcile records with actual owned VM processes. Bind ownership to runtime identities and generation tokens, not just a PID that may have been reused.

A controller crash must not result in automatically rerunning an operation whose external effects are unknown. Mark such attempts interrupted and let the workload’s declared retry policy decide. There is no honest blanket “exactly once” guarantee for website interactions.

The initial security policy should include:

- Chromium’s inner sandbox remains enabled; failure to establish it fails qualification.
- No default home-directory, SSH-agent, or credential-store mounts.
- Authenticated dashboard/CDP/display access with origin checks and narrowly scoped capabilities.
- Bounded logs, artifacts, disk growth, concurrent sessions, and runtime.
- Public-web egress by default, with explicit permission for local/private services.

SmolVM itself warns that exposed mounts, networking, and forwarded agents are additional authority granted to the guest; a VM boundary does not neutralize those capabilities. [GitHub](https://github.com/smol-machines/smolvm/blob/main/README.md?utm_source=chatgpt.com)

---

## 11. The implementation roadmap

### Milestone 0 — Prove the platform contracts

Resolve architecture and image packaging. Build the display-capable SmolVM variant; verify display and input symbols. Establish native ARM64 browser work and the existing amd64 baseline. Qualify the Python extension on Linux.

**Exit condition:** exact runtime and browser identities are recorded, unsupported combinations fail clearly, and no successful result depends on silently substituting another browser or disabling the sandbox.

### Milestone 1 — One observable browser appliance

Boot one VM. Start the compositor and browser. Attach through Shadowdriver. Display it through the existing SmolVM viewer. Type into a fixture page, click, scroll, open a popup, and stop cleanly.

Use the reference compositor here to isolate problems. No dashboard framework or scheduler is necessary yet.

**Exit condition:** browser readiness, changed pixels, real keyboard/pointer input, CDP navigation, and cleanup are all demonstrated end to end.

### Milestone 2 — `bctl up/down/status/serve`

Implement the single-owner daemon, SQLite state, runtime adapter, immutable image selection, persistent profiles, authenticated display gateway, and a minimal session dashboard.

Readiness must progress through machine → guest worker → compositor → browser/CDP → display. An open TCP port is not sufficient.

**Exit condition:** two isolated sessions can run concurrently; CLI/dashboard reconnect works; restarting the daemon does not duplicate sessions or lose ownership.

### Milestone 3 — Useful jobs and human takeover

Integrate Grab and `exec` jobs, source/input sealing, progress events, cancellation, artifacts, and the interaction lease.

**Exit condition:** a real crawl can require login, pause for manual control, continue in the same browser, produce a verified mirror, and resume after an interrupted crawl.

**This is the first useful `bctl` release.**

### Milestone 4 — Make Tinyland the default appliance compositor

Implement only the browser-specific contract above. Run the same acceptance suite against reference and Tinyland images.

**Exit condition:** Tinyland passes the browser suite, including dialogs, pointer input, reconnect, and crash behavior. Remove the reference compositor from the default distribution, retaining it as a diagnostic fixture.

### Milestone 5 — Operational hardening and measured acceleration

Add failure injection, resource admission, retained-failure expiry, storage GC, profile recovery, and representative browser workloads.

Measure cold readiness, first visible frame, input latency, memory per session, idle CPU, viewer overhead, artifact bandwidth, and cleanup latency. Establish baselines before starting an optimization campaign.

Then evaluate snapshots, compressed video, and a small warm pool individually.

### Milestone 6 — Remote execution and alternative VM backends

Add a remote `bctl` context using the same API and ownership model. The local CLI and dashboard should not need to know whether the session runs on the Mac or a Linux host.

Only then evaluate another VMM or a unikernel-oriented runtime against the same appliance acceptance suite.

**A new backend earns its place through measured benefit, not novelty.**

---

## 12. Snapshots and unikernels: deliberately later

I would distinguish three features rather than calling all of them “resume”:

| Feature | Meaning |
|---|---|
| **Profile persistence** | Restart Chromium with its saved browser data |
| **Workload resume** | Restart a job from application-level durable progress |
| **VM checkpoint restore** | Restore guest RAM, processes, and device state |

Build the first two before relying on the third.

SmolVM’s browser guide specifically warns that its documented branching setup requires `--no-zygote`, and that live network connections and host-side GPU renderer state do not survive the branch. Its example also disables Chromium’s sandbox. That example is not sufficient evidence for your intended sandboxed graphical appliance. [smol machines](https://www.smolmachines.com/docs/guides/headless-browser-computer-use)

The first acceleration experiment should therefore be **a clean, credential-free appliance template**, potentially captured before Chromium starts. A running-browser checkpoint must separately prove sandbox behavior, compositor recovery, network reconnection, timers, fresh instance identity, and profile isolation.

Do not promise that existing CDP connections survive restore. Make reconnection explicit until you have demonstrated stronger semantics.

A true unikernel effort should answer a measured question: does it materially improve ready-to-use latency, memory, density, or maintenance burden over your minimal Linux appliance while preserving Chromium’s behavior? It should not block the management product.

## Recommended repository shape

Keep the new repository small enough to understand:

```text
bctl/
  crates/
    bctl/                 # CLI
    core/                 # operations, state, leases, scheduler
    server/               # API, dashboard assets, stream gateway
    runtime-smolvm/       # typed process adapter
    guest/                # browser-specific application worker
  web/                    # Solid dashboard
  images/                 # appliance recipes and manifests
  jobs/                   # built-in adapters and example jobs
  tests/
    fixtures/
    acceptance/
    recovery/
  docs/
    architecture.md
    lifecycle.md
    image-contract.md
    roadmap.md
```

No Chromium source tree, copied crawler implementation, second compositor fork, or general CI engine.

**The immediate target should be one clean command that starts your browser in a VM, shows it in a browser tab, accepts real input, and lets Shadowdriver operate that exact session.** That proves the foundation of the product you actually want. SmolVM already supplies much of the difficult display plumbing; the highest-value work is now ownership, integration, observability, and the human-to-automation handoff.