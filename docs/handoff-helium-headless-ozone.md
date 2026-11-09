# Handoff: headless Ozone in the Helium Linux build

For whoever maintains `~/d/chromium-build`. Hydrogen needs a Helium
0.20.3.1 x86-64 musl archive that can run headless with its normal Linux
sandbox enabled, so the browser can be qualified inside a Nanos guest that
has no display server.

## Problem

The accepted archive `helium-0.20.3.1-linux-x86_64-musl.tar.xz`
(sha256 `6853af22b60345c92e0cbe69181aebd8af3fa79842c7f0135d20c9656c5918c6`)
compiles in only the Wayland Ozone backend. Every headless entry point
aborts:

```sh
helium --headless=new --user-data-dir="$PWD/profile" about:blank
helium --ozone-platform=headless --user-data-dir="$PWD/profile" about:blank
# both: Invalid ozone platform: headless
```

The cause is `config/profiles/helium-wayland-release.gn:41`
(`ozone_platform_headless = false`), which `scripts/verify-gn-args.py:98`
enforces.

## Change

1. In `config/profiles/helium-wayland-release.gn` set
   `ozone_platform_headless = true`. Keep `ozone_platform = "wayland"` as the
   default and `ozone_platform_wayland = true`, so the headed Wayland
   behavior and its acceptance stay the same.
2. Change the expected value in `scripts/verify-gn-args.py` to match.
3. Rebuild, stage and package as usual, with a new `platform_version` or
   archive name so the new archive cannot be confused with the accepted one.

Do not use `--no-sandbox`, `--single-process` or `--no-zygote` anywhere in
qualification.

## Acceptance

All of these must hold on Linux x86-64, as a non-root user with unprivileged
user namespaces available, and with no Wayland or X server running:

1. The existing Wayland acceptance (`docs/helium-acceptance.md`) still passes
   unchanged.
2. `helium --headless=new --remote-debugging-port=0 --user-data-dir=<dir> about:blank`
   stays up, and the `DevTools listening on ws://…` line appears on stderr.
3. `GET /json/version` on that port answers with the Helium product string.
4. Navigating to a local HTTP page works, and `Runtime.evaluate` returns a
   value the page computed.
5. While it runs, a renderer process (`--type=renderer`) shows
   `NoNewPrivs: 1` and `Seccomp: 2` in `/proc/<pid>/status`, and lives in its
   own PID namespace (`NSpid` has more than one field).
6. `Browser.close` exits the browser with status 0 and leaves no processes
   behind.
7. Report the archive size change against the accepted archive.

## Deliverable

- The archive under `~/d/chromium-build/.work/artifacts/helium-<version>/`,
  with its `.sha256` and an acceptance JSON that adds the headless checks
  above.
- The GN diff and the commit SHA that produced it.

Hydrogen then pins the new path and sha256 in `plan.md` (slice G5.1) and
drops its Wayland stopgap.
