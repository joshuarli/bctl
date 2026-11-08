# Validate the Rust daemon

Run local checks with the repository's pinned nightly and locked offline
dependencies. These commands do not qualify real devices or VT behavior.

## Run the normal checks

Run the tests and strict Clippy:

```sh
cargo test --offline --locked
cargo clippy --offline --locked --all-targets -- -D warnings
```

Build both supported musl targets when their Rust targets and the Linux
musl linker are installed:

```sh
export RUSTC_WRAPPER=kache
export RUSTFLAGS='-Clinker=mold -Clinker-flavor=ld'
cargo build --offline --locked --release --target x86_64-unknown-linux-musl
cargo build --offline --locked --release --target aarch64-unknown-linux-musl
```

Rust compilation uses the `kache` rustc wrapper. Linux musl links use `mold`.
The required target installations are external prerequisites; a command that
cannot find a target or linker does not establish a successful target build.

## Check the wire protocol

The protocol corpus and its expected transcript live in
[`harness/protocol.golden`](../harness/protocol.golden). It pins the
exact bytes the daemon must exchange: framing, opcodes, errors, and
the handoff sequence. Run it with:

```sh
cargo test --offline --locked --test differential
```

The golden file was verified byte-identical against the upstream C
daemon before the C sources left this repo, and is frozen since. It
checks client-visible behavior for the covered cases. It does not
qualify device access, VT switching, or compositor integration, and it
does not establish kernel behavior for DRM, evdev, or hidraw. A raw
socket guest client speaks the same protocol without any C code; see
[the VM recipe](../harness/VM.md).

## Run the idle timing gate

The idle parity case is ignored in the normal test run. Run it only when
`perf-gate-ready` reports an idle host. Check readiness immediately before and
after the measurement:

```sh
perf-gate-ready
cargo test --offline --locked --release --test storm storm_idle_stays_quiet -- --ignored --exact --nocapture
perf-gate-ready -1
```

If either readiness check reports a busy host, discard the measurement. The
gate is deferred until both checks pass. A deferred gate is not a pass, and
VT-disabled timing does not establish VT behavior. Do not report performance
parity until the valid measurement is available.

## Miri scope

Miri validates the pure buffer and codec logic only. Run it with the
pinned toolchain after installing the component once
(`rustup component add --toolchain nightly-2026-09-15 miri`):

```sh
cargo +nightly-2026-09-15 miri test --locked --offline --lib -- --exact \
  conn::tests::ring_wraparound_preserves_order \
  conn::tests::ring_counters_wrap_without_loss \
  conn::tests::ring_restore_recovers_frame_header \
  wire::tests::wire_opcode_values_match_c_header \
  wire::tests::wire_header_round_trip_native_endian \
  wire::tests::wire_header_explicit_bytes_not_struct_cast
```

All six pass under Miri. Everything else in `conn.rs` is kernel
behavior Miri cannot validate: `recvmsg`/`sendmsg` are foreign calls
Miri refuses to execute, SCM_RIGHTS transfer, `EINTR`/`EAGAIN` retry,
`MSG_CTRUNC` handling, EOF semantics, and FD ownership across the
kernel boundary are proven only by the real socketpair tests in the
normal suite and the disposable-guest qualification below.

## Re-qualification after the socket and lifecycle repairs

On 2026-10-28 at `62da737`, the repaired release binaries re-passed
the guest and client proofs. Build recipe: pinned nightly, locked
offline deps, `RUSTFLAGS="-Clinker=mold -Clinker-flavor=ld"` on top of
the container's `~/.cargo` kache wrapper (local deviation from the CI
recipe, which sets only the env flags; hashes below are for the local
recipe and are recorded as tested, not as canonical release hashes):

| Target | Size | SHA-256 |
|---|---:|---|
| x86_64-unknown-linux-musl | 692376 | `339e726d9e4607fc00c8acf974a706bb2d19e2938a98b57b4de13a844c88cfae` |
| aarch64-unknown-linux-musl | 671952 | `810e72a4670450355898428bd0965503224ebfab6ba31c7dad1ecf10d4826c00` |

Both are static (`DT_NEEDED` count 0), x86_64 static-pie. Evidence run
on the exact binaries above:

- KVM guest `transition` mode: release, acquire, device revocation and
  reopening, DRM master handoff, restoration after closing the seat —
  `VM_TRANSITION_OK`.
- KVM guest `shutdown` mode: restoration after SIGTERM with a client
  connected — `VM_SHUTDOWN_OK`.
- Raw-protocol guest client: the same seat, device, revoke, and handoff
  checks pass with no C code in the loop — markers above.
- `cargo test --offline --locked` green (57 lib tests including the new
  admission and revoke-transition pins), strict Clippy clean.

Compositor mode was not re-run (needs the Laputa package mirror, absent
here); N3 dwl/foot proof remains the earlier recorded run. Physical
devices, hidraw revoke, and ARM hardware stay out of scope as before.

## Qualification limits

N1–N3 passed on 2026-10-28 in an x86_64 KVM guest running Alpine
Linux 6.6.142-0-virt, using virtual GPU and keyboard devices only:

- N1: VT1→VT2→VT1 release/acquire, console restoration after seat close,
  and SIGTERM restoration while a seat client remained connected.
- N2: evdev returned `ENODEV` after revoke and worked after close/reopen.
  A competing DRM descriptor gained master while the seat was disabled and
  was refused master again after the original seat reacquired it.
- N3: dwl 0.9 with wlroots 0.20 and foot rendered a magenta terminal marker.
  QMP keyboard injection reached foot through libinput and Rust seatd.

The integrated harness reproduced all three modes. The compositor screenshot
was also visually inspected. Commands and cached-guest preparation are in
[the VM recipe](../harness/VM.md). No host DRM, input, or VT device was used.
This proof covers that guest configuration. Physical devices, hidraw revoke,
and ARM hardware remain outside these results. Unit tests and raw protocol
parity alone do not establish kernel device behavior. Since the upstream C
sources left this repo, no C libseat client has been run against the
daemon; compatibility rests on the frozen golden transcript and the
raw-protocol guest client, and a C-client run remains an open gap.

The recorded R4 history includes one unidentified test failure during
contaminated reruns. It remains unclassified. The multi-client disconnect
crash reproduced during R7 was fixed; that fix does not resolve the unknown
R4 report.

## Local delivery results (superseded)

The record below is the 2026-10-28 closeout, kept for history. It
references C sources and differential scripts that have since left
this repo; the re-qualification entry above it is the current
evidence. The R4 unknown and the hidraw, ARM, and compositor limits
below still stand.

On 2026-10-28, the pinned toolchain and locked dependencies passed 79 tests.
The normal suite ignores the idle timing case. That case subsequently passed
five release-mode samples with valid readiness checks. Strict Clippy passed
with warnings denied. Both retained C unit tests passed after
a fresh oracle rebuild.

Both musl release binaries built with `mold` and reported version `0.9.3`.
The aarch64 binary ran through `qemu-aarch64` user-mode emulation.
This does not qualify ARM hardware, devices, or real VT behavior.

| Target | Unstripped binary size |
|---|---:|
| x86_64-unknown-linux-musl | 685080 bytes |
| aarch64-unknown-linux-musl | 665416 bytes |

The [CI workflow](../.github/workflows/ci.yml) runs the functional checks and
both target builds on a disposable Ubuntu runner. Checkout uses a full commit
pin. The compiler wrapper version is pinned. The wrapper
archive is checked against its pinned digest. Dependency fetches use
`Cargo.lock`, and compilation runs offline afterward. CI leaves the idle
measurement deferred until a separate host-ready run.

Workflow YAML, embedded shell syntax, and the explicit musl target
configuration were checked locally. Hosted CI runs on push; check the
workflow status for the pushed head.

## Idle gate result

Five release-mode samples passed the default host-readiness check before
each run and the `-1` check immediately afterward. Each sample observed the
daemon over five seconds with zero CPU ticks and zero voluntary
context-switch increments in every sample: median 0, range 0–0. This
passes the absolute idle gate at the counters' resolution: no wakeups,
no CPU. It establishes no throughput or busy-workload claim, and the
gate stays deferred until a host-ready run reproduces it.

This passes the idle CPU/wakeup regression gate at the counters' resolution.
It establishes no throughput, startup-latency, or busy-workload speedup.
The byte, descriptor, and memory storm checks remain separate functional
resource gates. Earlier samples attempted while readiness checks failed were
discarded and are not part of these results.
