# Runtime

bctl uses Hydrogen, its Nanos fork in `kernel/`, by default for headless sessions on Apple Silicon
and Linux x86_64. Set `BCTL_RUNTIME=smolvm` for differential testing or headed
sessions with SmolVM. Runtime selection and default image names live in
`crates/bctl/src/runtime.rs`.

SmolVM is vendored under `third_party/smolvm` at the revision recorded in
`third_party/UPSTREAM.lock`. `runtime/build.sh` builds it into
`.work/runtime/smolvm`. `runtime/build-nanos.sh` and `runtime/build-nanos-arm64.sh` build the kernel
from the committed `kernel/` tree.

## Build and install

Build the dashboard before the CLI: `crates/bctl/build.rs` embeds `web/dist`
and refuses a missing build. Use the Rust toolchain in `rust-toolchain.toml`
and the Node and pnpm versions in `web/package.json`.

```sh
pnpm --dir web install --frozen-lockfile
pnpm --dir web build
cargo build -p bctl --locked
```

The CLI is `target/debug/bctl`. Runtime and image installation is separate.

### SmolVM on Apple Silicon

Install Xcode Command Line Tools, Docker, `jaq`, the pinned Rust toolchain
and `stable`, and Homebrew's `libkrun/krun/virglrenderer-krun`, `libepoxy`,
and `molten-vk`. Add the `aarch64-unknown-linux-musl` target to the pinned
toolchain. The guest builder uses the pinned Alpine container described below.

```sh
runtime/build.sh
BCTL_BROWSER_ARCHIVE=DIR images/build.sh helium
bctl_state="${BCTL_HOME:-$HOME/Library/Application Support/bctl}"
mkdir -p "$bctl_state/runtime" "$bctl_state/images"
ln -sfn "$PWD/.work/runtime/smolvm" "$bctl_state/runtime/smolvm"
ln -sf "$PWD/.work/images/helium.tar" "$bctl_state/images/helium.tar"
BCTL_RUNTIME=smolvm target/debug/bctl doctor
```

`DIR` contains one accepted arm64 Helium archive and its sidecars, as defined
by the [image contract](image-contract.md). Reference images under `images/`
remain available for diagnostics through `bctl up NAME --image IMAGE`.

### SmolVM on Linux x86_64

Headed sessions also run under KVM. Install Docker, `jaq`, `patchelf`, a C
compiler named `cc`, the pinned Rust toolchain and `stable`, and Alpine's
`virglrenderer` and `libepoxy` runtime libraries; the display is libkrun's 2D
scanout, so no host GPU or Vulkan driver is used. `runtime/build.sh` fetches
libclang from the pinned llvm-prebuilt-musl release.

```sh
runtime/build.sh
BCTL_BROWSER_ARCHIVE=DIR images/build.sh helium
bctl_state="${BCTL_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/bctl}"
mkdir -p "$bctl_state/runtime" "$bctl_state/images"
ln -sfn "$PWD/.work/runtime/smolvm" "$bctl_state/runtime/smolvm"
ln -sf "$PWD/.work/images/helium.tar" "$bctl_state/images/helium.tar"
BCTL_RUNTIME=smolvm target/debug/bctl doctor
```

`DIR` contains one accepted x86_64 Helium archive, r5 or later, and its
sidecars. Shared directories such as `/profile` use virtual ownership on
Linux, as on macOS: host files belong to the invoking user and the guest's
owner and mode live in the `user.containers.override_stat` xattr, so the
profile directory needs a filesystem with user xattrs.

### Nanos

Follow [Nanos runtime layout](#nanos-runtime-layout) for Linux builds and
installation, or its Apple Silicon instructions for the libkrun backend.

## Shared graphics source

Tinyland and its seatd daemon come from the commit in `guest/tinyland.rev` in
[the Tinyland upstream](https://github.com/laputa-systems/tinyland).
`guest/harness/tinyland-context.sh` supplies the named Git build context to
all image builders. `guest/` contains only the bctl appliance crate.

Land shared fixes, adjustments, and features upstream first. Update the pin
and verify the consuming image afterward. Do not restore local source copies
or consumer-only patches. bctl owns its appliance, image assembly, and VM proof.

## What is vendored

`third_party/smolvm` holds three source trees with no nested Git repository:

| Tree | Upstream | Pinned |
|---|---|---|
| `third_party/smolvm/` | `smol-machines/smolvm` | the commit in `UPSTREAM.lock` `[smolvm]` |
| `third_party/smolvm/libkrun/` | `smol-machines/libkrun` | its gitlink at that commit |
| `third_party/smolvm/libkrunfw/` | `smol-machines/libkrunfw` | its gitlink at that commit |

`UPSTREAM.lock` records, for one build: the SmolVM commit and branch, the
libkrun and libkrunfw revisions, the Linux kernel version, its stable Git
revision and archive SHA-256, the ordered patch list with each patch's tree and
digest, and the two Rust toolchains. The importer checks the whole-tree source
identity before replacing an existing import. The host build checks patch
digests and applicability, but does not check that whole-tree identity.

Each source tree keeps its own license: `smolvm/LICENSE` and
`smolvm/THIRD-PARTY-NOTICES.md`, `libkrun/LICENSE` and
`libkrun/src/rutabaga_gfx/LICENSE`, and libkrunfw's GPL-2.0-only and
LGPL-2.1-only notices. The kernel archive carries Linux `COPYING` and
`LICENSES`; it is not vendored, only its identity is locked.

Upstream paths that do not define this build are not imported: demos, examples,
documentation, packaging, CI configuration, the Git LFS library blobs, the
upstream Alpine rootfs script's prebuilt binaries, and the Fedora/`krunvm`
helpers. `smolvm/AGENTS.md` stays because the CLI embeds it in `--help`.

## Local patches

`UPSTREAM.lock` owns the ordered patch list, target trees, and digests.
`runtime/patches/` contains the inputs. The importer applies them to fresh
source; the host build accepts a patch only when it applies forward or is
already applied. Preserve these contracts when refreshing the patches:

- **Graphics:** macOS's Venus-only virglrenderer refuses CPU-rendered 2D
  resources. A separate rutabaga 2D component owns those resources and every
  subsequent operation on them. Tinyland's dumb buffers use this path.
- **Checkpoints:** `KRUN_GPU_BACKEND=2d` has no 3D capsets. Checkpoints retain
  resource backing ranges, scanout state, and virtio-input queues. Pixels
  are restored from guest RAM instead of duplicating framebuffers in the
  16 MiB device-state stream. The restored frame is presented before the
  guest runs. Machine records retain launch arguments and published socket
  paths; restored listeners replace old connections. Host-backed archives
  are checked against their recorded content digest. Virgl/Venus GPUs,
  `local-dir:` layers, and Docker-socket checkpoints remain refused.
- **Keyboard input:** noVNC can send a shifted keysym without a Shift event.
  The adapter supplies Shift around those symbols while preserving clients
  that send real modifier events.
- **ARM firmware:** the HVF GIC level interrupt is lowered after the last
  virtio-MMIO cause is acknowledged, and 16-bit firmware writes are handled.
  ACPI describes the configured CPUs and MMIO devices through fw_cfg.
  Firmware-backed checkpoints remain unavailable until fw_cfg state is saved.
- **Linux shared directories:** user virtiofs mounts use libkrun's
  override-stat ownership on Linux, as macOS's server always does. Without
  it the unprivileged Linux server refuses any guest write whose gid is not
  the host user's, and the browser runs as gid `video`.
- **Guest network:** the agent turns off IPv6 duplicate address detection on
  the guest NIC. Otherwise the kernel's link-local address leaves its
  tentative state about a second after boot, and the address event makes
  Chromium fail in-flight requests with `ERR_NETWORK_CHANGED`.
- **Balloon queues:** optional queues follow the negotiated features, as in
  Linux's virtio-MMIO transport. Without free-page hinting, Hydrogen's
  reporting queue follows its stats queue at index 3. The device must route
  that queue to reporting, including after snapshot resume, rather than
  acknowledging it as an inactive hinting round. Existing snapshots retain
  their negotiated features and physical queue indices.
- **Cryptography:** graviola is the sole provider. TLS users select providerless
  features, rcgen signs through a graviola `RemoteKeyPair`, and certificates
  explicitly supply serial numbers and subject key identifiers. The s3fs
  client uses reqwest in place of ureq's provider-selecting TLS feature;
  SHA-256 call sites use `sha2`.

The vsock connection-lifetime and receive-header patches are also recorded
in the lock. Their exact scope and ordering remain in the patch inputs.
When the host and guest have both shut down their write halves, the Unix
proxy closes after draining its guest output. Either shutdown order releases
the descriptor. A single half-close still permits traffic in the other direction.

## What `runtime/build.sh` builds

All from the vendored source, on the host:

1. **libkrunfw.** The Linux kernel bundle `kernel.c`, built from the pinned
   kernel archive and libkrunfw's aarch64 configuration and patches, becomes
   `libkrunfw.5.dylib`.
2. **libkrun.** `libkrun.dylib`, with the features in `UPSTREAM.lock`
   (`blk,net,gpu,input`), bctl's patch, and `krun-init` embedded. The GPU
   feature is what exports `krun_add_display`; the input feature is what
   exports `krun_add_input_device`.
3. **smolvm.** The CLI, linked against the just-built `libkrun.dylib`.
4. **The guest agent rootfs and `krun-init`**, plus the pre-formatted disk
   templates, built in a pinned Alpine container (see below).

The kernel and the guest binaries are built in a container because this host is
macOS: the kernel needs a Linux toolchain, and the guest's C tools need a Linux
musl compiler and headers.

`krun-init`, `smolvm-agent`, and every program in the rootfs link statically
against the musl `libc.a` and startup objects from the `llvm-prebuilt-musl`
release that the Helium image also uses. Rust std is rebuilt with
`-Zbuild-std` on the nightly named in `UPSTREAM.lock`, and clang is the C compiler
and linker, so no GCC object, `.comment`, or runtime enters a guest binary.
`runtime/guest/check-purity.sh` fails the build on any `INTERP`, `DT_NEEDED`,
GCC `.comment`, or GNU runtime symbol in the rootfs or `krun-init`.

The container image is pinned by digest, and every input it fetches is pinned
by version and SHA-256: the LLVM/musl toolchain, musl's headers, BusyBox, GNU
tar, e2fsprogs, util-linux, crun, json-c, libcap, libseccomp, argp-standalone,
curl, Go, Crane, and the CA bundle; Rust comes from rustup. The rootfs is
assembled by `runtime/guest/rootfs.sh`, not by upstream's Alpine script: the
vendored script can only produce an Alpine userland, and every Alpine binary is
GCC-built. The assembly keeps the layout SmolVM needs: `/usr/local/bin/smolvm-agent`,
`/usr/local/bin/crane`, `/sbin/init`, `/usr/bin/crun`, `/usr/bin/seatd`,
e2fsprogs in `/sbin`, and the `/etc` files the agent reads. It includes only
what the agent execs and reads: BusyBox for `sh` and the coreutils, mount,
umount, cp, sleep, true, nc and wget applets; GNU tar for the long options
BusyBox lacks; e2fsprogs for `mkfs.ext4`, `fsck.ext4`, `e2fsck` and
`resize2fs`; util-linux for `fstrim -a` and `nsenter`; curl; crane; crun; and
seatd. There is no CUDA runner, Vulkan bundle, or Rosetta wrapper: bctl uses
none of them.

`runtime/guest.sh` does only that container work. It reuses existing kernel,
init, rootfs, and disk-template outputs without comparing their source inputs.
Set `FORCE=1` after changing guest inputs or pins. The kernel archive is verified
against `UPSTREAM.lock` before it enters the container, and `krun-init` from
that build is embedded into the host `libkrun.dylib` rather than rebuilt on the
host, so the host and guest agree on it by construction.

## Installed layout

```
.work/runtime/smolvm/
  smolvm                  # sets DYLD_LIBRARY_PATH and SMOLVM_AGENT_ROOTFS
  smolvm-bin              # the CLI, signed with smolvm.entitlements
  agent-rootfs/           # the guest agent rootfs, static musl
  lib/
    libkrun.dylib
    libkrunfw.5.dylib -> copied file, plus the libkrunfw.dylib symlink
    libvirglrenderer.1.dylib
    libMoltenVK.dylib
    libepoxy.0.dylib
  storage-template.ext4.zst
  overlay-template.ext4.zst
```

The wrapper is upstream's `scripts/smolvm-wrapper.sh`; it sets
`SMOLVM_AGENT_ROOTFS` to the bundled directory and puts `lib/` on
`DYLD_LIBRARY_PATH`. The disk templates are next to the binary, which is where
the CLI looks for them after `$HOME/.smolvm`.

The GPU dylibs are Homebrew's, rewritten so every dependency resolves at
`@loader_path` and re-signed ad-hoc. `libMoltenVK` and `libepoxy` are the
Homebrew builds; `libvirglrenderer` is `libkrun/krun/virglrenderer-krun`, the
Venus-only build whose 2D refusal the patch works around.

## Nanos runtime layout

On Linux x86_64, the builder requires `/dev/kvm`, QEMU, GCC, ccache, Rust,
and the kernel build tools named by `kernel/dev/toolchain.lock`.
Image packaging also needs Python 3 and `fd`.
`virtiofsd` also needs the host libseccomp and libcap-ng runtime libraries.
Commit any `kernel/` changes first; the builder refuses uncommitted ones.

```sh
sh runtime/build-nanos.sh
BCTL_BROWSER_ARCHIVE=DIR sh images/nanos-helium/build.sh
bctl_state="${BCTL_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/bctl}"
mkdir -p "$bctl_state/runtime/nanos" "$bctl_state/images"
ln -sf "$PWD/.work/runtime/nanos/bin/vhost-device-vsock" "$bctl_state/runtime/nanos/vhost-device-vsock"
ln -sf "$PWD/.work/runtime/nanos/bin/virtiofsd" "$bctl_state/runtime/nanos/virtiofsd"
cp .work/images/nanos-helium.img .work/images/nanos-helium.kernel.img .work/images/nanos-helium.profile.img "$bctl_state/images/"
BCTL_RUNTIME=nanos target/debug/bctl doctor
```

`DIR` holds the accepted x86_64 Helium archive and sidecars. The build cache
is described in [build-loop.md](build-loop.md). Installed paths are:

```
<BCTL_HOME>/runtime/nanos/vhost-device-vsock
<BCTL_HOME>/runtime/nanos/virtiofsd
<BCTL_HOME>/images/<image>.img
<BCTL_HOME>/images/<image>.kernel.img
<BCTL_HOME>/images/<image>.profile.img
```

The image and its kernel are a pair. `images/nanos-helium/build.sh` writes
`.work/images/nanos-helium.img`, `.work/images/nanos-helium.kernel.img` and
the empty profile volume template `.work/images/nanos-helium.profile.img`
together, copying the kernel from the kernel build whose `tmpfs` and `shmem`
klibs the image packs. Install all three files: bctl boots
`<BCTL_HOME>/images/<image>.kernel.img` beside `<image>.img`. A kernel from a
different build than the image's klibs crashes in `klib_initialize`, so there
is no separate `runtime/nanos/kernel.img`; that directory holds only
`vhost-device-vsock` and `virtiofsd`. `virtiofsd` is built from crates.io
against the host's libseccomp and libcap-ng, which only need their runtime
libraries installed. Starting a session with an image but no kernel beside it
fails with both paths and the build command. On Linux each profile's disk is
a qcow2 file converted once from the template, so it starts at a few
megabytes and grows with the profile; QEMU's image lock keeps two sessions
from opening it at once.

On Apple Silicon, the experimental Nanos backend uses libkrun and an EFI disk
image containing its own kernel. Run `runtime/guest.sh` and
`runtime/build-nanos-krun.sh`, then install `.work/runtime/nanos-krun` as
`<BCTL_HOME>/runtime/nanos`. The bundle includes the signed runner, optimized libkrun,
EFI firmware and its license. Run `runtime/build-nanos-arm64.sh`, then
`BCTL_BROWSER_ARCHIVE=DIR sh images/nanos-helium/build-macos.sh`, and copy
both `.work/images/nanos-helium.img` and `.work/images/nanos-helium.profile.img`
to `<BCTL_HOME>/images`. The second file is the empty profile volume template.
The image embeds the kernel and modules built together; it needs no separate kernel file.
Ready headless sessions report no VNC port and reject viewer input.
The kernel's ARM checks are recorded in the
[macOS qualification](kernel/macos-qualification.md).

On both platforms, internal VM sockets use private, short directories under
`<BCTL_HOME>/ns` so long session names fit the Unix socket path limit.
Each profile has its own Nanos disk, initialized from the template only once
and locked while in use. Stopping a Nanos session asks the guest to close the
browser through CDP `Browser.close`, which commits the cookie store's pending
writes (SIGTERM drops up to 30 s of them), and waits for the guest to flush
the disk and power off. Profiles from the SmolVM backend use a different
format and cannot be opened as Nanos profiles, or vice versa; bctl refuses to
open a profile in the other runtime rather than replacing its state.
After building the runner and kernel, `python3 tests/runtime-nanos/vsock-reconnect.py`
checks reconnects, shutdown and port reuse with a bounded host descriptor limit.
The ignored `completed_cdp_connections_do_not_exhaust_the_vmm_descriptors`
runtime test makes 1,000 fresh CDP connections with the macOS VMM limited to
128 descriptors. It covers host-initiated connections through the guest relay.

On either host, a failed Nanos start terminates the supervisor's process group,
allowing five seconds for TERM and five seconds for KILL. If processes remain,
the launcher exits with status 3 and keeps the machine and socket directories
for recovery.

An ordinary `bctl down` of a SmolVM session runs `machine stop` with a 30 s
timeout, which covers the headed appliance's 25 s stop sequence, and then
deletes the stopped machine. A stop that fails still deletes the VM and is
reported as a forced removal. Failed or stale cleanup, and a paused machine,
are deleted at once.

## Verification

`runtime/build.sh` fails unless every check passes:

- each patch's digest matches `UPSTREAM.lock`, and each patch applies;
- libkrun exports `krun_add_display`, `krun_add_input_device`, and
  `krun_set_display_backend`;
- `libkrun.dylib` references no Homebrew path, so it loads only what is bundled;
- `libkrun.dylib` and `libkrunfw.5.dylib` carry a valid ad-hoc signature;
- `smolvm-bin` is signed with exactly the entitlements in
  `smolvm.entitlements`, including `com.apple.security.hypervisor`, which HVF
  requires;
- every staged Mach-O is `arm64`;
- the guest rootfs and `krun-init` have no `INTERP`, no `DT_NEEDED`, no GCC
  `.comment`, and no GNU runtime symbols;
- `smolvm --version` reports the version pinned in the lock.

`smolvm.entitlements` is upstream's file: the hypervisor entitlement, the
disabled library validation that lets the signed binary load the ad-hoc-signed
dylibs, and the JIT allowance.

## Refreshing it

Follow the upstream default-branch tip:

```sh
scripts/update-smolvm.sh
runtime/build.sh
```

Pin a stable release instead of the tip:

```sh
scripts/update-smolvm.sh v1.31.4
runtime/build.sh
```

The importer resolves the selected commit, the exact libkrun and libkrunfw
revisions that commit's gitlinks name, and the kernel archive identity from
libkrunfw's `Makefile`. It validates the archive layout, symlinks, license
notices, and that every patch applies, then replaces `third_party/smolvm` and
`UPSTREAM.lock` together. It stages the replacement under `.work` and refuses a
source tree and lock that disagree, so a partial write fails loudly instead of
building something unpinned. It prints a source diff against the previous tree
before publishing.

Review the diff before committing. When a patch no longer applies, rebase it in
`runtime/patches/` onto the new tree and import again; the importer will not
accept a patch that does not apply, and the build will not accept one whose
digest changed.

## What is still a release artifact

Nothing in the runtime. libkrun, libkrunfw (and so the guest kernel),
`krun-init`, the guest agent, the agent rootfs, the disk templates, and the
CLI are all built from source. The rootfs's CA bundle is Mozilla's certificate
data, pinned by SHA-256, not a binary. The Homebrew GPU dylibs are third-party
dependencies, not SmolVM release files, and they are what the release bundle
ships too.

## Tests

Build the dashboard first, as above. The SmolVM VM tests and proofs read
`.work/runtime/smolvm` directly.

```sh
cargo test --workspace                                   # no VMs
cargo test -p bctl-runtime-smolvm --test lifecycle -- --ignored
cargo test -p bctl --test daemon -- --ignored
cargo test -p bctl --test profiles -- --ignored
cargo test -p bctl --test cdp -- --ignored
cargo test -p bctl --test egress -- --ignored
cargo test -p bctl --test tabs -- --ignored
cargo test -p bctl --test snapshots -- --ignored
cargo test -p bctl --test actions -- --ignored
cargo test -p bctl --test batches -- --ignored
cargo test -p bctl --test authority -- --ignored
BCTL_GRAB="$PWD/../shadowdriver/grab/.venv/bin/grab" cargo test -p bctl --test runs -- --ignored
cargo run -p bctl-proof -- m1 --runs 10
BCTL_PROOF_IMAGE=.work/images/ref-alpine-tinyland.tar cargo run -p bctl-proof -- m1
pnpm --dir web exec playwright test                      # needs cargo build -p bctl
```

The ignored tests and the proofs boot real VMs from `.work/runtime` and
`.work/images`.
