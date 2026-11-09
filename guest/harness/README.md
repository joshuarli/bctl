# guest harness

One arm64 SmolVM proof for the vendored laputa stack: `guest/tinyland` as
the compositor and `guest/seatd` (built from this tree) as the broker.

## Run

Needs Docker with `linux/arm64` builds, Python 3, and the patched SmolVM
runtime. The script uses the main checkout's
`.work/runtime/smolvm/smolvm`; set `SMOLVM=/path/to/smolvm` to override.

```sh
sh guest/harness/smolvm-proof.sh
```

The script builds `image/Dockerfile`, saves the archive under
`.work/harness/`, creates a machine with `--gpu`, starts it with
`SMOLVM_DISPLAY=1280x800` and `SMOLVM_VNC=127.0.0.1:<port>`, and always
stops and deletes the machine. `SMOLVM_DISPLAY_SIZE`, `SMOLVM_VNC_PORT`,
and `TINYLAND_SKIP_IMAGE_BUILD=1` adjust a run.

## What it proves

- `image/tinyland-session` starts the vendored `/usr/local/bin/seatd` as
  root with `SEATD_VTBOUND=0` and socket `/run/seatd.sock`, then runs
  tinyland and foot as the unprivileged `tiny` user. The image carries no
  Alpine `seatd` or `libseat` package.
- `rfb.py` reads the host VNC framebuffer and requires non-black foot
  pixels (at least 0.5%).
- Typing `laputa` plus Return as VNC key events changes at least 200
  pixels and raises the magenta fraction, which `guest/test-app` prints
  only for the exact handshake word.
- `tinyland` runs with uid 1000 and cannot open `/dev/dri/card0` directly
  while it holds the card through seatd.
- The compositor log carries `TINYLAND_SEAT_OPEN`,
  `TINYLAND_DRM_ORIGIN seat:/dev/dri/card0`, `TINYLAND_DRM_OK`,
  `TINYLAND_COMPOSITOR_READY`, `TINYLAND_SURFACE_MAPPED`, and
  `TINYLAND_PRESENTED` with no session failure.

Frames (`vnc-before.ppm`, `vnc-after.ppm`), the three guest logs, and test
logs land under `.work/harness/`.

## Build notes

`guest/` is one Cargo workspace (`seatd`, `seatd-protocol`, `tinyland`).
`guest/.cargo/config.toml` selects `mold` for the musl targets; the
aarch64 musl tinyland build uses the target's default `crt-static`, so the
binary has no program interpreter and no `DT_NEEDED` (including no
libseat). `seatd` is built for the same target and copied into the image
from this workspace.

No component links libudev: tinyland reads evdev directly, and Alpine's
foot and the vendored seatd do not use it, so libudev-zero is not
installed.

## arm64 cargo tests

The `tests` stage carries the pinned `nightly-2026-09-15` toolchain and
prefetched locked workspace dependencies.

```sh
docker build --platform linux/arm64 --target tests \
  -f guest/harness/image/Dockerfile -t bctl/tinyland-smolvm-tests:local guest
docker run --rm --platform linux/arm64 --network none -w /src/seatd \
  bctl/tinyland-smolvm-tests:local cargo test --locked
docker run --rm --platform linux/arm64 --network none -w /src/tinyland \
  bctl/tinyland-smolvm-tests:local cargo test --locked
docker run --rm --platform linux/arm64 --network none -w /src \
  bctl/tinyland-smolvm-tests:local cargo test -p seatd-protocol --locked
```
