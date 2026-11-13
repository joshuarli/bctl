Prefix: HYD
Next: 5

### HYD-3 · P2 · A run queue can hold a null scheduler task

- **Area:** Concurrency
- **Where:** `nanos/src/kernel/schedule.c:333`, `nanos/src/kernel/schedule.c:349`
- **Problem:** The parallel bctl up/down loops at `e4aad9cc` crash about once per 2,300 cycles with a page fault at address `0x10` in `sched_sort`, reached from `sync_complete` → `sched_enqueue` → `pqueue_insert`: the priority queue compares a null element. Earlier captures showed the same null task through `sched_dequeue`. It survived the locked-heap management repair, so it is not that double allocation; the writer is unknown. Evidence is in `../bctl/.work/tmp/par-3d97a9b0-1/diagnostics/live-up-516.log`.
- **Recommendation:** `0523dbbc` moved CPU 0's run queue, whose vector reallocated from the unlocked general mcache, and every other post-SMP allocation a debug detector found there to the locked heap; the next 2,400 loop cycles had no failure. At the old rate that is about a one-in-three chance even without a repair, so run at least 5,000 more cycles on the pinned kernel: close this if none fails, otherwise add a debug check that halts when a null or freed task enters a run queue and fix what it names.
- **Fix with:** `root-cause`
- **Effort:** M (days)

### HYD-4 · P2 · A browser can outlive SIGKILL at guest shutdown

- **Area:** Concurrency
- **Where:** `nanos/src/unix/signal.c:349`, `nanos/src/unix/syscall.c:542`
- **Problem:** Twice in about 1,500 parallel bctl up/down cycles (Hydrogen `e0db91ef` and the control `86e1c394`, which lacks `5ece982a`), the guest logged that the browser ignored `Browser.close` and `SIGTERM`, then sent `SIGKILL`; the browser never exited, both CPUs idled in `kernel_sleep`, and the host's 18 s stop read timed out. A thread stuck in an uninterruptible wait, such as a file read whose completion never runs, fits the symptom; unproven, and it did not recur in 2,300 cycles with the `debug/kill-watch` probe.
- **Recommendation:** Keep the `debug/kill-watch` probe, which prints each thread's syscall and wait 3 s after a `SIGKILL`, in the long loops until it fires, then fix the wait it names with a fixture that reproduces it.
- **Fix with:** `root-cause`
- **Effort:** M (days)
