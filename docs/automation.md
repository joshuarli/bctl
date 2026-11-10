# Native automation

bctl drives a session's browser itself over CDP, without Grab, Shadowdriver,
or Playwright. There are two ways in:

- **The daemon**, through `bctl tab`, `bctl page`, or the daemon socket. The
  daemon owns one CDP connection per session generation, keeps the selected
  target in SQLite, and hands out element refs that survive between commands.
- **The `bctl-automation` crate**, linked into a host program that opens its
  own CDP connection through a `bctl cdp-url` capability.

Both run the same code: the daemon is a host program of the crate.

## Identity

Every value hangs off the one before it:

| Level | Identity | Ends when |
|-------|----------|-----------|
| Session generation | `generation` in `bctl inspect` | `bctl down`, or a new `bctl up` |
| Page target | CDP `targetId`, as `bctl tab list` shows it | the tab closes |
| Frame and document | the frame id and the `loaderId` it committed | the frame navigates or goes away |
| Element ref | `e12`, as a snapshot shows it | its node leaves its document, its document ends, or it expires |

A session's native commands act on one *selected* target. Only `bctl tab
new`, `tab use`, and `tab close` move the selection; a popup, a tab a person
activates, or a target that closes never does. When the selected target is
gone, commands fail with `TARGET_GONE` and nothing is selected in its place.
The selection is stored with the generation: it survives a daemon restart and
is forgotten by the next generation.

## Refs

`bctl page snapshot` gives each interactive node a ref. A ref is scoped to the
generation, target, frame, and document it was read from and names one backend
DOM node:

- It keeps working across same-document changes while its node stays in the
  document.
- It fails with `STALE_REF` once the node is removed or moved into another
  document, its frame navigates or is gone, it is used on another target, or it came from another connection
  (an earlier generation or daemon). bctl never re-resolves a stale ref by
  similar role or text.
- Its number is never handed out again: the daemon reserves numbers from
  SQLite, so a ref from before a restart cannot name a node after it.
- Each connection keeps at most 50 000 live refs; past that the oldest expire
  and fail as stale.

## Commands and the socket

The daemon listens on `$BCTL_HOME/bctl.sock`. A client writes one JSON request
on one line and reads one JSON response line; the CLI is such a client, and
`--json` prints exactly the response. A request is

```json
{"op": "page-click", "session": "research", "allow_private": [],
 "action": {"role": "button", "name": "Sign in", "values": [], "gone": false}}
```

`op` is a CLI command with a dash (`tab-list`, `page-snapshot`, `page-click`,
...). Fields that are not optional in `crates/bctl/src/protocol.rs`
(`allow_private`, and an action's `values` and `gone`) must be present. A
response's `error`, when set, has a stable `code` that scripts match on, a
`message` that may change, an optional `remedy`, and for page errors the
`target` it concerns.

## Actions

An action that names an element runs in two parts.

1. **Preparation** finds the element and checks what the action needs of it:
   that it is there, visible, enabled (for everything but `hover`, `scroll`,
   and `get`), editable (for `fill`), and for a click or hover, that it stays
   in place for two animation frames once scrolled into view and is the
   topmost element at the point the input goes to, in its frame and in every
   frame above it. An action that sends input also waits until the page shows
   its document: the browser drops input to a new page until it paints some
   content or is 500 ms old. When a check fails, bctl waits for the page to change (a
   DOM mutation, a transition or animation ending, a scroll, a resize, a
   navigation, or at most 200 ms without any) and checks everything again.
   Preparation sends no input.
2. **Input** is sent once, when every check has passed. It is never retried.

Preparation gives up at the action's deadline, `--timeout MS` after the
action began (`timeout_ms` in a batch step or on the socket; 5000 by default,
at most 300 000). The error carries the code of the check that failed last,
such as `NOT_VISIBLE`, `DISABLED`, `COVERED`, `NOT_STABLE`, or `NOT_FOUND`,
and says how long bctl waited. `goto` and `wait` use the same option for
their own deadlines (30 s and 10 s by default); `screenshot`, `eval`, and
`dialog` take none.

A query is evaluated again at every check, so it follows the page: if its
element is replaced, or its document navigates, the next check finds the new
one. A ref names one node for good: if that node leaves its document, moves
into another, or its document ends while the action waits, the action fails
with `STALE_REF` at once.

Once input has been sent, a lost reply, or a connection that ends before the
action completes, fails it with `UNKNOWN_OUTCOME`: the input may have acted.
A click that bctl sent but that the page did not receive where it was aimed
fails with `MISSED` or `NOT_ACTIONABLE`, and is not sent again either.

## Batches

`bctl page batch SESSION FILE` (or `-` for stdin) and the `page-batch` op run a
JSON list of page actions in order:

```json
[{"action": "goto", "url": "https://example.com/login"},
 {"action": "fill", "label": "Email", "text": "me@example.com"},
 {"action": "click", "role": "button", "name": "Continue"},
 {"action": "wait", "text": "Welcome", "timeout_ms": 15000}]
```

Each step's `action` names a page action (`goto`, `click`, `dblclick`,
`fill`, `type`, `press`, `hover`, `scroll`, `select`, `check`, `uncheck`,
`wait`, `get`, `screenshot`, `eval`, `dialog`); its other fields are that
action's request fields, and fields an action does not have are refused.
Every step is checked before any runs, so a malformed step anywhere runs
nothing. A batch holds 1 to 1000 steps.

The steps run over the session's one connection on the target that was
selected when the batch began, and no other native command on the session runs
between them. The batch stops at the first step that fails. The response's
`page.batch.ran` lists each step that ran with its `result` or `error` and its
`elapsed_ms`; the failed step is last, and the top-level `error` repeats its
code with the step's position in the message.

A batch is not a transaction. Steps before the failure stay done; nothing is
rolled back, and a step that failed after sending input may have acted. A step
whose command was sent but whose reply never came fails `UNKNOWN_OUTCOME` and
is not retried, here or in a later batch. External CDP clients, Grab runs, and
a person can still act on the page between steps.

## Authority

Each session has one control authority, owned by the daemon and kept in
memory: `automation`, `yielding`, or `human`. A daemon restart starts every
session under `automation`.

| From | Event | To |
|------|-------|----|
| `automation` | a viewer takes control, no native command settling | `human` |
| `automation` | a viewer takes control, native commands settling | `yielding` |
| `yielding` | the last native command begun under `automation` ends | `human` |
| `yielding` or `human` | the same viewer resumes automation, or its live view closes | `automation` |
| any | `bctl down` | `automation` |

Taking or resuming as another viewer while a person holds control, or is
taking it, is refused with HTTP 409 and changes nothing. A native command is
*settling* from the moment it starts under `automation` until it returns.

- **Native commands.** Under `yielding` and `human`, commands that change the
  page (`goto`, `click`, `fill`, `eval`, `tab new`, `tab close`, a batch with
  any such step, and the rest) fail with `HUMAN_IN_CONTROL` before doing
  anything. Reads (`snapshot`, `resolve`, `get`, `wait`, `screenshot`, `tab
  list`, `tab use`) still run.
- **Commands in flight.** A takeover ends a settling command with
  `TAKEN_OVER` anywhere before it sends input or changes the page: while an
  action prepares (finding its element, checking it, scrolling it into view,
  hit-testing it), during a `wait`, or between batch steps, within about
  20 ms plus one CDP call. A command that has begun sending input runs to its
  end, and a `goto` waiting for its page to load ends within 20 ms; it has
  already sent its navigation. Nothing is retried. The browser, the selected
  tab, and the profile are untouched.
- **No input after a takeover.** Right before its first input, a command asks
  the session's authority once more, under the lock that a takeover takes to
  leave `automation`, with nothing awaited between the answer and sending the
  input. So either the takeover came first and nothing is sent, or the input
  began under `automation` and `human` waits for the command to end.
- **The RFB input lease** is held exactly in `human`: until then the viewer's
  keyboard and pointer input is dropped, and the dashboard shows `Yielding`
  with the number of commands it waits for.
- **CDP attachments.** Under `yielding` and `human` the gateway refuses new
  attachments through `bctl cdp-url` capabilities with HTTP 409. Connections
  already open, such as a run's, are not cut.
- **Runs.** Resuming automation also resumes the session's run if it is in
  `needs-interaction`. A takeover does not wait for a running run: neither
  Grab nor an exec job has a boundary to pause at without ending the attempt.

## The Rust API

`crates/automation` (`bctl-automation`) is the library. A host program:

1. connects with `bctl_automation::cdp::connect` to the browser websocket a
   `bctl cdp-url NAME` capability names, and runs the returned driver to
   completion on a thread of its own;
2. supplies a `BindingStore`, which keeps the selected target per generation,
   and a `RefCounter`, which must never return a number twice for as long as
   any ref it numbered may be presented;
3. selects a target with `Tabs::open` or `Tabs::select` and reads it back with
   `Tabs::selected`, which returns a `TargetScope`;
4. reads `Pages::snapshot` and runs `Pages::act` with an `Action` on a
   `Locator`: a ref, or a `Query` by CSS, role and name, label, placeholder,
   or test id, with an `Occurrence` to choose among several matches.

`crates/automation/examples/drive.rs` does this end to end:

```sh
cargo run -p bctl-automation --example drive -- "$(bctl cdp-url NAME)" https://example.com/
```

The example connects through a capability URL rather than through the daemon.
That way the program is the single owner of its connection and of the refs
issued on it: nothing else sends on it, and its refs are meaningless to the
daemon and the reverse. Going through the daemon instead would make the daemon
the owner and the program a client of its socket protocol, which is what the
CLI already is. The capability is still the gateway's: `bctl cdp-revoke` or
`bctl down` closes the connection.

## Known limits

- A popup an action opens is not reported; `bctl tab list` shows it.
- One very large DOM read can exceed the 32 MiB CDP message cap.
- A host program's own connection is not interrupted when a person takes
  control in the dashboard; the gateway only refuses new attachments.
  `Pages::act` takes an `Interrupt`, which a host can use to stop its own
  waits.
- A read begun after a takeover, such as a long `wait`, is not interrupted by
  it and holds the session's page lock until it ends.

## External automation clients (planned M8C)

Browser-use 1.0.0 and agent-browser v0.42.0 are **proposed CDP consumers**,
not yet qualified against bctl. Both should attach from the host through
the `bctl cdp-url NAME` gateway (not run inside the browser appliance).
Their browser driver, tab bindings and element references remain separate
from bctl's native automation. See
[external-client research](research/external-clients.md) and
[M8C](CAMPAIGN-ROADMAP.md#m8c-external-browser-clients-browser-use-100-and-agent-browser-v0420).

**Current exclusivity limitation:** `yielding`/`human` blocks **new** CDP
attachments, but does not revoke connections already in use. Therefore a
running external client can still mutate the page during human control.
Until M8C ownership is implemented, do not claim an exclusive human
takeover against an uncontrolled external CDP client. The proposed initial
policy refuses takeover while a managed external controller is active,
rather than pretending arbitrary Python/CLI automation can pause.
One browser-use controller may legitimately use multiple WebSockets
(e.g. a separate domain guard).

Client disconnect must leave a borrowed bctl browser alive. In contrast,
an explicitly bctl-owned ephemeral session needs generation-checked
reconciliation and expiry after external client failure. Both semantics
require real-client tests before being described as supported.
