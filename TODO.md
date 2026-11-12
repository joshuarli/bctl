Prefix: BCTL
Next: 12

### BCTL-11 · P2 · Linux SmolVM guests lose module fetches to the host

- **Area:** Concurrency
- **Where:** `crates/bctl/tests/host_http.rs:113`, `runtime/patches/`, `third_party/smolvm/src/` (the guest network relay)
- **Problem:** On Linux x86_64 with KVM, `BCTL_TEST_RUNTIME=smolvm cargo test -p bctl --test host_http -- --ignored` failed 2 of 3 runs. The guest page lost 6 and 10 of 60 rounds whose module scripts never arrived over reused keep-alive connections to the host at `100.96.0.1`. macOS passed 24 of 24 at SmolVM 1.34.0. Headed browsing that loads many subresources from host services can stall the same way. Unproven whether the 1.34.1 update or the Linux relay is the cause.
- **Recommendation:** Capture one failing round at the relay (host-side connection states and the guest's TCP view), then fix the dropped or stalled stream in a patch and keep `host_http` as the regression test.
- **Fix with:** `root-cause`
- **Effort:** M (days)

### BCTL-9 · P2 · A macOS Nanos startup timeout can leave the libkrun runner running

- **Area:** Concurrency
- **Where:** `crates/runtime-nanos/src/supervisor_macos.rs:97`, `crates/runtime-nanos/src/supervisor_macos.rs:109`, `crates/runtime-nanos/src/supervisor_macos.rs:113`
- **Problem:** On its startup timeout `launch` sends one TERM to the supervisor's process group through `/bin/kill`, then kills and waits for only the supervisor. Nothing confirms the group is empty or escalates to KILL, so a runner that ignores TERM outlives the failed start, the same defect BCTL-4's fix removed from the Linux supervisor. Unproven: the module does not build on Linux and no macOS run was made.
- **Recommendation:** Build `process_group::OwnedGroup` for macOS as well and use it in the macOS `launch`, so a failed start terminates and reaps the whole group with bounded escalation. Port `crates/runtime-nanos/tests/failed_start.rs` to the macOS launcher and run it on an Apple Silicon host.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** S (hours)

### BCTL-10 · P3 · The web UI has no tone for paused sessions

- **Area:** Types
- **Where:** `web/src/api.ts:7`, `web/src/app.tsx:9`
- **Problem:** The daemon reports `paused` sessions (`crates/bctl/src/daemon.rs:806`), but `SessionView["state"]` omits `paused`, so the type is wrong and `stateTone` returns undefined for a paused session's badge.
- **Recommendation:** Add `paused` to the state union and a tone for it in `stateTone`, and check the badge renders for a paused session.
- **Fix with:** none
- **Effort:** S (hours)
