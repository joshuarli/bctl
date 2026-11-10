# Nanos runtime build loop

`runtime/build-nanos.sh` builds three independent parts under
`.work/runtime/nanos`: the Nanos kernel and tools, the patched
`vhost-device-vsock` backend, and the LLVM toolchain named by the hydrogen
tree's `dev/toolchain.lock`.

Each part has a stamp beside its output. A part is skipped only when the
stamp records the same input identity and every output still exists with the
SHA-256 the stamp records. The stamp stores output paths relative to
`.work/runtime/nanos`, so moving the worktree or reaching it through a
symlink does not invalidate it. `FORCE=1` rebuilds all three.

## What each part keys on

| Part | Input identity | Outputs checked |
|---|---|---|
| kernel and tools | pinned hydrogen commit, `ccache gcc`, `gcc --version`, `ccache --version`, make targets `kernel tools` | `kernel.img`, `boot.img`, `mkfs` |
| vhost-device-vsock | crate version, crates.io SHA-256, patch digest, `rustc -V`, `cargo -V` | `bin/vhost-device-vsock` |
| LLVM | `LLVM_URL`, `LLVM_SHA256`, `LLVM_DIR`, `LLVM_RELEASE` from `dev/toolchain.lock` | `llvm/bin/clang` |

The LLVM inputs come from `git show <commit>:dev/toolchain.lock` in the
hydrogen clone, so an LLVM-only rebuild does not need the exported kernel
tree. The script prints one line per part, `kernel: reused` or `kernel:
built` (and likewise for `vhost-device-vsock` and `llvm`), which
`tests/build/build-nanos-cache.sh` reads to assert which parts ran.

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
