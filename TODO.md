Prefix: HYD
Next: 3

### HYD-2 · P2 · The run harness leaves child processes alive on timeout and startup failure

- **Area:** Concurrency
- **Where:** `dev/run.sh:11`, `dev/run.sh:82`, `dev/run.sh:104`
- **Problem:** The exit trap only removes the expected-case file. The deadline watchdog sends one TERM to the main child, after which the harness waits without escalation. Running the actual `run.sh --native --timeout 1` with a fixture that ignores TERM left both child and harness alive after three seconds, with the deadline marker present and no `result.json`. A second probe used unchanged launcher scripts and a `virtiofsd` fixture that never created its socket: the launcher returned `virtiofsd did not start` after its startup deadline while the helper remained alive. Both probes used isolated process groups and cleaned up their own children. These are two exits from the same incomplete child-ownership lifecycle.
- **Recommendation:** Install cleanup ownership when each child starts, including helper and watchdog processes. On failure, interruption, or timeout, terminate and reap every owned process with a bounded TERM-to-KILL escalation. Cover native descendants as well as the direct child. Preserve the original failure and write the final timeout result after bounded cleanup. Verify an ignored TERM and a helper failing before socket publication.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M (days)
