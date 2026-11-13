# Distribution design

This document explains how bctl will be packaged for macOS and Linux and
installed by `~/usr`'s `sh sync`. It is a design. None of it is implemented
yet. The current manual installation remains in [runtime.md](runtime.md#build-and-install).

## Scope

Releases are for the owner's own machines. On macOS, binaries are signed
ad hoc, and nothing is notarized or signed with a Developer ID. The hypervisor
entitlement on the Nanos runner works with an ad hoc signature once the
download's quarantine attribute is cleared, as `~/usr` already does for its
other tools.

Releases ship only the Nanos runtime. SmolVM stays in the repository for
differential testing and is never packaged. SmolVM is also the only runtime
with headed sessions, so released bctl supports headless sessions only.
A release build therefore has to select Nanos without `BCTL_RUNTIME`. Today
`RuntimeKind::chosen` in `crates/bctl/src/runtime.rs` falls back to SmolVM.

## Artifacts

A release has two kinds of artifact with different sizes and lifetimes.

| Artifact | Size today | Changes when |
| --- | --- | --- |
| CLI and runtime archive, one per platform | about 6 MB CLI plus 8 MB runtime on macOS | any bctl or runtime commit |
| `nanos-helium` image | 636 MB on disk, 204 MB compressed | the guest, browser, or Hydrogen pin changes |

The archive `bctl-<version>-<platform>.tar.xz` holds `bin/bctl` and
`runtime/nanos/`. On macOS, `runtime/nanos/` is the `nanos-krun` bundle: the
runner, `libkrun.dylib`, and the EFI firmware with its license. On Linux it
holds `vhost-device-vsock` and `virtiofsd`. `virtiofsd` is linked statically
against musl, so the archive needs no host libseccomp or libcap-ng. QEMU and
`/dev/kvm` remain host requirements, which `bctl doctor` already checks.

Each release also publishes `SHA256SUMS` and a `manifest.json`. The manifest
names the platform, the bctl commit, the Hydrogen pin, and the digest of every
image file this bctl accepts. The macOS image embeds its kernel. The Linux
image and its `.kernel.img` are one artifact, because a kernel from a different
build than the image's klibs crashes in `klib_initialize`.

Images are separate artifacts because a compressed image is 204 MB, against
about 14 MB for the archive contents, and images change less often. Shipping
them in every archive would make each CLI update a 200 MB download.

## Image fetching

bctl fetches its own images. A new `bctl image pull` reads the image digests
from the manifest built into the running bctl, downloads any missing image,
verifies its digest, and installs it under the data directory. bctl already
owns the image layout and the image and kernel pairing, so the fetcher lives
beside that knowledge instead of in each installer.

Images are GitHub release assets on the same `release-<sha>` releases. That
matches how `sh sync` already discovers and verifies releases. A GitHub asset
is limited to 2 GiB. If a compressed image approaches that limit, move images
to the OCI registry at `17166969.xyz`, which also stores unchanged layers once
across image versions.

## Data and state directories

Today one `BCTL_HOME` holds everything: on macOS
`~/Library/Application Support/bctl`, on Linux `$XDG_STATE_HOME/bctl`. Release
artifacts are immutable and versioned, while profiles, sessions, the daemon
socket, and its lock change at run time. They need different lifetimes and
backup treatment, so the design splits them before anything ships.

| Contents | macOS | Linux |
| --- | --- | --- |
| Runtimes and images | `~/Library/Application Support/bctl` | `$XDG_DATA_HOME/bctl` |
| Profiles, sessions, daemon socket and lock | `~/Library/Application Support/bctl` | `$XDG_STATE_HOME/bctl` |

`BCTL_HOME` keeps its meaning as a single root for tests and isolated
installations. Making the split before the first release avoids migrating
installed state later.

## Installing with `~/usr`

`sh sync tools bctl` installs a release the way `sh sync` installs gluon.

1. Find the newest `release-<sha>` through `releases.atom`.
2. Verify the platform archive and `manifest.json` against `SHA256SUMS`.
3. Extract to `~/.local/share/bctl/releases/<tag>-<platform>/` and point
   `~/.local/share/bctl/current` at it.
4. Link `~/.local/bin/bctl` through `current`.
5. Run `bctl image pull` and `bctl doctor` from the staged release before
   switching `current`. If either fails, the previous release stays selected.
6. Prune releases other than `current`.

## Building releases

Linux archives and all images build in the existing pinned containers. The
macOS runner and libkrun build on macOS, because they are signed there with
the hypervisor entitlement. A rebuild of the ARM kernel from the same Hydrogen
commit was byte-identical, so release checks can rebuild and compare kernel
and image digests.

## Integration test clients

`tests/interop/run.sh` downloads agent-browser 0.43.0 as its upstream release
binary and verifies it against a SHA-256 pinned in the script. `~/usr` installs
the latest agent-browser release binary the same way, verified against the
digest GitHub reports, so the tested client and the installed client come from
the same kind of artifact.
