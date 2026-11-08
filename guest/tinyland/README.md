# tinyland

Minimal single-fullscreen Wayland kiosk that scans out real foot pixels
through DRM and takes keyboard input. Proven inside a QEMU/KVM Alpine
guest on Linux x86_64.

## Run

Needs Linux x86_64 with KVM and `qemu-system-x86_64`.

```sh
make smoke
```

Other proofs: `make drm-proof`, `make wayland-pixels`, `make drm-parity`,
`make negative`.

## Configure

```text
usage: tinyland [--drm-proof] [--wayland-pixels] [--smoke] [--drm-device PATH] [--width N] [--height N] [--socket NAME] [-- COMMAND...]
```

Defaults are `--drm-device /dev/dri/card0`, `1024x768`, and socket
`wayland-0`. Bad options exit 2. `QEMU` overrides the emulator binary and
`ALPINE_MIRROR` overrides the package mirror.

## Output

Each proof writes console, compositor, and QEMU logs plus a `.png`
screenshot under `.work/`. `smoke` passes when the guest reports
`GUEST_TEXT_OK` and the screenshot holds the magenta marker.

## Help

File issues at `laputa-systems/tinyland`. Developer detail starts at
`AGENTS.md`.

## License

No `LICENSE` file is committed.
