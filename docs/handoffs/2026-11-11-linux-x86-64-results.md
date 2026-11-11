# Linux x86_64 results, 2026-11-11

From threadripper (AMD Ryzen 9 9950X, Alpine Linux edge 7.2.9), in reply to
[the Linux handoff](2026-11-11-linux-x86-64.md). Pull `main` at `d0da931` or
later, and Hydrogen `main` at `6dc7d43e` or later.

## Done

- **Interop on Linux Nanos.** `docs/compatibility.md` has a `linux-nanos`
  tuple: 113 of 113 in 2 of 4 runs, file checks included. The two failures
  are listed there.
- **Roadmap.** M8C's status records the Linux leg.
- **No guest stall.** The Linux "stall" was `bctl run` hashing the 2 GiB
  Nanos image on every run, which took 97 s in a debug build. `a11b620` keeps
  the digest until the image file changes, and dev builds optimize `sha2`.
  There was nothing to bisect.
- **Files directory on Nanos (Hydrogen `6dc7d43e`, bctl `d0da931`).**
  Hydrogen has a virtio-fs driver: FUSE 7.31 over PCI or MMIO, mounted by its
  tag. Its harness suite `dev/test.sh virtiofs` passes under QEMU with 1 and
  2 CPUs. The bctl side:
  - On Linux, each session's supervisor runs `virtiofsd` with its namespace
    sandbox for `<BCTL_HOME>/files/<session>`.
  - `nanos-krun` passes the directory to libkrun with `krun_add_virtiofs`.
  - The images mount tag `files` at `/home/browser/Downloads`.
  - `bctl inspect` now always reports `files_dir`.
  - `runtime/build-nanos.sh` builds virtiofsd 1.14.0.
- **Both Hydrogen pins are at `6dc7d43e`.** That revision has the arm64
  secondary CPU fix, the x86 FIONREAD fix, and virtio-fs.
- **Orphaned VMs on remove (`c9db778`).** When the control socket does not
  answer, `remove()` stops the supervisor's process group, which the
  supervisor records in a locked `supervisor.lock`. A supervisor killed
  with SIGKILL can still leave its VMM behind.
- **Interop script fixes.** A half-built browser-use venv no longer blocks
  later runs (`5428dbe`). Linux receipts print the image hash (`3b29402`).

## Verify on the Mac

Done on 2026-11-11. The results are below. Before that, everything above
type-checked for `aarch64-apple-darwin` and had not run on macOS.

- Rebuild with `runtime/build-nanos-arm64.sh`, `runtime/build-nanos-krun.sh`
  and `images/nanos-helium/build-macos.sh`.
- Run `cargo test -p bctl-runtime-nanos`, then rerun the mac-nanos tuple and
  update its row and its file cells.
- In Hydrogen, run `dev/test.sh virtiofs` under HVF. It checks the virtio-fs
  driver over MMIO through libkrun, and `dev/krun-run.c` now takes
  `VIRTIOFS_TAG VIRTIOFS_DIR`.

## Open

- **Empty guest shutdown reply.** `bctl down` failed once in the suite and
  once in 84 up/down cycles. The guest closed its stop connection without
  answering `ok`, so the session stayed `stopping` until the daemon settled
  it. No failing run kept the guest console.
- **browser-use navigate timeout.** It happened once, in the `ultrafast`
  cell: `Page.navigate` timed out after 20 s.
- **Linux measurement** (`docs/performance.md#external-clients`). This is
  waiting for a quiet host. An xsh campaign shares threadripper and cannot be
  paused, and `perf-gate-ready` passed even at a load average of 21.
- **9p in Hydrogen.** Reading the code, `9p.c` frees a closed file's
  metadata while its parent still lists it. virtio-fs hit exactly this bug
  and now avoids it. 9p is untested.
- **Hydrogen harness.** `cpus_online` does not build on `main`, which stops
  `dev/test.sh harness`; `dev/test.sh virtiofs` builds only its own fixture.
- **Host tooling on threadripper.** The git build in
  `~/.local/share/git/current` has its exec path set to `/usr/bin`, so
  `git submodule` fails. These runs set
  `GIT_EXEC_PATH=$HOME/.local/share/git/current`.

## Carried over

- Existing Linux Nanos homes still need `<image>.kernel.img`, and now also
  `runtime/nanos/virtiofsd`.

## Mac results, 2026-11-11

Apple M5 Max, macOS 27.0.1. bctl `e1d1d62`, Hydrogen kernel pin `6dc7d43e`.

- **Rebuild.** `runtime/build-nanos-arm64.sh`, `runtime/build-nanos-krun.sh`,
  and `images/nanos-helium/build-macos.sh` with the Helium r6 archive. The
  bundle is installed at `~/Library/Application Support/bctl/runtime/nanos`,
  with `nanos-helium.img` and `nanos-helium.profile.img` beside the other
  images. Image sha256
  `399f8ca598690434de22b6847b8ed80e6efb84db3bd38ad5bddbdbad7c3eccce`.
  That image build also re-saved `helium.tar`, so its tar hash is no longer
  the mac-smolvm receipt.
- **`cargo test -p bctl-runtime-nanos`.** 17 passed, 0 failed. BSD accepts
  `/bin/kill -s SIG -PGID` and `pgrep -g`. The lock tests and
  `remove_stops_a_supervisor_whose_control_socket_is_gone` passed.
- **virtiofs under HVF.** `dev/test.sh virtiofs` passed 5 of 5 at one CPU
  and at two, and the host check. The first run's host check compared BSD
  `wc`'s padded count with `1000` and failed after a correct guest run.
  Hydrogen `f503d850` strips that padding. The share was 1000 bytes and
  `gdir` was gone.
- **Interop.** `BCTL_RUNTIME=nanos sh tests/interop/run.sh` passed 113 of
  113 twice. One run before `e1d1d62` failed the download check: Chromium
  had created `.org.chromium.Chromium.*` and the check treated it as the
  finished file, although `blob.bin` was 200000 bytes. The guest-path upload
  and the owner-death reclaim failed in that same run. Both passed in the
  two runs after the suite ignored dotfile download temps. The reclaim
  failure left no `console.log`. The suite deletes its home on exit.
- **SmolVM.** Two runs on the re-saved `helium.tar`, each 112 of 113. One
  missed `browser-use no client socket left open`: the check saw a socket
  that was gone when the failure line was printed. The other missed
  `cookies stay in their profile` because `bctl page eval` printed nothing.
  The file checks passed both times. The mac-smolvm row is unchanged.
