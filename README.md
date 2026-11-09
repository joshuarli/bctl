# bctl

`bctl` (browserctl) runs headed browsers inside small Linux VMs on an Apple
Silicon Mac, shows each one live in a local dashboard, and lets automation
attach to it over CDP. See [the campaign](docs/CAMPAIGN-ROADMAP.md) for the
design and its status, and [the image contract](docs/image-contract.md) for
what an appliance image provides.

## Build

Requirements: macOS on Apple Silicon, Xcode Command Line Tools, rustup with the
`nightly-2026-09-15` and `stable` toolchains, Node 26 with pnpm, Docker
(OrbStack works), Homebrew's `libkrun/krun/virglrenderer-krun` with `libepoxy`
and `molten-vk`, and `rustup target add --toolchain
nightly-2026-09-15 aarch64-unknown-linux-musl`.

```sh
runtime/build.sh                       # SmolVM, libkrun, libkrunfw and the guest agent, from source
BCTL_BROWSER_ARCHIVE=DIR images/build.sh helium   # the product appliance, into .work/images
pnpm --dir web install && pnpm --dir web build   # the dashboard, embedded by the next step
cargo build -p bctl
```

The runtime is built from the SmolVM source vendored in `third_party/smolvm`,
at the upstream default-branch tip pinned by `third_party/UPSTREAM.lock`, rather
than from a release bundle. bctl patches and fixes that source, so it lives
in-tree. [`docs/runtime.md`](docs/runtime.md) describes what is vendored, how
the build is pinned, what it verifies, and how to refresh it
(`scripts/update-smolvm.sh`).

The kernel and the guest agent are built in a pinned Debian container, because
this host is macOS.

`DIR` holds chromium-build's `helium-*-linux-aarch64-musl.tar.xz` with its
`.sha256` and `.acceptance.json` (`docs/image-contract.md`). The reference
images (`images/build.sh ref-alpine-chromium` and the others under `images/`)
stay as diagnostics; select one with `bctl up NAME --image IMAGE`.

`bctl` keeps its state in `~/Library/Application Support/bctl` (or
`$BCTL_HOME`). Put the runtime and images there:

```sh
home="$HOME/Library/Application Support/bctl"
mkdir -p "$home/runtime" "$home/images"
ln -sfn "$PWD/.work/runtime/smolvm" "$home/runtime/smolvm"
ln -sf "$PWD/.work/images/helium.tar" "$home/images/"
target/debug/bctl doctor
```

## Use

```sh
bctl up research                               # start a browser session on helium
bctl serve --open                              # open the dashboard
bctl profile login personal                    # sign in to sites by hand
bctl status
bctl grab https://example.com/ --session research   # crawl it with Grab
bctl run mirror-docs --session docs            # run a job from ./bctl.toml
bctl runs                                      # runs and their outcomes
bctl pause research                            # checkpoint its VM; the browser keeps its pages
bctl resume research                           # restore it, with the same tabs and page state
bctl down research                             # stop it; the profile stays
```

`bctl pause` and `bctl resume` take a session name or a run id; an argument
made only of digits is a run id. A paused session holds its profile, refuses
page and tab commands with `SESSION_NOT_READY`, and drops open DevTools
connections, which reconnect after `bctl resume`.

Jobs, runs, and their outcomes are described in [docs/jobs.md](docs/jobs.md).

Automation attaches to a CDP control URL from `bctl cdp-url NAME` (for example
Shadowdriver's `control_url`, Playwright's `connectOverCDP`). The URL is a
capability served by the gateway: its host is `<token>.localhost`, so clients
that keep only a URL's host and port still present it. `bctl cdp-revoke NAME`
(or `bctl down NAME`) revokes it and closes its DevTools connections. While a
person has control of the session, or is taking it, new attachments are
refused.

bctl's own automation acts on one selected page target per session, named by
its CDP target id:

| Command | Effect |
|---------|--------|
| `bctl tab list S` | Lists page targets; `*` marks the selected one. |
| `bctl tab new S [URL]` | Opens a page target (default `about:blank`) and selects it. |
| `bctl tab use S TARGET_ID` | Selects a page target. |
| `bctl tab close S TARGET_ID` | Closes a page target; closing the selected one leaves none selected. |

With `--json` the result is `{"tabs":{"selected":ID|null,"targets":[{"id","title","url"}]}}`.
The selection survives a daemon restart and ends with the session's generation
(`bctl down`). Only these commands move it: a popup or a tab a person
activates never does. If the selected target disappears, the commands fail
with `TARGET_GONE`. The error's `target` names the vanished target and
`targets` lists the remaining ones; nothing is selected in its place. The
other codes are `NO_SUCH_TARGET`, `LAST_TARGET` (closing the last page
target would end the browser), `HUMAN_IN_CONTROL` (`tab new` and `tab close`
while a person has control or is taking it), `UNKNOWN_OUTCOME` (the browser may or
may not have acted; bctl never retries), and `CDP_FAILED`. URLs shown or
stored by these commands have no userinfo, query, or fragment.

`bctl page` acts on the selected target: `snapshot` prints its accessibility
tree with element refs (`@e12`), and `goto`, `click`, `fill`, `type`, `press`,
`select`, `check`, `wait`, `get`, `screenshot`, `eval`, and the rest act on
an element named by a ref or a query (`--role button --name Go`, `--label`,
`--css`, ...) with real input. `bctl page batch S FILE` runs a JSON list of
those actions in order and stops at the first failure; it is not a
transaction. The same automation is a Rust library, `bctl-automation`.
[docs/automation.md](docs/automation.md) describes the commands, the socket
protocol, batches, the library, ref lifetimes, and the known limits.

"Take control" in the dashboard first moves the session to `yielding`: native
commands that would change the page are refused, a running `wait` ends with
`TAKEN_OVER`, and the person's input reaches the browser once the commands in
flight have ended. "Resume automation" hands control back and resumes a run
that waits for a person. See the authority section of
[docs/automation.md](docs/automation.md).

The guest's CDP is exposed to the host only as a Unix socket in a 0700
directory under `BCTL_HOME`; no host TCP port reaches it, so the gateway is the
only way in. Guests launch with `--block-private`: the public internet is open,
while RFC 1918, link-local, and host loopback addresses are not, unless the
session was started with `bctl up NAME --allow-private CIDR`.

## Test

```sh
cargo test --workspace                                   # no VMs
cargo test -p bctl-runtime-smolvm --test lifecycle -- --ignored
cargo test -p bctl --test daemon -- --ignored
cargo test -p bctl --test profiles -- --ignored
cargo test -p bctl --test cdp -- --ignored
cargo test -p bctl --test egress -- --ignored
cargo test -p bctl --test tabs -- --ignored
cargo test -p bctl --test snapshots -- --ignored
cargo test -p bctl --test actions -- --ignored
cargo test -p bctl --test batches -- --ignored
cargo test -p bctl --test authority -- --ignored
BCTL_GRAB="$PWD/../shadowdriver/grab/.venv/bin/grab" cargo test -p bctl --test runs -- --ignored
cargo run -p bctl-proof -- m1 --runs 10
BCTL_PROOF_IMAGE=.work/images/ref-alpine-tinyland.tar cargo run -p bctl-proof -- m1
pnpm --dir web exec playwright test                      # needs cargo build -p bctl
```

The ignored tests and the proofs boot real VMs from `.work/runtime` and
`.work/images`.
