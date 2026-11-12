# Nanos runtime build loop

`runtime/build-nanos.sh` builds four parts under `.work/runtime/nanos`:
the Nanos kernel and tools, patched `vhost-device-vsock`, `virtiofsd`, and
the LLVM toolchain named by Hydrogen's `dev/toolchain.lock`.

Each part has a stamp beside its output. A part is skipped only when the
stamp records the same input identity and every output still exists with the
SHA-256 the stamp records. The stamp stores output paths relative to
`.work/runtime/nanos`, so moving the worktree or reaching it through a
symlink does not invalidate it. `FORCE=1` rebuilds all four.

## What each part keys on

| Part | Input identity | Outputs checked |
|---|---|---|
| kernel and tools | pinned hydrogen commit, `ccache gcc`, `gcc --version`, `ccache --version`, make targets `kernel tools` | `kernel.img`, `boot.img`, `mkfs` |
| vhost-device-vsock | crate version, crates.io SHA-256, patch digest, `rustc -V`, `cargo -V` | `bin/vhost-device-vsock` |
| virtiofsd | crate version, crates.io SHA-256, `rustc -V`, `cargo -V` | `bin/virtiofsd` |
| LLVM | `LLVM_URL`, `LLVM_SHA256`, `LLVM_DIR`, `LLVM_RELEASE` from `dev/toolchain.lock` | `llvm/bin/clang` |

The LLVM inputs come from `git show <commit>:dev/toolchain.lock` in the
hydrogen clone, so an LLVM-only rebuild does not need the exported kernel
tree. The script prints one line per part, `kernel: reused` or `kernel:
built`, with equivalent lines for `vhost-device-vsock`, `virtiofsd`, and
`llvm`. `tests/build/build-nanos-cache.sh` currently checks the kernel,
vsock, and LLVM parts.

The kernel builds with `ccache gcc`; the tree path is the same on every
build, so a from-scratch tree hits the cache. The kernel and its klibs
(`tmpfs`, `shmem`) come from the same make run, and a reused kernel keeps
those files untouched. Repack an image only from the tree that produced its
kernel and klibs.

## Measured wall times

Linux x86_64, 32 cores, `perf-gate-ready` quiet around the timed builds.
Downloads were already in `.work/runtime/nanos/dl`.

| Case | Old script | New script |
|---|---|---|
| cold, from a removed tree | 17.47 s, 19.18 s | 17.93 s (empty `CCACHE_DIR`) |
| unchanged | 0.00 s | 0.46 s |
| vsock patch changed | 17.47–19.18 s (rebuilt all) | 0.81 s (vsock only) |
| kernel input changed | 17.47–19.18 s (rebuilt all) | 3.85 s (kernel only, warm ccache) |

A `FORCE=1` rebuild with the default warm ccache took 9.27 s. The old script
had no per-part key: a patch or pin change deleted the tree and rebuilt the
kernel, the vsock backend, and LLVM.

## Nanos Helium image build loop

`images/nanos-helium/build.sh` builds four parts and prints one line per
part, `installer:`, `rootfs:`, `guest:`, and `image:`, each `built` or
`reused`. `tests/build/nanos-helium-cache.sh` reads those lines to assert
which parts ran. `FORCE=1` rebuilds the installer, the rootfs, and the image;
the guest still goes through cargo under the persistent target, so cargo's
own incrementality still decides.

The archive sidecar check runs on every build: the script hashes the archive
and refuses a `.sha256` that does not bind that digest and name, an
`.acceptance.json` that does not name product `helium`, architecture `amd64`,
the archive and its digest, and an archive older than r5. Nothing cached can
skip this check.

| Part | Input identity | Reuse condition |
|---|---|---|
| installer | archive SHA-256, chromium-build pin, SHA-256 of the three scripts | stamp matches and the scripts are present |
| rootfs | archive SHA-256, chromium-build pin, hydrogen `COMMIT`, kernel stamp, `guest/appliance/rootfs/etc/{passwd,group}` | stamp matches and the rootfs integrity digest matches |
| guest | none: cargo decides | cargo does not say `Compiling bctl-guest` |
| image | archive SHA-256, chromium-build pin, hydrogen `COMMIT`, kernel stamp, kernel/boot/mkfs SHA-256, rootfs digest, guest SHA-256, this script | stamp matches and `.img`, `.kernel.img`, and `.profile.img` match their recorded SHA-256 |

The three chromium-build scripts are cached under
`.work/images/dl/chromium-build/<pinned-rev>/` and fetched only when absent.
The cached scripts are inputs to the installer stamp, so editing one reruns
the installer's `--verify-extract` rather than trusting the old result.

The rootfs integrity digest is the sorted `stat` output for every entry below
the rootfs: type, mode, size, name, and a symlink's target. It does not read
file contents. That is deliberate: the rootfs is 907 MiB and 542 entries, and
hashing it would cost more than the extraction it guards. A removal,
addition, or change to the recorded metadata changes the digest.
A same-size content edit or replacement with identical metadata does not.
An input-identity change or metadata-integrity miss rebuilds the rootfs from
the archive. This is a metadata check, not a content-integrity guarantee.

The guest builds with `CARGO_TARGET_DIR=.work/images/nanos-helium/target`,
which survives every run; a source change recompiles incrementally instead of
starting from an empty target directory. The image and its kernel are one
unit: the pack step writes `.work/images/nanos-helium.img` and copies the
exact `kernel.img` from the hydrogen tree whose `tmpfs` and `shmem` klibs it
packs to `.work/images/nanos-helium.kernel.img`. The image stamp records
those two outputs and the empty profile-volume template. A missing kernel
or template forces a repack. The three Nanos outputs
the pack uses are checked against `build-nanos.sh`'s kernel stamp first, so a
tree whose kernel no longer matches its stamp is refused instead of paired
with the image. A failed pack can currently delete the previous published
image; [BCTL-7](../TODO.md#bctl-7--p2--a-failed-nanos-image-rebuild-deletes-the-previous-usable-image)
tracks preserving that output through failure.

## Measured image build wall times

Linux x86_64, 32 cores, `perf-gate-ready` exited 0 immediately before and
after every timed run. The archive and the chromium-build script cache were
present.

| Case | Old script | New script |
|---|---|---|
| cold, `.work/images/nanos-helium` removed | 59.00 s | 38.00 s |
| unchanged | 32.00 s | 7.00 s |
| guest Rust code changed | 38.00 s | 14.00 s |

The old script did the same work in every case: it deleted the image work
directory, downloaded the chromium-build scripts, ran the installer and the
full rootfs build, built the guest in a fresh target directory, deleted that
directory, and packed. The old "unchanged" and "guest code changed" numbers
differ only because of filesystem cache warmth; the script never reused
anything.


## ccache

The kernel build from a freshly exported tree at the same path:

| Compiler | Wall time | Result |
|---|---|---|
| `gcc` | 12.28 s | no ccache |
| `ccache gcc`, first build | 14.08 s | 547 misses |
| `ccache gcc`, second build | 5.95 s | 547 hits, 0 misses |

`ccache -s` moved from 17333 hits and 14185 misses to 17880 hits and 14185
misses across the warm build. ccache stays.

## Test

`sh tests/build/build-nanos-cache.sh` forces a cold build, then checks an
unchanged run, a deleted `kernel.img`, a truncated `vhost-device-vsock`, a
changed patch digest through a scratch copy of the script and patch, a
changed hydrogen commit through a scratch clone and a repinned scratch copy
of the script, and a
changed compiler through a `gcc` wrapper on `PATH`. It asserts the three
part lines and leaves the worktree on the pinned commit with the original
patch.
