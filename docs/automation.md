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
- It fails with `STALE_REF` once the node is removed, its frame navigates or
  is gone, it is used on another target, or it came from another connection
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
- **Commands in flight.** A takeover lets settling commands end on their own,
  except where they wait: a `wait`, or a `goto` waiting for its page to load,
  ends within about 100 ms with `TAKEN_OVER`, and a batch stops before its next
  step with `TAKEN_OVER`. A `goto` that ends this way has already sent its
  navigation. Nothing is retried. The browser, the selected tab, and the
  profile are untouched.
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

- Actions fail at once when their element is not actionable; they do not wait
  for it. Use a `wait` first.
- A popup an action opens is not reported; `bctl tab list` shows it.
- Node liveness for refs is checked in the page's main world; a node moved
  between two same-process frames is not detected.
- One very large DOM read can exceed the 32 MiB CDP message cap.
- A host program's own connection is not interrupted when a person takes
  control in the dashboard; the gateway only refuses new attachments.
  `Pages::act` takes an `Interrupt`, which a host can use to stop its own
  waits.
- A read begun after a takeover, such as a long `wait`, is not interrupted by
  it and holds the session's page lock until it ends.
