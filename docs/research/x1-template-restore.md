# X1: restoring a pre-browser appliance template

## Question

Can a credential-free appliance, checkpointed before Chromium starts
(`smolvm machine checkpoint`/`branch`), cut cold-ready time by more than H1's
spread without weakening the sandbox? Restoring a running browser is out of
scope, and existing CDP connections are never promised to survive a restore.

## Answer

Yes, by a clean margin, in the one configuration that restores today: the
appliance is parked at its first `smolvm-branch-ready` forkpoint (before
`bctl-guest run`, so before seatd, tinyland and Chromium), captured with
`smolvm machine checkpoint --wait-ready`, and each session is created with
`machine create --from` and started normally.

Ten interleaved checkpoint launches and ten cold launches of the same Helium
appliance measured 1,137 ms versus 1,445 ms at the median (**−309 ms, −21.3 %**).
Every one of the ten checkpoint runs was faster than every one of the ten cold
runs (checkpoint 1,114–1,170 ms, cold 1,429–1,472 ms). This region is narrower
than H1's: it starts at `SmolVm::launch` and stops at CDP answering plus a first
non-black VNC frame, so it excludes daemon and CLI work and is not directly
comparable to H1's `bctl up` cold range. The cold/checkpoint comparison is
within-run and interleaved. The renderer sandbox is intact: the restored
session's `bctl-guest check` reports `sandbox ok` (renderer `NoNewPrivs: 1`,
`Seccomp: 2`) and `input ok`.

The measured template has no guest network and no host profile mount. Both are
restore blockers, not choices; see **Blockers**. A networked appliance fails to
restore (the restored guest agent never answers), so the template is captured
network-free. The cold comparison therefore includes the runtime's normal
`--block-private` launch while the template path does not; the network setup is
a small part of the time and the template path's advantage survives it, but the
comparison is not network-for-network.

## How the template is taken

1. The machine boots the appliance image with its workload entrypoint replaced
   by `/usr/local/bin/smolvm-branch-ready -- /usr/local/bin/bctl-guest run`
   (`crates/runtime-smolvm/src/lib.rs:187`). SmolVM injects that helper into the
   container of a `--forkable` machine and installs it as the container's
   entrypoint; it writes the ready marker and blocks
   (`third_party/smolvm/crates/smolvm-agent/src/forkpoint.rs:153-211`).
   No Chromium, seatd or tinyland exists yet: `bctl-guest run` has not exec'd.
2. `smolvm machine checkpoint --wait-ready` waits for that marker
   (`third_party/smolvm/src/cli/pack.rs:182-187`) and captures the parked guest
   to one portable file.
3. The source machine is deleted. Each session runs
   `machine create --from <artifact>`; on start, `finalize_live_restore`
   writes the release marker, the helper continues into `bctl-guest run`, and
   the browser starts (`third_party/smolvm/src/portable_checkpoint.rs:6329-6344`,
   `third_party/smolvm/src/agent/fork.rs:2308`).

The capture and restore must both run with a display **and VNC**, at the same
geometry. A capture without VNC and a restore with VNC leaves the restored
guest agent unable to answer (see Blockers). Capturing with VNC and restoring
with VNC works; the VNC port itself is host-side and may differ.

## Implemented path

- `crates/runtime-smolvm/src/lib.rs`: `LaunchSource::{Image,Checkpoint}`
  (`:80`), `TemplateSpec` and `SmolVm::create_template` (`:175`, `:342`) which
  boots, waits for the forkpoint, captures and deletes the source; a checkpoint
  launch creates with `--from` and only the session's host CDP socket (`:284`).
- `runtime/patches/smolvm-0002-pause-2d-sessions.patch` (rebased): the portable
  checkpoint now records the GPU device and backend, its VRAM window, and the
  guest paths of the published sockets, and restores them
  (`crates/smolvm-pack/src/format.rs:733-758`,
  `src/portable_checkpoint.rs:3689-3757`, `src/portable_checkpoint.rs:4030-4055`,
  `src/machine_spec.rs:1100-1145`, `:1347-1352`). A 2D virtio-gpu is admitted
  to a portable capture (`src/portable_checkpoint.rs:3724-3728`); a virgl/Venus
  GPU is still refused. `machine checkpoint --wait-ready` is the new boundary
  wait (`src/cli/pack.rs:127-143`).
- `libkrun-0002-gpu-2d-snapshot.patch` already carried the 2D device state:
  `snapshot_2d` / `restore_2d_state` (`src/devices/src/virtio/gpu/virtio_gpu.rs:719`,
  `:772`), including scanout configuration and the first presented frame
  (`:834`, `:856`), and virtio-input's device state.
- `crates/bench/src/restore.rs`: the `restore` suite, timing a cold `SmolVm`
  launch against a checkpoint launch. `crates/runtime-smolvm/tests/restore.rs`:
  the acceptance test.

## Measurement

Command:

```sh
PERF_GATE_MAX_LOAD=30 target/release/bctl-bench restore --rounds 10 --quiet-wait 60 --out .work/tmp/x1-bench
```

Result: `.work/tmp/x1-bench/20261109T225145Z-restore.json` (exit 0). Every sample
is clean under the harness gate; the one-minute load recorded with each timing is
beside the sample. The order alternates every round (cold→checkpoint, then
checkpoint→cold), each sample uses a fresh machine and CDP socket on the same
runtime, and the template is captured once (2,314 ms; artifact 287,899,727
bytes). One untimed warm-up of each path ran first.

| Path | n | median | q1–q3 | min–max | all samples (ms, 1-min load) |
|---|---|---|---|---|---|
| Cold `SmolVm` launch | 10 | 1,445 | 1,437–1,458 | 1,429–1,472 | 1445 (13.66), 1429 (14.17), 1441 (14.17), 1463 (14.32), 1459 (14.37), 1472 (14.37), 1458 (14.18), 1429 (14.09), 1436 (14.09), 1445 (14.32) |
| Checkpoint `SmolVm` launch | 10 | 1,137 | 1,126–1,150 | 1,114–1,170 | 1150 (13.66), 1115 (13.66), 1114 (14.17), 1159 (14.32), 1149 (14.37), 1124 (14.37), 1134 (14.18), 1137 (14.18), 1137 (14.09), 1170 (14.32) |

Every checkpoint sample is faster than every cold sample, and the medians do not
overlap. The host was shared with another lane's VM; the one-minute load stayed
at 13.5–14.4, well under the `PERF_GATE_MAX_LOAD=30` admission, and the
interleaving is what makes the comparison usable.

The measured region is a `SmolVm` launch, not `bctl up`. It starts at
`SmolVm::launch` and stops when `/json/version` answers on the exposed CDP
socket and the first non-black VNC frame has arrived. The cold path boots the
Helium image (including the runtime's `--block-private` network setup); the
checkpoint path runs `machine create --from` (extraction of the artifact) and
the restored guest's resume from the parked state. Both paths include browser
start and the first frame. Teardown, daemon work and CLI work are excluded. The
checkpoint was captured before Chromium existed, so the time saved is the
removed guest boot up to the forkpoint (kernel, agent, container start,
packed-layer setup), not browser state.

## Sandbox proof

`cargo test -p bctl-runtime-smolvm --test restore -- --ignored` passed twice
(exit 0, 1 test each). The test captures a template, launches a
`LaunchSource::Checkpoint` with `SmolVm::launch`, asserts ready, navigates the
page to a local `data:` URL over the CDP socket, drives an RFB pointer click and
`type_text("x1")`, reads the field back with `Runtime.evaluate` over the same
socket (B1-style input reached the page), checks the VNC frame is 1280x800 and
not black, and runs `bctl-guest check` through `smolvm machine exec`, asserting
`sandbox ok`, `browser ok` and `input ok`.

The check's `sandbox` line is true only when the first renderer's
`/proc/<pid>/status` has `NoNewPrivs: 1` and `Seccomp: 2`
(`guest/appliance/src/check.rs:47-64`). A manual restore of the same template
configuration printed:

```
seat ok
compositor ok
browser ok
sandbox ok
input ok
```

No launch path adds `--no-sandbox`; the template changes nothing in the image
or in Chromium's sandbox flags.

## Blockers and limits

- **A networked appliance does not restore.** A portable restore of a machine
  that has both the packed-layers virtiofs and virtio-net leaves the restored
  guest agent unresponsive: `machine start` times out with
  `clone agent did not respond to ping within timeout`
  (`socket_exists=true connect_successes=26`). The guest is running: libkrun's
  debug log shows vsock TX traffic and repeated accepted guest connections to
  the agent port, each closed after the host's ping times out. The restore
  matches and re-activates saved devices by type
  (`libkrun/src/vmm/src/device_manager/hvf/mmio.rs:334`), and the vsock reset
  handshake is at `libkrun/src/devices/src/virtio/vsock/muxer.rs:406` and
  `event_handler.rs:89-107`; the root cause was not established. Measured
  isolation: image+net fails; image alone works; bare machine+net works;
  image+net without `--block-private` also fails. The template is therefore
  captured network-free.
  *What would remove it:* a restore that re-establishes the agent channel with
  virtio-net and the packed-layers virtiofs present — verifying the same-type
  device match order and the transport-reset acknowledgement for the restored
  guest, then re-running the bench network-for-network.
- **Capture and restore must agree on the display/VNC surfaces.** Capturing
  with `SMOLVM_DISPLAY` only and restoring with display+VNC also leaves the
  restored agent unanswered; capturing with both works. The 2D restore
  configures and presents the scanout in `virtio_gpu.rs:834`/`:856`, and the
  host VNC framebuffer is created at start (`src/agent/vnc.rs`). The template
  path captures with a free VNC port at the session geometry.
  *What would remove it:* make the restored display backend independent of
  whether a VNC server was attached at capture, or record the host display
  surfaces in the checkpoint.
- **Restored sessions cannot take a host profile mount.** The checkpoint
  restore refuses `-v` mounts (`src/machine_spec.rs:1091-1093`) and re-attaches
  only the host mounts recorded at capture. The template is captured without
  one, so the browser's `/profile` lives in the guest overlay and does not
  survive `down`. This is why the bench/test session cannot use a per-profile
  host directory.
  *What would remove it:* allow a restore to replace the host side of a
  captured mount (the way ports may move their host side), or add a
  virtiofs device whose host path is a restore parameter with the captured
  device topology preserved.
- **The template fixes the display geometry and the CPU/memory topology.**
  A session requesting a different viewport would start a different scanout
  than the captured one; the restore does not validate this today.
- **The artifact is host-bound.** A portable capture of a local image archive
  and of published sockets is admitted only with `--host-mounts`
  (`src/portable_checkpoint.rs:3694`, `:3709-3715`); the artifact names the
  archive by digest and is restored only where that archive still exists.
- **No bctl CLI exposes templates** until the network and profile blockers are
  removed.

## Unverified

- Long-lived stability of a restored session (the test ran twice; the bench
  restored the same artifact 11 times including the warm-up, each to CDP ready
  and a first frame).
- A second restore of a machine that was itself restored (the artifact is a
  one-shot input; each bench sample launches it again from scratch).
- `smolvm machine branch` (FORK) as an alternative template path. It bypasses
  the capture gate but was not measured here.
- Profile persistence, networking, and any display geometry other than
  1280x800, for restored sessions.
- Cross-host restore. The artifact is bound to this host's image archive and
  agent rootfs.

## Reproduction

```sh
sh runtime/build.sh                       # builds the patched SmolVM and libkrun
cargo test -p bctl-runtime-smolvm --test restore -- --ignored   # twice
cargo build --release -p bctl-bench
PERF_GATE_MAX_LOAD=30 target/release/bctl-bench restore --rounds 10 --quiet-wait 60 --out .work/tmp/x1-bench
```
