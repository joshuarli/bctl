# Runtime

bctl runs its VMs on a SmolVM runtime that it builds from source. The source is
vendored under `third_party/smolvm`, pinned by `third_party/UPSTREAM.lock`, and
built by `runtime/build.sh` into `.work/runtime/smolvm`.

The engine is the upstream default-branch tip, not a release bundle. bctl keeps
it in-tree because it fixes SmolVM itself: the 512-entry TCP relay table in
`crates/smolvm-network/src/tcp_relay.rs` and pause/checkpoint for its own
sessions come later. Nothing has been reported upstream.

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
digest, and the two Rust toolchains. The source identity covers the whole tree,
so a build that does not match it is refused.

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

`runtime/patches/` holds bctl's patches, applied in the order `UPSTREAM.lock`
lists them. Each patch names the tree it applies to in its filename
(`<tree>-<order>-<slug>.patch`).

`libkrun-0001-rutabaga-2d-beside-3d.patch` keeps a 2D rutabaga component
beside the default one. macOS virglrenderer is built for Venus only and rejects
`RESOURCE_CREATE_2D` with `EINVAL`, so no CPU-rendered scanout reaches the host.
The patch creates a resource the default component refuses on the 2D component,
and every later operation on that resource (backing attach and detach,
transfer, flush, unref) is routed by the resource's own component. This is what
makes `RESOURCE_CREATE_2D` work, and Tinyland's dumb buffers need it too.

`smolvm-0001-graviola-only-crypto.patch` makes graviola the tree's only
cryptography provider, so no `ring` or `aws-lc-rs` package is built or linked.
rustls keeps no crypto backend; every TLS config takes `rustls-graviola`'s
provider, installed once as the process default, and reqwest, hyper-rustls,
tokio-rustls and axum-server use their `*-no-provider` features. Certificates
rcgen used to sign through ring are generated and signed by graviola P-256
through rcgen's `RemoteKeyPair`; the serial number and subject key identifier
are supplied explicitly because a crypto-less rcgen cannot derive them.
`smolvm-s3fs` uses reqwest's blocking client in place of ureq, whose `tls`
feature hard-enables ring, and the SHA-256 call sites use the tree's existing
`sha2` instead of ring's digest.

`libkrun-0002-gpu-2d-snapshot.patch` makes a virtio-gpu device that runs on
rutabaga's 2D component alone checkpointable. `KRUN_GPU_BACKEND=2d` now builds
no virglrenderer component and advertises no capsets, so the device is a 2D KMS
scanout and nothing else. Its device state carries the 2D resources, their
guest backing ranges and the scanout assignment, but no pixels: a restore
re-attaches each backing, reloads the resource from it (the backing in the
checkpointed guest RAM holds everything the guest ever transferred), reconfigures
the host scanout and presents the frame before the guest runs. Saving pixels
instead put every framebuffer in the device state and overran the checkpoint
stream's 16 MiB state limit after the first resume. The patch also gives virtio-input a device state
(negotiated features and both queue rings), so a restored guest keeps its
event and status queues instead of finding them silently reset.

`smolvm-0002-pause-2d-sessions.patch` records the GPU backend on the machine
(`--gpu-mode 2d|3d`), selects it at every launch, and admits a paused session
with a 2D GPU, exposed Unix sockets, and host-backed image layers. An exposed
socket's listener is re-created at the same host path on resume, because every
launch from the machine record now carries the record's published sockets,
Docker socket and CUDA flag (resume used to drop them); its live connections
are dropped. A runtime refusal on the checkpoint stream is reported with its own
reason instead of as an invalid stream. A host-backed image archive is admitted only under its
recorded content digest, which resume re-checks and refuses with the expected
and actual digests if the archive changed. Virgl/Venus GPUs, `local-dir:` image
layers and the Docker socket stay refused.

`smolvm-0003-vnc-shifted-keysyms.patch` holds Shift around a shifted keysym
(`:`, `"`, `A`, and the rest of the US layout's shifted symbols) when the VNC
client sent no Shift key event of its own. The keysym table maps `:` and `;` to
the same key, so before this patch a client that sends only the shifted symbol,
as noVNC does for synthesized keys, typed `;`. Clients that send real Shift
events are unchanged.

`libkrun-0003-arm-mmio-interrupts.patch` lowers an HVF GIC level interrupt
when the guest acknowledges its last virtio-MMIO cause. It also supports
16-bit MMIO writes from arm64 firmware.

`libkrun-0004-arm-efi-acpi.patch` supplies ACPI tables to arm64 EFI guests
through fw_cfg. The tables describe the configured CPUs and attached MMIO
devices. Firmware-backed guests cannot be checkpointed until fw_cfg state is
included in the checkpoint.

The importer applies every patch to freshly imported source, so a patch that no
longer matches the tip fails the import, not a later build. `runtime/build.sh`
verifies each patch's digest and that it applies to either the pristine or the
already-patched tree.

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

`runtime/guest.sh` does only that container work. It is skipped when its four
outputs are present, so a host rebuild of libkrun and the CLI does not rebuild
the kernel; set `FORCE=1` to rebuild them anyway. The kernel archive is verified
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

On Linux x86_64 with `/dev/kvm`, bctl also runs hydrogen's Nanos.
`runtime/build-nanos.sh` builds under `.work/runtime/nanos`; what bctl needs
from it is installed as:

```
<BCTL_HOME>/runtime/nanos/vhost-device-vsock
<BCTL_HOME>/images/<image>.img
<BCTL_HOME>/images/<image>.kernel.img
```

The image and its kernel are a pair. `images/nanos-helium/build.sh` writes
`.work/images/nanos-helium.img` and `.work/images/nanos-helium.kernel.img`
together, copying the kernel from the hydrogen tree whose `tmpfs` and `shmem`
klibs the image packs. Copy both files: bctl boots
`<BCTL_HOME>/images/<image>.kernel.img` beside `<image>.img`. A kernel from a
different build than the image's klibs crashes in `klib_initialize`, so there
is no separate `runtime/nanos/kernel.img`; that directory holds only
`vhost-device-vsock`. Starting a session with an image but no kernel beside it
fails with both paths and the build command.

On Apple Silicon, the experimental Nanos backend uses libkrun and an EFI disk
image containing its own kernel. Run `runtime/guest.sh` and
`runtime/build-nanos-krun.sh`, then install `.work/runtime/nanos-krun` as
`<BCTL_HOME>/runtime/nanos`. The bundle includes the signed runner, libkrun,
EFI firmware and its license. Run `runtime/build-nanos-arm64.sh`, then
`BCTL_BROWSER_ARCHIVE=DIR sh images/nanos-helium/build-macos.sh`, and copy
`.work/images/nanos-helium.img` to `<BCTL_HOME>/images`. The image embeds the
kernel and modules built together; it needs no separate kernel file. Set
`BCTL_RUNTIME=nanos` to select this backend; SmolVM remains the macOS default.

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

## Using it

bctl finds the runtime at `<BCTL_HOME>/runtime/smolvm/smolvm`. Put the built
tree there:

```sh
home="$HOME/Library/Application Support/bctl"
mkdir -p "$home/runtime"
ln -sfn "$PWD/.work/runtime/smolvm" "$home/runtime/smolvm"
target/debug/bctl doctor
```

The VM-backed tests and the proofs read `.work/runtime/smolvm` directly.
