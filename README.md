# bctl

`bctl` (browserctl) runs browser appliances in small VMs, exposes their CDP
endpoints to automation, and manages sessions, profiles, and jobs.

| Host | Runtime | Browser interface |
| --- | --- | --- |
| Apple Silicon macOS | Nanos, the default | Headless browser with CDP and job controls |
| Linux x86_64 with KVM | Nanos, the default | Headless browser with CDP and job controls |
| Apple Silicon macOS | SmolVM, selected with `BCTL_RUNTIME=smolvm` | Headed browser with a live dashboard and keyboard/pointer input |
| Linux x86_64 with KVM | SmolVM, selected with `BCTL_RUNTIME=smolvm` | Headed browser with a live dashboard and keyboard/pointer input |

Releases ship only Nanos; SmolVM is built from source for differential testing.

## Install

Install a release with `~/usr`'s `sh sync tools bctl`, or download the
archive for your platform from a `release-<commit>` release, then run
`bctl image pull`. [Distribution](docs/distribution.md) describes the
artifacts. To build from source, follow
[the build and installation instructions](docs/runtime.md#build-and-install)
for your host. The [image contract](docs/image-contract.md) describes the
Helium archive and sidecars each image builder requires.

State and images live under `~/Library/Application Support/bctl` on macOS.
On Linux, state lives under `${XDG_STATE_HOME:-$HOME/.local/state}/bctl` and
runtimes and images under `${XDG_DATA_HOME:-$HOME/.local/share}/bctl`.
`BCTL_HOME` overrides both. Run `bctl doctor` after installing the runtime and images.

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

## Kernel

`kernel/` holds Hydrogen, bctl's fork of [Nanos](https://github.com/nanovms/nanos),
which runs Chromium's multiprocess sandbox in one microVM. It keeps Nanos's
Apache-2.0 license in `kernel/nanos/LICENSE`. [Kernel development](docs/kernel/development.md)
covers its layout, rules, toolchains, and qualification suites.

## Project status

The [active campaign](docs/CAMPAIGN-ROADMAP.md) records the roadmap and
qualification status. [TODO.md](TODO.md) tracks review findings. The [graphics performance campaign](docs/CAMPAIGN-GRAPHICS-PERFORMANCE.md) prioritizes headless attribution while coordinating upstream tinyland damage and headed host/viewer optimization.
