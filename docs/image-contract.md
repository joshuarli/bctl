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

`/usr/local/bin/bctl-guest` is the entrypoint, and `machine exec` finds it on
`PATH`. Two implementations exist: the Rust `guest/appliance` crate in
`ref-scratch-chromium`, and the shell script `images/bctl-guest` in the
Alpine images.

- `bctl-guest run` (the default command) brings up, in order, the seat
  (`/run/seatd.sock`), the compositor (`$XDG_RUNTIME_DIR/wayland-1`), and the
  browser (CDP `/json/version` answering on the CDP port). It prints
  `bctl-guest: ready` once all three are up.
- The Rust entrypoint stops on SIGTERM or SIGINT: it signals the browser,
  waits up to 15 s for it to exit, then stops the compositor and the seat,
  syncs, and exits 0. If the seat, the compositor, or the browser exits on
  its own, or a stage does not come up within 30 s, it stops the rest in the
  same order and exits non-zero. The shell entrypoint exits 0 when the
  browser exits.
- `bctl-guest check` prints one line per stage, `<stage> ok` or
  `<stage> missing`, for `seat`, `compositor`, `browser`, and `sandbox`, and
  exits non-zero if any stage is missing. `sandbox ok` means a renderer
  process runs with `NoNewPrivs: 1` and `Seccomp: 2`. A last line,
  `input ok` or `input absent`, reports whether both VNC input devices exist;
  `input absent` means a view-only runtime and does not fail the check.

## Browser

- Runs as the unprivileged user `browser` (uid 1000, group `video`), headed,
  on Wayland, with the Chromium sandbox enabled. A shell-free image carries
  its own `/etc/passwd` and `/etc/group` for that user and group
  (`guest/appliance/rootfs/etc`).
- In `ref-scratch-chromium` the browser is an archive in the Helium archive
  layout, installed unchanged at `/opt/browser`, with
  `/usr/local/bin/browser` linking to the executable that its
  `package-manifest.json` names. `bctl-guest browser` sets what the archive's
  `helium-wrapper` would (`LD_LIBRARY_PATH=<root>/lib`,
  `FONTCONFIG_FILE=<root>/fontconfig/fonts.conf`,
  `FONTCONFIG_PATH=<root>/fontconfig`) and runs the executable directly.
- CDP is relayed on the guest Unix socket `/run/bctl/cdp.sock`
  (`BCTL_CDP_SOCKET`); bctl exposes that socket to a host path in a
  per-session directory only the user can enter, so no host TCP port reaches
  CDP. Setting `BCTL_CDP_PORT` also relays it on that guest TCP port, for
  diagnostics that publish ports themselves. Chromium builds
  `webSocketDebuggerUrl` from the request's Host header.
- The profile lives at `/profile` (`BCTL_PROFILE_DIR`), owned by `browser`.
- The first page is `BCTL_START_URL`, default `about:blank`.

## Logs

Stage logs are in `/run/bctl/`: `seatd.log`, `compositor.log`,
`browser.log`, and `relay.log`.

With `BCTL_TRACE_MAPS=1` in the image environment, the Rust `bctl-guest run`
also samples `/proc/*/maps` every 0.3 s into `/run/bctl/maps.tsv`, and
`bctl-guest maps` prints it. This is the census trace tap for images without
a shell (`guest/audit/trace.sh`).

## What SmolVM needs inside the image

Nothing. libkrun's `krun-init` and the SmolVM agent run from the VM's own
root, outside the image. The agent runs the image as a container whose PID 1
is the agent binary, bind-mounted read-only at `/run/smolvm/init` when the
container starts, and it writes `/etc/resolv.conf` and `/etc/hosts` into the
container's writable layer at run time. `ref-scratch-chromium` boots and
passes B1 with no SmolVM file of its own.

## Images

| Image | Base | Browser | Compositor | Role |
|---|---|---|---|---|
| `ref-alpine-chromium` | Alpine 3.22 | Alpine `chromium` | Sway (pixman) | Test scaffolding until the Helium arm64 archive exists; never a default |
| `ref-alpine-tinyland` | Alpine 3.22 | Alpine `chromium` | `guest/tinyland` with `guest/seatd` | Stand-in for `helium-tinyland` until Helium exists; never a default |
| `ref-scratch-chromium` | scratch: no base, busybox, or shell | Alpine `chromium` and its runtime closure, packed in the Helium archive layout | `guest/tinyland` with `guest/seatd`, supervised by the Rust `bctl-guest` | Stand-in for the shell-free product image until Helium exists; never a default |

Build one with `images/build.sh <image>`; the archive lands in
`.work/images/<image>.tar`.

`ref-scratch-chromium` takes its browser from one input, the Dockerfile
stage `browser-archive`. By default that stage packs Alpine's `chromium` APK
with `standin-archive.sh`: the browser directory, `lib/` with the DT_NEEDED
closure of its ELF files and of the libraries a traced B1 run showed it
`dlopen`ing, DejaVu fonts, and fontconfig. To build the image around a
Helium archive instead, override the stage with a directory holding exactly
one `*.tar.xz`:
`docker build --build-context browser-archive=DIR --build-context guest=guest images/ref-scratch-chromium`.
`stage-browser.sh` installs whichever archive arrives.
