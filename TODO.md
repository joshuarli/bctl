Prefix: HYD
Next: 4

### HYD-3 · P2 · A run queue can hold a null scheduler task

- **Area:** Concurrency
- **Where:** `nanos/src/kernel/schedule.c:333`, `nanos/src/kernel/schedule.c:349`
- **Problem:** The parallel bctl up/down loops at `e4aad9cc` crash about once per 2,300 cycles with a page fault at address `0x10` in `sched_sort`, reached from `sync_complete` → `sched_enqueue` → `pqueue_insert`: the priority queue compares a null element. Earlier captures showed the same null task through `sched_dequeue`. It survived the locked-heap management repair, so it is not that double allocation; the writer is unknown. Evidence is in `../bctl/.work/tmp/par-3d97a9b0-1/diagnostics/live-up-516.log`.
- **Recommendation:** Find the writer before changing code: add a debug check that halts when a null or freed task is enqueued or found in a run queue, with the enqueuing caller, and rerun the loops; then fix the lifetime or locking defect it names, with a fixture that reproduces it.
- **Fix with:** `root-cause`
- **Effort:** M (days)
