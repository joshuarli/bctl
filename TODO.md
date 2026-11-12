Prefix: HYD
Next: 3

### HYD-1 · P1 · External regression suite reports crashed and incomplete runs as passing

- **Area:** Tests
- **Where:** `dev/test.sh:323`, `dev/test.sh:335`, `dev/test.sh:339`
- **Problem:** `suite_external` discards the guest runner's exit status and overall verdict, then judges only the case records that exist. A crash before the first record produces zero passing cases, zero total cases, and no explicitly failing required name. The suite prints `PASS ... 0/0` and returns success. An incomplete list containing only passing cases has the same acceptance problem. At `e4aad9cc`, a bounded probe executed the unchanged suite function with controlled build and runner edges: the native run passed, both guest runs returned exit 1 with `verdict=CRASH` and `tests=[]`, and the suite printed two `PASS ... 0/0` lines with `failures=0` and exit 0. This proves the result adapter, not a new kernel crash.
- **Recommendation:** Reject crashed, timed-out, malformed, or incomplete attempts before considering expected per-case failures. Capture the successful native run's case names and require the guest attempt to account for the expected cohort. Keep explicit failure allowances separate from terminal failure and missing records. Verify empty-crash, partial-crash, timeout, complete-pass, and complete-known-failure outcomes through this adapter.
- **Fix with:** `test-audit`
- **Effort:** S (hours)

### HYD-2 · P2 · The run harness leaves child processes alive on timeout and startup failure

- **Area:** Concurrency
- **Where:** `dev/run.sh:11`, `dev/run.sh:82`, `dev/run.sh:104`
- **Problem:** The exit trap only removes the expected-case file. The deadline watchdog sends one TERM to the main child, after which the harness waits without escalation. Running the actual `run.sh --native --timeout 1` with a fixture that ignores TERM left both child and harness alive after three seconds, with the deadline marker present and no `result.json`. A second probe used unchanged launcher scripts and a `virtiofsd` fixture that never created its socket: the launcher returned `virtiofsd did not start` after its startup deadline while the helper remained alive. Both probes used isolated process groups and cleaned up their own children. These are two exits from the same incomplete child-ownership lifecycle.
- **Recommendation:** Install cleanup ownership when each child starts, including helper and watchdog processes. On failure, interruption, or timeout, terminate and reap every owned process with a bounded TERM-to-KILL escalation. Cover native descendants as well as the direct child. Preserve the original failure and write the final timeout result after bounded cleanup. Verify an ignored TERM and a helper failing before socket publication.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M (days)
