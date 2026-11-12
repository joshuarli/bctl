# macOS qualification results, 2026-11-12

The Apple Silicon qualification uses macOS 27.0.1 on an Apple M5 Max with
48 GiB RAM. The guest browser is Helium 0.20.3.1-r6 for Linux arm64 musl.
This report records the outcome of the [macOS handoff](2026-11-12-macos.md).

## Repairs

- `e75dfe9` closes BCTL-9. Failed Nanos launches terminate the supervisor's
  process group, including a runner that ignores TERM. Cleanup failure
  preserves the machine directory and reports exit status 3.
- `10c6166` fixes daemon restart recovery on macOS. Closing an unread Unix
  socket produces EOF here. The CLI probes readiness with the existing
  read-only ping before sending an operation. An operation with an uncertain
  outcome is never replayed.
- `e4dcc57` fixes libkrun's host socket leak after reciprocal write shutdowns.
  The old path retained one descriptor per completed CDP connection. The
  real-VM regression failed after 31 connections with a 128-descriptor limit.
  The repaired runtime completes 1,000 connections under the same limit.
- Hydrogen fixes two ARM boot races. Filesystem initialization no longer
  suspends while holding the storage spinlock. Virtio-MMIO queue vectors use
  the locked heap when growing alongside filesystem initialization.
  [Hydrogen's qualification report](../../../hydrogen/docs/macos-qualification.md)
  records the kernel evidence and tests.

The libkrun fix updates the existing `libkrun-0008-vsock-close.patch` and its
vendored result. Source identity verification and the runtime patch checks pass.
Tinyland and seatd remain pinned to their upstream owner at `5c5da46`.

## Runtime and image identities

| Component | Identity |
| --- | --- |
| SmolVM | 1.34.1, upstream `59bdcf62d769f6491a6fc49f63f8cd8320f7ac58`, 13 patches |
| libkrun | `9e6cdcab6462cdcc2770c6e4eec933e1e20922ea` plus the locked patches |
| Linux guest kernel | 6.18.60, rebuilt with the guest agent and pinned seatd |
| Hydrogen ARM pin | `5fe102e8a055a4c675467baae0cc90c28c8024f9` |
| `helium.tar` | sha256:`a889b1d2cf1b06e4c82746f339871926851a05a9c82e9d4f5a2c14979a935098` |
| `nanos-helium.img` | sha256:`ed145673302106f08dab45589c5325ef4bbe58f448ac7e8f26e5b0ddfb8bd732` |
| `nanos-helium.profile.img` | sha256:`05dd0dfc484d3c8448edfff441976f85f6ea87dfd878df73353902dc785d1936` |
| ARM `kernel.img` | sha256:`661a29f4e66153fe3d6d2d18a521debdebae638e687ada7d83cb465d9f14a37a` |
| ARM `kernel.elf` | sha256:`2a2178cfd4347aad7978ed63783663284a2e3721179fd6daa151e49e94515208` |

The guest build's ELF purity checks pass. Both host bundles were rebuilt
after the libkrun descriptor repair.

## Verification

| Check | Result |
| --- | --- |
| `cargo test --workspace --locked` | Pass |
| Failed Nanos startup cleanup | Both process-group regressions pass |
| Daemon restart recovery | All three restart tests pass, including no replay after a consumed operation |
| libkrun Unix proxy unit tests | 5 pass on macOS and native Linux |
| SmolVM lifecycle and restore | Pass again with the final host runtime |
| SmolVM bctl VM suites | All requested suites pass |
| SmolVM interoperability | 113/113 in each of two final runs |
| Nanos descriptor regression | 1,000 completed CDP connections at a 128-descriptor limit |
| Nanos lifecycle | All 4 tests pass with the final pin, including 10 consecutive ready/stop cycles and 300 checks |
| Nanos kernel regression | 200 persistent boots pass in two loops after the MMIO heap repair |
| Nanos profile tests | Committed-cookie crash recovery and 20 persistence cycles pass with the final pin |
| Nanos reconnect | 800 connections pass with the final pin and host runtime |
| Nanos interoperability | 113/113 in each of two runs with the final pin, image, and host runtime |

The requested SmolVM suites are `daemon`, `sessions`, `profiles`, `cdp`,
`egress`, `tabs`, `snapshots`, `actions`, `batches`, `authority`, `failures`,
`media`, `native_cdp`, `host_http`, and `runs`. Egress uses
`ref-alpine-chromium`. Runs uses Shadowdriver's `grab` environment.
The VM-crash profile case runs on Nanos only.
The requested bctl SmolVM suites passed before the final image export.
Lifecycle, restore, and interoperability were repeated against the final exports.

Receipts remain in `.work/tmp/`: `macos-bctl-*.log`,
`macos-smolvm-final-*.log`, `macos-smolvm-interop-final-{1,2}.log`,
`macos-nanos-*-final.log`, and `macos-nanos-interop-final-{1,2}.log`.
The descriptor regression receipts are `macos-cdp-close-before.log` and
`macos-cdp-close-final.log`. The Linux unit receipt is
`/home/josh/d/bctl/.work/tmp/macos-vsock-linux-unit.log` on threadripper.

An earlier concurrent interoperability attempt produced one false leak
failure per runtime. The suites scan the shared client environment's processes,
so each counted a client from the other run. The failure evidence remains in
`macos-{nanos,smolvm}-interop-concurrent-1.log`. Final receipts run sequentially.

## Verification limits

The pinned Carbon independent reviewer was unavailable. No independent review
is claimed. Hot-unplug during pending filesystem initialization remains
unexercised. HYD-3 and HYD-4 remain open. The owner waived performance gates
and relaxed the longer loops. BCTL-10 and the handoff's unrelated upstream
items remain outside this qualification.
