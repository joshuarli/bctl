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

Both patches are applied in the order `UPSTREAM.lock` lists them.

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
4. **The guest agent rootfs** and the pre-formatted disk templates, built in a
   pinned Debian container (see below).

The kernel and the guest agent are built in a container because this host is
macOS: the kernel needs a Linux toolchain, and the agent's C dependencies
(`ring`) need a musl C compiler and headers that macOS does not have. The
container image is pinned by digest, and every input it fetches is pinned:
Alpine's release, the `apk.static` from it, Crane's release archive by SHA-256,
and Rust from rustup.

`runtime/guest.sh` does only that container work. It is skipped when its four
outputs are present, so a host rebuild of libkrun and the CLI does not rebuild
the kernel; set `FORCE=1` to rebuild them anyway. The kernel archive is verified
against `UPSTREAM.lock` before it enters the container, and `krun-init` from
that build is embedded into the host `libkrun.dylib` rather than rebuilt on the
host, so the host and guest agree on it by construction.

The agent rootfs is upstream's Alpine rootfs (3.24.4, the release in
`runtime/guest.sh`), built by upstream's own `scripts/build-agent-rootfs.sh`
with `--crane-release`. Two of that script's optional components are not in it:
`smolvm-cuda-run` (the `--cuda` guest runner) and the Vulkan/Venus guest driver
bundle, which upstream's script stages from RPMs and therefore needs an RPM
extractor the pinned container does not have. bctl uses neither: it never passes
`--cuda`, and its guests render on the CPU. The release rootfs is Alpine 3.19;
the version is not part of what an appliance image provides, because the image
brings its own userland.

## Installed layout

```
.work/runtime/smolvm/
  smolvm                  # sets DYLD_LIBRARY_PATH and SMOLVM_AGENT_ROOTFS
  smolvm-bin              # the CLI, signed with smolvm.entitlements
  agent-rootfs/           # the guest agent rootfs, Alpine-based
  lib/
    libkrun.dylib
    libkrunfw.5.dylib -> copied file, plus the libkrunfw.dylib symlink
    libvirglrenderer.1.dylib
    libMoltenVK.dylib
    libepoxy.0.dylib
  templates/
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
CLI are all built from the vendored tip. The Homebrew GPU dylibs are
third-party dependencies, not SmolVM release files, and they are what the
release bundle ships too.

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
