# External browser controllers: source-pinned integration research

Status: **E1 passed on macOS arm64** (SmolVM and Nanos) with the released
clients; Linux x86_64 is unverified. See [Runtime results](#runtime-results-e1).
The rest of this document is source inspection. Campaign owner: M8C in
[CAMPAIGN-ROADMAP.md](../CAMPAIGN-ROADMAP.md). Inspected bctl commit:
`bc6462a54a0fb77aa2492816381ca653708087e8`.
The source dates are November 9, 2026 (America/Los_Angeles). Runtime results
are deliberately distinguished from inspection.

## Goal and non-goals

Make bctl's Helium appliance (SmolVM headed on macOS arm64, hydrogen/Nanos
headless on Linux x86_64) a **locally managed browser backend** for the
open-source browser-use 1.0 agent and agent-browser v0.42 CLI.
Both consumers run on the host as CDP clients. bctl owns VM lifecycle,
profile, egress, CDP capabilities, dashboard, leases, run metadata and
artifacts. No agent models, JavaScript/Python automation interpreter,
browser launcher, upstream provider SDK, or client dependency tree belongs
in the guest. Preserve bctl-native automation, Grab and Shadowdriver.

This is not an integration with Browser Use Cloud, the built-in
agent-browser `browser-use` cloud provider, or a generic cloud browser API.
Do not silently replace Helium with the clients' own Chrome or turn off the
Chromium renderer sandbox to make an upstream test pass.

## Pinned sources

| Item | Source and identity |
| --- | --- |
| bctl roadmap | [CAMPAIGN-ROADMAP.md @ bc6462a](https://github.com/joshuarli/bctl/blob/bc6462a54a0fb77aa2492816381ca653708087e8/docs/CAMPAIGN-ROADMAP.md) |
| bctl gateway and CDP discovery | [gateway.rs](https://github.com/joshuarli/bctl/blob/bc6462a54a0fb77aa2492816381ca653708087e8/crates/bctl/src/gateway.rs), [endpoint.rs](https://github.com/joshuarli/bctl/blob/bc6462a54a0fb77aa2492816381ca653708087e8/crates/cdp/src/endpoint.rs) |
| bctl current native authority | [automation.md](https://github.com/joshuarli/bctl/blob/bc6462a54a0fb77aa2492816381ca653708087e8/docs/automation.md#authority) |
| browser-use 1.0.0 | [release](https://github.com/browser-use/browser-use/releases/tag/1.0.0), tag commit `070be98109c03f04a95f704533e8f6e9868d0525` |
| browser-use browser selection/ownership | [browser.py](https://github.com/browser-use/browser-use/blob/070be98109c03f04a95f704533e8f6e9868d0525/browser_use/browser/browser.py) |
| browser-use migration | [MIGRATING.md](https://github.com/browser-use/browser-use/blob/070be98109c03f04a95f704533e8f6e9868d0525/MIGRATING.md) |
| browser-use domain guard | [domains.py](https://github.com/browser-use/browser-use/blob/070be98109c03f04a95f704533e8f6e9868d0525/browser_use/domains.py) |
| browser-use remote uploads | [remote_upload.py](https://github.com/browser-use/browser-use/blob/070be98109c03f04a95f704533e8f6e9868d0525/browser_use/remote_upload.py) |
| browser-use tests | [test_agent.py](https://github.com/browser-use/browser-use/blob/070be98109c03f04a95f704533e8f6e9868d0525/tests/ci/test_agent.py), [conftest.py](https://github.com/browser-use/browser-use/blob/070be98109c03f04a95f704533e8f6e9868d0525/tests/ci/conftest.py) |
| browser-harness | `browser-harness==0.1.13` from [pyproject.toml](https://github.com/browser-use/browser-use/blob/070be98109c03f04a95f704533e8f6e9868d0525/pyproject.toml); inspect the frozen dependency before a client bump |
| agent-browser v0.42.0 | [release tag](https://github.com/vercel-labs/agent-browser/releases/tag/v0.42.0), commit `e7519957b9067c541825afa21a792fbe1f483047` |
| agent-browser browser-level CDP | [browser.rs](https://github.com/vercel-labs/agent-browser/blob/e7519957b9067c541825afa21a792fbe1f483047/cli/src/native/browser.rs) |
| agent-browser provider dispatch | [providers.rs](https://github.com/vercel-labs/agent-browser/blob/e7519957b9067c541825afa21a792fbe1f483047/cli/src/native/providers.rs) |
| agent-browser plugin protocol | [plugins.rs](https://github.com/vercel-labs/agent-browser/blob/e7519957b9067c541825afa21a792fbe1f483047/cli/src/plugins.rs), [plugins.mdx](https://github.com/vercel-labs/agent-browser/blob/e7519957b9067c541825afa21a792fbe1f483047/docs/content/docs/plugins.mdx) |

These links establish source behavior **only**, not end-to-end compatibility.
Pins are intentionally not `main`. Any version change requires rereading
the affected attach, ownership and file-transfer code.

## Source-verified integration seams

### bctl as the provider

- `bctl cdp-url NAME` mints a short-lived-in-practice but not TTL-enforced,
  revocable capability of the form
  `http://<token>.localhost:<gateway-port>`. The capability maps to an
  existing named browser session, not a browser-start request.
- `/json/version`, `/json/list` and `/json` are proxied to guest CDP over a
  private Unix socket; `webSocketDebuggerUrl` is rewritten to the gateway
  authority. The gateway rejects browser-Origin CDP requests and refuses
  new connections during `yielding`/`human` authority states.
- Existing WebSockets are not stopped on takeover. `cdp-revoke` and
  `down` do close them. The gateway is currently an opaque CDP proxy, not
  a per-command authorized input gate.
- bctl's own transport special-cases `*.localhost` to 127.0.0.1 while
  preserving the HTTP Host header. **This does not prove upstream Python
  and Rust DNS libraries behave identically.** macOS and Linux must be
  tested with real clients, proxy variables, HTTP discovery, and WebSocket
  upgrade. Do not assume browser WS paths fix unresolved names.
- bctl `exec` jobs run a host process group without shell interpolation,
  with `BCTL_CDP_URL`, `BCTL_OUTPUT_DIR` and `BCTL_RUN`. The process has
  the invoking host user's authority. The browser is in a VM; the Python
  agent/REPL and agent-browser executable are not.
- Each session has a persistent generation; each writable profile is
  exclusive on SmolVM. Nanos currently retains profile state only on its
  disposable disk overlay, not across fresh `down`/`up`.

### browser-use 1.0.0

- A major rewrite deleted the old DOM/action engine in favor of a
  persistent Python REPL through browser-harness and raw CDP. Do **not**
  use 0.x `BrowserSession` examples.
- `Browser()` now creates a Browser Use Cloud session by default.
  `Browser("chromium")` launches a new local Chromium, `Browser("chrome")`
  attaches to the user's Chrome, and `Browser(cdp_url=...)` is the
  requested externally controlled browser. The latter kind is `cdp`.
- `Browser._ws_url` resolves an HTTP(S) endpoint by fetching
  `/json/version`, preserving the input query; an explicit `ws://` or
  `wss://` URL is accepted directly. `Browser(headers=...)` raises:
  custom WebSocket auth headers are unsupported by the pinned daemon.
- The client starts its own named browser-harness daemon and REPL worker.
  Client close/stop is not authorization to call `Browser.close` on
  a bctl-managed Helium process. `keep_alive`, client start/stop, signal
  cleanup and process death must be exercised rather than assumed.
- `allowed_domains` / `prohibited_domains` add a **second CDP WebSocket**
  for the browser-wide `Fetch` guard. This is one logical client with
  multiple sockets. The guard can interfere with other tabs and is not a
  host egress firewall: subresources, worker Python networking and model
  code are beyond its enforcement. bctl's egress policy remains authoritative.
- `remote_upload.py` falls back from `DOM.setFileInputFiles` to transfer
  local file bytes via page-side JavaScript for remote browsers. Use real
  fixture checksums and verify its file size/concurrency limits. In
  `cdp_url` mode the browser uses its own download folder; do not promise
  `workspace/downloads` collection as if this were a cloud/local launch.
- The persistent Python worker executes **model-written Python with the
  host user's file and process access**. Redaction and domain restrictions
  are not a sandbox; additional trust isolation is a separate project.
- Use scripted `FunctionModel` / actual Agent and REPL tests to exercise
  the real code without API charges. Do not let missing model credentials
  trigger an unexpected cloud browser.

### agent-browser v0.42.0

- The native Rust manager speaks CDP directly. It connects to a WS URL,
  an HTTP-discovery endpoint, or a port. Generic `--cdp` and
  `--cdp-headers`/`AGENT_BROWSER_CDP_HEADERS` are separate from browser
  *provider* support.
- Its HTTP discovery obtains a browser-level WebSocket URL via the
  supplied host/port and `/json/version`, not an arbitrary base-path
  preserve operation. A full browser WS URL may avoid path-prefix issues
  but still needs resolvable hostname/auth.
- The generic externally attached browser is not killed when the client
  closes. Its own client daemon, target selection and file behavior still
  require real interop tests. Cross-origin frames and tab selection are
  client concerns, not mappings from bctl's native element refs.
- The plugin system executes a configured local command, one JSON
  stdin/stdout request at a time, with protocol
  `agent-browser.plugin.v1`. `browser.provider` receives
  `browser.launch`/`browser.close`; manifest discovery uses
  `plugin.manifest`. A response supplies `browser.cdpUrl`,
  `directPage`, `metadata`, `cleanup`. It has no documented
  authentication-header field. Standard plugin provider launch/close
  waits are bounded (60 s / 15 s in v0.42.0); cleanup can be
  best-effort. bctl must independently reconcile owned sessions.
- `DOM.setFileInputFiles` receives the caller's path unchanged, not
  bytes staged into the VM. External browsers cannot read arbitrary
  host file paths. Similarly, browser download events do not turn
  remote guest paths into local files without an explicit transfer.
- A provider plugin is **optional**. First prove unmodified generic CDP;
  a small bctl-owned provider executable is then preferable to a fork
  or vendored agent-browser dependency.
- The built-in provider named `browser-use` targets Browser Use Cloud,
  *not* the open-source browser-use Python agent.

## Planned compatibility surface

E1 qualified the first row on macOS arm64; the other rows are not qualified.

| Capability | browser-use 1.0.0 | agent-browser v0.42.0 | bctl/runtime prerequisite |
| --- | --- | --- | --- |
| Attach and detach existing Helium | `Browser(cdp_url=...)` | generic `--cdp` | reachable, revocable gateway URL |
| Navigate / JS / AX / screenshot | direct CDP + REPL | native CDP CLI | correct target and event routing |
| Multiple CDP sockets | domain guard uses extra one | client may create many sessions | single *logical* controlling lease (E2: done) |
| Browser survives client close | externally owned semantics | external process is not launched by client | bctl owns `down` |
| Human takeover | no cooperative contract | no bctl-specific cooperative contract | refuse until controller releases (E2: done) |
| File upload | existing remote-byte fallback, test it | host path cannot work remotely | staged or streamed guest bytes (E4: a guest path in the files directory) |
| File download | external-CDP uses browser's download directory | guest-side download paths | bounded artifact collection (E4: downloads land in the files directory) |
| Named provider | external `Browser(...)` recipe first | `browser.provider` plugin | managed launch + opaque cleanup (E5: `bctl agent-browser-plugin`) |
| Persisted profiles | supplied by bctl, not library | supplied by bctl, not CLI | durable backing and exclusive writer |

**Runtime matrix**, interpreted as observed bctl status (not an upstream
interop result):

| Area | SmolVM/macOS arm64 Helium | hydrogen/Nanos/Linux x86_64 Helium |
| --- | --- | --- |
| Browser rendering | headed Tinyland, live VNC | headless CDP, no VNC |
| CDP / screenshots | available | available |
| Human keyboard/pointer takeover | available for bctl native control | unsupported, no input devices |
| VM `pause`/`resume` | available in current bctl | `UNSUPPORTED_BY_RUNTIME` |
| Profile after fresh `down`/`up` | host-backed persistent | not established; disk overlay is disposable |
| Guest file exchange | via documented SmolVM mechanisms | no `machine exec` equivalent; design explicitly |
| Private egress | blocked unless allowed | policy-checked host SOCKS5 over vsock |

Do not conflate VM checkpoints, app-level run continuation, persistent
profiles, and client REPL persistence. In particular, browser-use
`keep_alive` preserves the client session, not an Nanos profile disk.

## Runtime results (E1)

`tests/interop/run.sh` runs the pinned clients against a real session and a
host fixture site, and exits non-zero on any failed check. On 2026-11-10, on
an Apple M5 Max with macOS 27.0.1 and bctl `aca8175`, the final suite passed
all 55 checks on SmolVM 1.34.0 (headed Helium 0.20.3.1) and on Nanos
(headless Helium 0.20.3.1, hydrogen `12600ce1` on libkrun). Earlier runs of
the same checks passed twice on SmolVM and once on Nanos; one Nanos run
failed on a race in the suite's own dialog cell, since fixed. The clients were
browser-use 1.0.0 with browser-harness 0.1.13, and agent-browser 0.42.0.
Linux x86_64 with Nanos has not been run and stays unverified.

| Check | browser-use 1.0.0 | agent-browser 0.42.0 |
| --- | --- | --- |
| `<token>.localhost` discovery and browser WS | pass | pass |
| `Browser.getVersion`, targets | pass | `tab list` |
| Form fill and click, seen by the fixture | pass, `default` and `ultrafast` | pass |
| Accessibility tree | `Accessibility.getFullAXTree`, `bu.state()` | `snapshot` |
| Screenshot PNG bytes | pass | pass |
| `confirm` dialog | pass | pass, with `--no-auto-dialog` |
| Popup target | pass | pass |
| Same- and cross-origin iframe | pass | pass, by snapshot ref |
| 10 attach/detach cycles, no socket left open | 10/10 | 10/10 |
| Proxy variables set, `NO_PROXY=localhost` | pass | pass |
| Helium still `ready` after client close | pass | pass |
| Revoked URL cannot attach | pass | pass |

The gateway refused a browser `Origin` (403), a bare `127.0.0.1` Host (401)
and an unknown token (401). `cdp-revoke` refused new attachments and closed
the open sockets within a second.

Findings that a recipe must account for:

- **agent-browser can bind a hidden tab.** On attach it takes an existing
  page target without `Page.bringToFront`. With several tabs open, that tab
  reported `visibilityState` `hidden` on both runtimes. Chromium dropped the
  clicks sent there, while each command still printed `Done`. Run
  `agent-browser tab new URL` (or `tab <id>`) after attaching.
- **agent-browser `frame <css>` matches the frame's name only.** A selector
  for `<iframe id=x>` without `name` fails with `Frame not found`. `frame
  @eN` from `snapshot` works, including for a cross-origin frame in its own
  process.
- **agent-browser `get title` can answer from its cache** after its socket
  is gone. Use `eval` to test whether a connection is live.
- **browser-use `ultrafast` accepts dialogs automatically**, as `ax.py`
  documents. `default` leaves them to the cell.
- **browser-use's domain guard adds a socket.** A guarded run held two
  gateway sockets, and both closed on `stop()`. A main-frame navigation to
  another site, started from page JavaScript, was blocked.
- **`Agent.run()` does not raise when the attach is refused.** It logs the
  error and returns no result. `Browser(cdp_url=...).start()` raises.
- **`bctl doctor` checked only SmolVM** and reported a Nanos home as
  failing. It now checks the host's runtime, and names a Nanos image missing
  its `profile.img` (or, on Linux, its `kernel.img`).

## Security and correctness gates

1. CDP capability URLs grant browser control. Never log their complete
   values, persist them in dashboard metadata, reveal them in artifacts,
   or publish them in examples. Ensure discovery and WS authorization
   agree and that revocation closes **all** connections in that lease.
2. A browser's own sandbox and egress rules stay enforced. Never use
   `--no-sandbox`, open raw guest TCP debugging ports, mount the host
   home, or silently allow loopback/RFC1918/IPv6 private addresses.
3. One logical controlling client per session. A browser-use domain
   guard can use a second socket under the same owner. Before managed
   leases land, existing CDP access is **not exclusive** against a
   human: takeover only blocks *new* CDP connections.
4. External client cleanup is not session destruction unless the
   client explicitly owns a managed ephemeral session. Cleanup must
   compare generation/attempt and be idempotent; stale clients cannot
   tear down a replacement session.
5. A response lost after mutating CDP input is an unknown outcome.
   Never silently retry in the controller. bctl cannot guarantee how
   another client retries; preserve actual client behavior in receipts.
6. Model-written Python runs on the host and can read host files and
   processes. Do not present browser VM isolation as REPL isolation.
7. File transfers must verify bytes (including nonempty binary),
   bounded sizes and names, path traversal/symlinks, unfinished
   downloads, collision policy, failure cleanup and artifact origin.
8. Client runtimes or provider plugins are not linked or vendored into
   the bctl product. Optional tooling is tested at pinned versions.

## Required proof plan

The M8C E0–E6 items define owners and acceptance. The immediate
independent E1 proof should run a fixture through the *real released*
clients using both the HTTP discovery URL and advertised browser WS
URL, then revoke and reconnect. Record OS, client, bctl commit,
runtime, Helium archive digest, browser `Browser.getVersion`, number
of attempts, outcome and command timings. Use fixed local pages for
form/action/AX/screenshot/iframe/dialog tests; no website-specific
anti-bot or LLM claims. For browser-use, exercise its guard's extra
connection and deterministic model-scripted Agent; for agent-browser,
its released CLI with explicit `--cdp` and a unique client session.

Minimum failure injections: DNS-resolution failure, wrong/revoked
capability, inaccessible CDP endpoint, browser process exit, host
client crash, two competing clients, competing human takeover, daemon
restart, client detach while browser continues, ambiguous side-effect
reply, and stale cleanup after a new session generation.

After E1, build managed ownership before promising seamless takeover
or a provider plugin. Implement guest file transfer only when its
runtime and threat model are established. A missing host or unsupported
operation remains **unverified** rather than being recorded as pass.

## Practical examples (proposals; not yet validated)

Existing manual bctl attachment starts with:

~~~sh
bctl up research
bctl cdp-url research       # sensitive capability URL; do not print in logs
~~~

A host-side browser-use program can obtain `BCTL_CDP_URL` from an
existing `exec` run; the core shape is:

~~~python
import os
from browser_use import Agent, Browser

async def drive(llm):
    browser = Browser(cdp_url=os.environ["BCTL_CDP_URL"])
    agent = Agent("Interact with the local deterministic fixture", llm=llm, browser=browser)
    try:
        return await agent.run()
    finally:
        await agent.close()
        await browser.stop()  # closes this client's harness; bctl still owns Helium
~~~

This example does not specify an actual model: tests should inject
a scripted, zero-cost FunctionModel; human workloads choose a model
explicitly. Verify the exact cleanup API against the pinned release.
No browser-use cloud key is intended.

For agent-browser, the general form is:

~~~sh
agent-browser --session unique-client-run --cdp "$BCTL_CDP_URL" open https://example.com
agent-browser --session unique-client-run close
~~~

These are **integration sketches**, not evidence the current bctl
capability URL resolves or that this CLI composition passes. Validate
the exact pinned CLI syntax, the program's own daemon lifetime, and
teardown before shipping a copy-paste recipe. The optional provider
would be `--provider bctl` with a small, trusted, explicitly configured
`browser.provider` plugin and an opaque ownership token.

## Non-goals and maintenance

No cloud/browser-use API emulation, Playwright replacement, duplicated
browser automation layer, Python/TypeScript bindings to bctl, headless
VM masquerading as headed, input takeover on Nanos, browser impersonation,
CAPTCHA solving, proxy product, remote-workspace filesystem mount,
model hosting, or agent-browser full feature parity. When upstream
client behavior changes, pin a new version and rerun the compatibility
matrix. Keep failed and skipped checks visible.
