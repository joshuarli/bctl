# tinyland

Minimal single-fullscreen Wayland kiosk that scans out real foot pixels
through DRM and takes keyboard input. Proven on SmolVM arm64, where a
Linux arm64 guest runs seatd, unprivileged tinyland, and foot.

## Verify

Needs Docker with arm64 image builds, Python 3, and the patched SmolVM
runtime. The harness defaults to the main checkout's
`.work/runtime/smolvm/smolvm`; set `SMOLVM=/path/to/smolvm` to override it.

```sh
sh ../harness/smolvm-proof.sh
```

The harness builds the Alpine arm64 image in `../harness/image`, boots it
with `--gpu`, starts seatd, then tinyland as the unprivileged `tiny` user,
then foot as the Wayland client. It asserts non-black foot pixels on the
host VNC framebuffer, a pixel change after typing `laputa` over VNC, the
seatd-mediated DRM path, and that `tiny` cannot open `/dev/dri/card0`
directly. It stops and deletes its machine on success and
failure. `SMOLVM_DISPLAY_SIZE`, `SMOLVM_VNC_PORT`, and
`TINYLAND_SKIP_IMAGE_BUILD=1` adjust a run.

Other checks: `make check`, `make test`, `make audit`.

## Configure

```text
usage: tinyland [--seat] [--drm-proof] [--wayland-pixels] [--smoke] [--drm-device PATH] [--width N] [--height N] [--socket NAME] [-- COMMAND...]
```

Defaults are `--drm-device /dev/dri/card0`, `1024x768`, and socket
`wayland-0`. Bad options exit 2. The connector's preferred mode controls
the actual scanout size; the requested width and height are a lower bound.

## Output

The harness writes `session.log`, `seatd.log`, and `compositor.log`, plus
raw `vnc-before.ppm` and `vnc-after.ppm` frames, under `.work/harness/`.
It prints `SMOLVM_TINYLAND_PROOF_OK` on success.

## Help

File issues at `laputa-systems/tinyland`. Developer detail starts at
`AGENTS.md`.

## License

No `LICENSE` file is committed.
