# Appliance image contract

An appliance image is an OCI image for `linux/arm64` that SmolVM runs as the
machine's workload. bctl starts it with a display and input attached and
reaches the browser only through CDP and the host-side VNC stream.

## Machine

bctl starts every appliance with `--gpu` and the environment
`SMOLVM_DISPLAY=<width>x<height>` and `SMOLVM_VNC=127.0.0.1:<port>`. The guest
then has:

- `/dev/dri/card0`, a KMS device with one `Virtual-1` connector whose
  preferred mode is the display size.
- Two evdev devices, "smolvm vnc keyboard" and "smolvm vnc pointer"
  (absolute), listed under `/sys/class/input`. The container gets no
  `/dev/input` nodes; the entrypoint creates them. A runtime without input
  support has neither device, and the image must still come up view-only.

The compositor must scan out from classic 2D (dumb) buffers or blob
resources. bctl's runtime accepts 2D resources; the stock SmolVM macOS
build does not.

## Entrypoint

`/usr/local/bin/bctl-guest` is the entrypoint.

- `bctl-guest run` (the default command) brings up, in order, the seat
  (`/run/seatd.sock`), the compositor (`$XDG_RUNTIME_DIR/wayland-1`), and the
  browser (CDP `/json/version` answering on the CDP port). It prints
  `bctl-guest: ready` once all three are up and exits with the browser.
- `bctl-guest check` prints one line per stage, `<stage> ok` or
  `<stage> missing`, for `seat`, `compositor`, `browser`, and `sandbox`, and
  exits non-zero if any stage is missing. `sandbox ok` means a renderer
  process runs with `NoNewPrivs: 1` and `Seccomp: 2`. A last line,
  `input ok` or `input absent`, reports whether both VNC input devices exist;
  `input absent` means a view-only runtime and does not fail the check.

## Browser

- Runs as the unprivileged user `browser` (uid 1000, group `video`), headed,
  on Wayland, with the Chromium sandbox enabled.
- CDP listens on guest port `9222` (`BCTL_CDP_PORT`) on all guest
  interfaces. bctl forwards it to a host loopback port; the guest network is
  reachable only through that forward. Chromium builds
  `webSocketDebuggerUrl` from the request's Host header, so clients see the
  host-side address.
- The profile lives at `/profile` (`BCTL_PROFILE_DIR`), owned by `browser`.
- The first page is `BCTL_START_URL`, default `about:blank`.

## Logs

Stage logs are in `/run/bctl/`: `seatd.log`, `compositor.log`,
`browser.log`, and `relay.log`.

## Images

| Image | Base | Browser | Compositor | Role |
|---|---|---|---|---|
| `ref-alpine-chromium` | Alpine 3.22 | Alpine `chromium` | Sway (pixman) | Test scaffolding until the Helium arm64 archive exists; never a default |

Build one with `images/build.sh <image>`; the archive lands in
`.work/images/<image>.tar`.
