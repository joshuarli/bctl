# External differential corpus

Cases ported to plain C from gVisor's syscall tests (Apache-2.0) at commit
`975fb172241c7946a9f67b4f52e0349d9240589a`. Each binary prints one
`MINI_CHROMIUM_TEST` record per case and one `MINI_CHROMIUM_RESULT` line, so
`dev/run.sh` reads it natively (`--native`) and on Nanos unchanged. Each case
runs under a 10 s `SIGALRM` watchdog that records `detail=watchdog`.

Build: `make -C tests/external` (output in `.work/build/external/gvisor_<file>`).

Changes from upstream, none of which weakens an observation:

- No gtest, fixtures or `TempPath`; file cases use a scratch file in the working
  directory that is unlinked after opening (upstream `lseek.cc` reads
  `/etc/passwd`).
- `WaitTest.SignalExit` blocks the child on a pipe instead of `sleep(10)`.
- `ExecTest.CloexecNormalFile` execs the test binary itself in place of the
  Bazel workload `exec_assert_closed_workload`, with the same exit codes.
- `UnixSocketPairCmsgTest` cases run for `SOCK_STREAM` and `SOCK_SEQPACKET`
  only; upstream also runs the filesystem and abstract bound pairs and the
  reversed pairs.

Status columns: native is this host (Linux x86_64, musl, static), three runs in
a row all PASS. Nanos is base `392de056` kernel, 1 and 2 vCPUs.

| Upstream file:TestName | Commit | Slice | Native | Nanos smp1 | Nanos smp2 |
| --- | --- | --- | --- | --- | --- |
| `dup.cc:DupTest.DupClearsCloExec` | 975fb17 | G1.2 | PASS | FAIL | FAIL |
| `dup.cc:DupTest.Dup3` | 975fb17 | G1.2 | PASS | FAIL | FAIL |
| `fcntl.cc:FcntlTest.IndependentDescriptorFlags` | 975fb17 | G1.2 | PASS | FAIL | FAIL |
| `fcntl.cc:FcntlTest.DupAfterO_ASYNC` | 975fb17 | G1.2 | PASS | PASS | PASS |
| `lseek.cc:LseekTest.EtcPasswdDup` | 975fb17 | G1.2 | PASS | PASS | PASS |
| `fork.cc:ForkTest.Pipe` | 975fb17 | G1.2 | PASS | FAIL | FAIL |
| `exec.cc:ExecTest.CloexecNormalFile` | 975fb17 | G1.3 | PASS | FAIL | FAIL |
| `wait.cc:WaitTest.SignalExit` | 975fb17 | G1.2 | PASS | FAIL | FAIL |
| `wait.cc:WaitTest.WaitidWNOWAIT` | 975fb17 | G1.2 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicFDPass_stream` | 975fb17 | G2.1 | PASS | PASS | PASS |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicFDPass_seqpacket` | 975fb17 | G2.1 | PASS | PASS | PASS |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicThreeFDPass_stream` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicThreeFDPass_seqpacket` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BadFDPass_stream` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BadFDPass_seqpacket` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.ShortCmsg_stream` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.ShortCmsg_seqpacket` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.FDPassRecvEFAULT_stream` | 975fb17 | G2.1 | PASS | PASS | PASS |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.FDPassRecvEFAULT_seqpacket` | 975fb17 | G2.1 | PASS | PASS | PASS |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicFDPassNoSpaceMsgCtrunc_stream` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicFDPassNoSpaceMsgCtrunc_seqpacket` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicThreeFDPassTruncationMsgCtrunc_stream` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicThreeFDPassTruncationMsgCtrunc_seqpacket` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.CloexecRecvFDPass_stream` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.CloexecRecvFDPass_seqpacket` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicCredPass_stream` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicCredPass_seqpacket` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_peercred.cc:UnixSocketPeerCredTest.GetPeerCred_stream` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_peercred.cc:UnixSocketPeerCredTest.GetPeerCred_seqpacket` | 975fb17 | G2.1 | PASS | FAIL | FAIL |
| `socket_unix_peercred.cc:UnixSocketPeerCredTest.PeerCredSurvivesPeerExit` | 975fb17 | G2.1 | PASS | FAIL | FAIL |

Case names carry a `_stream` or `_seqpacket` suffix for the socket type. Slice `G1.3` is `execve` close-on-exec; the brief
listed only G1.2 and G2.1.

## Binary sha256 (static, `-O2`, pinned clang 23.1.5 + musl sysroot)

    df73f416cc3a7e7d799b0d3b5192dda94818e9cee5b9121bc5a85d763ca4b540  gvisor_dup
    87afdea58c99bf9dbe7213585f15233eceadf78ec1d168334fddbd00cc87d894  gvisor_exec
    df6b67bf2e82e8fbafd16b2abb3526032107c5526e0cda07a8b95d8b3e221b08  gvisor_fcntl
    08a2d90863536f02b697035969eda7ba370836262cc7e8811fb3aa56cf99ce5e  gvisor_fork
    a65e449dee40422f0b8e7642e7b9f6a1f2aa2c5e9281f7f4d35a6ab2e086b949  gvisor_lseek
    c837a116058f8f791ed5f7e51192732aafad3be663eab6cbcbba56dc95f9f667  gvisor_socket_unix_cmsg
    b4f30a4d7c77d7d778bf67815586e363da22bdf33a12cb42dfa13c3726809e4f  gvisor_socket_unix_peercred
    d337aec5746daa76fd6dd995339af81d494ae48dc712ddffb8d51ccb65541e08  gvisor_wait

## Considered and left out

- `socket_unix_cmsg.cc:BasicFDPassNullControlMsgCtrunc`: upstream skips it on
  gVisor (`IsRunningOnGvisor`) because of gvisor.dev/issue/207; the case is not
  among the behaviors the slices in flight implement and was not run natively.
- `socket_unix_cmsg.cc:FDPassPeekEFAULTNoControl`, `ZeroLengthRecvReceivesFD`,
  `MsgCtruncInputIsNoop`, `CloexecDroppedWhenFDPassed`, `FDPassNoRecv`,
  `FDPassInterspersed1/2`, `FDPassNotCoalesced`: ported-eligible but held back to
  keep the first batch at 20 upstream tests; first candidates for the next batch.
- `socket_unix_cmsg.cc:BasicCredPassDifferentPID`, `CredPassDifferentPIDUnprivileged`,
  `CredPassNonexistentPID`: need `CAP_SYS_ADMIN` or capability dropping
  (`AutoCapability`); the guest's capability model is not decided yet.
- `socket_unix_peercred.cc` PID-namespace tests (`peerCredNSConnectChild`): need
  `CLONE_NEWPID`, which the campaign does not implement.
- Truncation closes dropped descriptors (EOF on a pipe after `MSG_CTRUNC`): no
  upstream test at this commit observes it this way; `FDPassRecvEFAULT` is the
  only upstream case that checks pipe EOF after a failed receive. A bespoke case
  belongs in the probe, not in this upstream-ported bank.
- `dup.cc:Rlimit`, `DupLimitedByNROpenSysctl`: need `RLIMIT_NOFILE` enforcement
  and `/proc/sys/fs/nr_open`, outside the slices in flight.
- `fcntl.cc:SetFlags`: needs `O_DIRECT` on a regular file, which depends on the
  filesystem under the test directory, so it is not stable natively.
- `fork.cc` `SharedMapping`/`PrivateMapping`/`COWSegment`: memory semantics for
  the COW check in G1.2; belong with the next batch together with the clone
  tests.
- `wait.cc` parameterized `WaitSpecificChildTest`/`WaitAnyChildTest`: sleeping
  children and `clone` stacks; not ported until their sleeps can be replaced
  with pipe synchronization without changing what they observe.
