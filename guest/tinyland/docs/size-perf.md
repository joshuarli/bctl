# Size / performance report (Phases 0-6 landed)

Measured 2026-10-30/31, Linux x86_64. `qemu-tinyland-ghostty` is the only
graphical profile. Numbers cite the commands that produced them.

## Tinyland: libxkbcommon -> KBVM

| Item | Before (xkbcommon) | After (KBVM) | Command |
|---|---|---|---|
| PM binary bytes | 2117480 | 3015600 (+42%) | `stat -c %s` on payload extracts of `fd64294d…` vs `3a028f58…` |
| PM payload tar.gz | 877.6K | 1.2M | `ls -lh .out/artifacts/x86_64/v2/sha256/<key>/payload.tar.gz` |
| PM payload SHA-256 | `8f7532ec…fcb393` | `29f0fd04…155c44` | `artifact.json` `payload_sha256` |
| PM artifact key | `fd64294d…766` | `3a028f58…74d` | `artifact.json` `key` |
| DT_NEEDED | `libseat.so.1`, `libxkbcommon.so.0`, `libgcc_s.so`, `libc.so` (+RUNPATH `/usr/lib`) | `libseat.so.1`, `libc.so`, `libgcc_s.so` (+RUNPATH `/usr/lib`) | `readelf -d` on each payload binary |
| Cargo locked crates | 80 (`xkbcommon 0.9.0`, no kbvm) | 95 (`kbvm 0.2.0`, no xkbcommon in default resolve) | `grep -c 'name = ' tinyland/Cargo.lock`; `cargo tree --locked \| grep -E 'xkbcommon|kbvm'` |
| Harness release binary | — (not re-measured pre-change) | 4886088 bytes, NEEDED `libseat.so.1`, `libc.musl-x86_64.so.1`, `libgcc_s.so.1` (no libxkbcommon) | `target/x86_64-unknown-linux-musl/release/tinyland` after `harness/build-guest.sh` |
| Unit tests | 42 passed (claimed) | 41 passed, 1 pre-existing seat segfault (`open_without_daemon_is_a_hard_error` segfaults in host `libseat_open_seat`; `seat.rs` untouched by this change) | test binary `--skip seat::tests::open_without_daemon_is_a_hard_error` → 41 ok; `input::` 5/5 ok; `--features oracle` differential 1/1 ok |
| Guest proofs (KBVM) | — | `smoke`, `wayland-pixels`, `drm-proof`, `drm-parity`, `negative`, `audit` all OK | `bash harness/{smoke,wayland-pixels,drm-proof,drm-parity,negative-runtime}.sh`, `bash harness/audit.sh` (see Phase 1) |

Regression assessment: the +900KB binary / +15 crates is the cost of
replacing the shared C `libxkbcommon` with statically linked pure-Rust
KBVM (plus `hashbrown`/`indexmap`/etc). The C library leaves the image
(the closure drops `libxkbcommon`); the Rust code enters it. No
`unsafe` added (`src/` still only `seat.rs` libseat FFI per audit).
Accepted as an investigated decision for the C-free XKB boundary, not
a silent regression. Cold-start-to-first-frame and idle RSS were not
re-measured (no profiling harness in this slice); the guest proofs
confirm presentation still meets the existing screenshot gates.

## Ghostty Custom (new package)

- Binary: 29.5M ELF (`ghostty-custom-x86_64-linux-musl.tar.gz` 18.3M,
  SHA-256 `9b56d0b8…`; aarch64 17.9M `110f7f08…`, runtime not claimed).
  Package `ghostty-custom-1.4.1-2`, payload 18,516,608 bytes.
- DT_NEEDED: `libEGL.so.1`, `libwayland-client.so.0`,
  `libxkbcommon.so.0`, `libc.so`. No GTK/Adwaita/D-Bus/X11/glibc.
- PM artifact `8a48057a…` (payload 17.8M), proof green x86_64
  (`--version` shows Ghostty 1.4.1-dev + wayland runtime + OpenGL
  renderer; `+show-config` exits 0; `xterm-ghostty` terminfo present,
  magic `0o432`, names `xterm-ghostty|…`).
- aarch64: recipe pins both hashes and the proof early-returns
  `cross-built` when arches differ; native aarch64 seed/guest remains
  Needs environment (no aarch64 host here).

## Profile closures / images

- `qemu-tinyland-ghostty`: 38 plan nodes; disk image 308M, rootfs 276M.
- Cold start: session ready with a 4.3 context at 13.82 s guest
  uptime (proof start 11.37 s; a prior run measured 10.51 s). Idle RSS
  after input: tinyland 7,172 KB, Ghostty 41,504 KB. Renderer: virgl,
  OpenGL 4.3 core. Host screenshot is 1280x800 with 0.61% magenta.

## Mesa: desktop GL enabled (EGL-only, no LLVM)

- Options `opengl=false` → `true` (`packages/mesa/PKGBUILD.xsh` rel 1→2);
  `llvm=disabled`, `glx=disabled` unchanged. No `libGL.so` (no GLX
  without X11); desktop contexts come from EGL, which is all Ghostty links.
- Generated sources regenerated for the new option set (70 → 81 outputs,
  archive `48e216f3…` → `9938c1ac…`); the old set lacked the 11 desktop GL
  tables. Procedure and tool versions in
  `packages/mesa/files/generated/README.md`.
- Payload 4.28M → 5.14M (+0.86MB). `DT_NEEDED` of every shipped ELF
  re-verified: no `libLLVM`, no new runtime deps
  (libm/libdrm/libwayland-client/libgbm/libgallium only).
- Function: GLES 3.1 + render unchanged; new desktop probe reports
  softpipe 3.3 compat ceiling with green FBO readback, machine-checked
  by the proof (see `tinyland/docs/opengl-probe.md` for the numbers and
  the virgl/llvmpipe decision).

## Rendering path (resolved)

Ghostty needs desktop OpenGL 4.3 core; softpipe maxes at 3.3 compat
(measured), and virgl on the host GPU gives the guest 4.3 core (guest
measured). QEMU cannot screendump a GL scanout, so the profile scans out
on a 2D `virtio-gpu-pci` and renders on a `virtio-gpu-gl-pci`; the host
pixel proof and the 4.3 context both hold. The llvmpipe +LLVM path is
costed in `tinyland/docs/opengl-probe.md` and was not needed.
