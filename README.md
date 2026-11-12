# bctl

`bctl` (browserctl) runs browser appliances in small VMs, exposes their CDP
endpoints to automation, and manages sessions, profiles, and jobs.

| Host | Runtime | Browser interface |
| --- | --- | --- |
| Apple Silicon macOS | SmolVM, the default | Headed browser with a live dashboard and keyboard/pointer input |
| Linux x86_64 with KVM | Nanos, the default | Headless browser with CDP and job controls |
| Apple Silicon macOS | Experimental Nanos, selected with `BCTL_RUNTIME=nanos` | Headless browser with CDP and job controls |

## Install

Follow [the build and installation instructions](docs/runtime.md#build-and-install)
for your host. The [image contract](docs/image-contract.md) describes the
Helium archive and sidecars each image builder requires.

State lives under `~/Library/Application Support/bctl` on macOS and
`${XDG_STATE_HOME:-$HOME/.local/state}/bctl` on Linux. `BCTL_HOME` overrides
that directory. Run `bctl doctor` after installing the runtime and images.

## Use

```sh
bctl up research
bctl serve --open
bctl status
bctl cdp-url research
bctl page snapshot research
bctl grab https://example.com/ --session research
bctl run mirror-docs --session docs
bctl runs
bctl down research
```

`bctl up` chooses `helium` for SmolVM and `nanos-helium` for Nanos. Select an
installed alternative with `--image IMAGE`. Use `--allow-private CIDR` when a
session must reach a private network. Stopping a session retains its profile.
The two runtimes use different profile formats; an existing profile cannot
be opened with the other runtime.

SmolVM also supports `bctl profile login personal`, `bctl pause research`,
and `bctl resume research`. Pause preserves the VM's pages and browser state.
Nanos has no checkpoint or viewer-input support. Headless sessions still
appear in the dashboard, with their status and run controls.

Jobs are defined in `bctl.toml`. [Jobs and run outcomes](docs/jobs.md) describes
configuration, cancellation, verification, logs, and artifacts.

## Automate a browser

Attach through the URL returned by `bctl cdp-url NAME`, such as with
Playwright's `connectOverCDP` or Shadowdriver's `control_url`. The gateway
encodes the capability in a `<token>.localhost` host, so clients retaining
only the URL's host and port still present it. `bctl cdp-revoke NAME` and
`bctl down NAME` revoke it and close its DevTools connections.

Native automation acts on one selected page target per session. Use
`bctl tab list`, `new`, `use`, and `close` to manage that selection. Popups
and a person's tab activation do not move it. If the selected target goes
away, commands return `TARGET_GONE` instead of choosing another target.

`bctl page snapshot` returns an accessibility tree with element refs such as
`@e12`. The page commands act through refs or queries such as `--role`,
`--label`, and `--css`. `bctl page batch S FILE` executes a JSON action list
in order and stops at the first failure. The same operations are available
through the `bctl-automation` Rust library. See [automation](docs/automation.md)
for commands, result shapes, ref lifetimes, and current limits, and
[client compatibility](docs/compatibility.md) for external clients.

On a headed session, "Take control" first enters `yielding`. Human input is
enabled when bctl's tracked native commands finish. "Resume automation"
hands control back and resumes a run waiting for interaction. See
[authority](docs/automation.md#authority) for external-controller behavior.

## Project status

The [active campaign](docs/CAMPAIGN-ROADMAP.md) records the roadmap and
qualification status. [TODO.md](TODO.md) tracks review findings.
