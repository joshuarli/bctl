# bctl

`bctl` (browserctl) runs headed browsers inside small Linux VMs on an Apple
Silicon Mac, shows each one live in a local dashboard, and lets automation
attach to it over CDP. See [the campaign](docs/CAMPAIGN-ROADMAP.md) for the
design and its status, and [the image contract](docs/image-contract.md) for
what an appliance image provides.

## Build

Requirements: macOS on Apple Silicon, Xcode Command Line Tools, rustup,
Node 26 with pnpm, Docker (OrbStack works), Homebrew's
`libkrun/krun/virglrenderer-krun`, and `rustup target add --toolchain
nightly-2026-09-15 aarch64-unknown-linux-musl`.

```sh
runtime/build.sh                       # SmolVM with bctl's libkrun, into .work/runtime/smolvm
images/build.sh ref-alpine-chromium    # the reference appliance, into .work/images
pnpm --dir web install && pnpm --dir web build   # the dashboard, embedded by the next step
cargo build -p bctl
```

`bctl` keeps its state in `~/Library/Application Support/bctl` (or
`$BCTL_HOME`). Put the runtime and images there:

```sh
home="$HOME/Library/Application Support/bctl"
mkdir -p "$home/runtime" "$home/images"
ln -sfn "$PWD/.work/runtime/smolvm" "$home/runtime/smolvm"
ln -sf "$PWD/.work/images/ref-alpine-chromium.tar" "$home/images/"
target/debug/bctl doctor
```

## Use

```sh
bctl up research --image ref-alpine-chromium   # start a browser session
bctl serve --open                              # open the dashboard
bctl profile login personal --image ref-alpine-chromium   # sign in to sites by hand
bctl status
bctl down research                             # stop it; the profile stays
```

Automation attaches to a CDP control URL from `bctl cdp-url NAME` (for example
Shadowdriver's `control_url`, Playwright's `connectOverCDP`). The URL is a
capability served by the gateway: its host is `<token>.localhost`, so clients
that keep only a URL's host and port still present it. `bctl cdp-revoke NAME`
(or `bctl down NAME`) revokes it and closes its DevTools connections. While a
person holds the dashboard input lease, new attachments are refused.

SmolVM publishes the guest CDP port only as `HOST:9222` with no bind-address
option, so the `cdp` URL in `bctl status` is still reachable by any process of
the same user on host loopback without a capability.

## Test

```sh
cargo test --workspace                                   # no VMs
cargo test -p bctl-runtime-smolvm --test lifecycle -- --ignored
cargo test -p bctl --test daemon -- --ignored
cargo test -p bctl --test profiles -- --ignored
cargo test -p bctl --test cdp -- --ignored
cargo run -p bctl-proof -- m1 --runs 10
pnpm --dir web exec playwright test                      # needs cargo build -p bctl
```

The ignored tests and the proofs boot real VMs from `.work/runtime` and
`.work/images`.
