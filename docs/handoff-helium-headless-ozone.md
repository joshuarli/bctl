# Handoff: finish the headless Helium archive

For whoever maintains `~/d/chromium-build`. Hydrogen needs a Helium
0.20.3.1 x86-64 musl archive that runs headless with its normal Linux
sandbox, so the browser can be qualified inside a Nanos guest with no
display server.

## State

- The GN change is done: commit `1761251` sets `ozone_platform_headless = true`
  in `config/profiles/helium-wayland-release.gn`, and `docs/helium-acceptance.md`
  already lists the headless checks (`headless-devtools`, `headless-identity`,
  `headless-page`, `headless-sandbox-renderer`, `headless-clean-exit`).
- The build
  `./chromium-build --product helium --profile helium-wayland-release build --jobs 16`
  compiled everything and then failed on the final step, `[1/1] LINK ./helium`,
  with `LLVM ERROR: out of memory` (`.work/tmp/build6.log`, 2026-11-09 15:49 UTC).
- The failure was the per-process memory-map limit, not RAM. `lld` had about
  8 GB resident but about 65,460 mappings against the default
  `vm.max_map_count` of 65,530 (`.work/tmp/maps.log`).
- The host limit is now `vm.max_map_count = 1048576`, set live and persisted
  in `/etc/sysctl.d/90-max-map-count.conf`. Docker containers inherit it.

## Do

1. Confirm `sysctl vm.max_map_count` prints 1048576.
2. Resume the same build. The work volume `helium-0.20.3.1-headless-work-amd64`
   keeps the compiled objects, so only the link should rerun.
3. If the link fails again, root-cause it before retrying. Watch the map count
   and RSS the same way `.work/tmp/maps.sh` does.
4. Stage, package and qualify as usual. Give the archive a name or
   `platform_version` that cannot be confused with the accepted non-headless
   archive (sha256 `6853af22b60345c92e0cbe69181aebd8af3fa79842c7f0135d20c9656c5918c6`).

Do not use `--no-sandbox`, `--single-process` or `--no-zygote` anywhere in
qualification.

## Acceptance

The existing acceptance in `docs/helium-acceptance.md` passes, both the
Wayland checks and all five headless checks. That acceptance runs as a
non-root user with unprivileged user namespaces and no display server.

## Deliverable

- The archive, its `.sha256` and the acceptance JSON under
  `~/d/chromium-build/.work/artifacts/helium-<version>/`.
- The archive size change against the accepted archive.
- The chromium-build commit SHA that produced it.

Hydrogen then pins the new path and sha256, switches `dev/helium-rootfs.sh`
and `tests/helium/guest_init.c` to `--headless=new`, and drops the Wayland
stopgap. That change is prepared in hydrogen and waits only on this archive.
