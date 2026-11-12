# Linux verification, 2026-11-12

This follows bctl's
[Linux results](../../bctl/docs/handoffs/2026-11-11-linux-x86-64-results.md).
The host is Linux x86-64 with KVM. The kernel is Hydrogen `f503d850`;
`bc164807` changes only the CPU fixture.

## CPU fixture repair

The pinned clang rejects `CPU_SET(cpu, &set)` in `cpus_online.c`:
musl's macro compares the signed `long` index with an unsigned `size_t`.
The build treats that warning as an error. `bc164807` converts the
nonnegative index to `size_t` at the macro boundary. The fixture builds
and passes with one and two guest CPUs.

## Regression results

- `dev/test.sh probe`: passes. The main probe passes 55 checks natively
  and at 1, 2, and 4 vCPUs. The raw probe passes six in each configuration.
- `VIRTIOFSD=/home/josh/d/bctl/.work/runtime/nanos/bin/virtiofsd dev/test.sh virtiofs`:
  passes at 1 and 2 vCPUs, including the host checks.
- The same `VIRTIOFSD` setting with `dev/test.sh harness`: fails one
  check, `tcp_loopback_rtt_nodelay0` at two CPUs. All other harness cases
  pass: 90 PASS lines, including `cpus_online` and virtio-fs, plus the
  silent host file checks.
  Receipt: `.work/runs/20261112T003157Z-tcp_loopback-smp2-18023`.

The failing TCP fixture reports a maximum request RTT of 66.1 ms for
2000 bytes, exceeding its 50 ms assertion. This is a single timing check,
not a demonstrated performance regression. The host load averages observed
afterward are 42.99, 42.06, and 33.90. Host scheduling and a kernel regression
remain possible causes. The threshold is unchanged and the failure is kept.
Repeat only after controlling host contention or adding evidence that
distinguishes scheduler delay from a guest TCP stall.

The initial harness run has five failures caused by test setup. Four are
the guest and host virtio-fs checks, because `virtiofsd` is absent from
`PATH`. The fifth is `hello_stdio` at two CPUs: a concurrent probe build
removed `kernel.img` while QEMU started. Its `qemu.log` reports that
missing file. Run builds and VM suites sequentially in this checkout.

Local suite logs are in `.work/tmp/linux-probe-resume.log`,
`.work/tmp/linux-virtiofs-resume.log`, and
`.work/tmp/linux-harness-resume.log`. The sequential harness result is
`.work/tmp/linux-harness-verified.log`. Failed receipts remain under
`.work/runs/`, including
`20261112T002834Z-hello_stdio-smp2-28553`.

## Parked stress diagnostic

The source is `lane/g4.2-stress` at `7d2c3a9b`, with `STRESS_CYCLES`
changed from 100 to 400 in `.work/tmp/mini_chromium_stress.c`.
The diagnostic binary uses the pinned guest clang and sysroot. Its
SHA-256 is
`3008d0f9851f9445e8f9fff7cb33702905c35127ac512e132e6eb610c1329653`.

Both runs use a 1 GiB guest and a 180-second deadline:

| vCPUs | Renderer launches | Processes before and after | Free memory drop, KiB | Receipt under `.work/runs/` |
| --- | --- | --- | --- | --- |
| 2 | 8 × 400, all succeed | 1 → 1 | 12408 | `20261112T002757Z-stress-resume-400-20076` |
| 4 | 8 × 400, all succeed | 1 → 1 | 12492 | `20261112T002824Z-stress-resume-400-smp4-26584` |

The historical hang does not reproduce in these runs. Its cause is still
unattributed. Later socket and TLB changes are not proven to have fixed it.
The old diagnostic prints a memory PASS using a placeholder allowance of
1073741824 KiB. That assertion does not establish memory recovery.

G4.2 remains open. Investigate the free-memory drop, replace the placeholder
allowance, and integrate the stress and repetition commands before accepting
the gate. The 9p metadata lifetime concern and bctl's intermittent shutdown
reply failure remain unverified. This session does not measure performance
or rerun browser interop.
