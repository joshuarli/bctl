# Failure modes

Each row is one failure injected during a live run. The suite is
`crates/bctl/tests/failures.rs`; every case is VM-backed and runs with
`cargo test -p bctl --test failures -- --ignored`. Every case ends in a named
state and leaves no VM, listening port, or input lease behind.

The named states reuse what already exists. Session `failed` at the live
generation means the session owns no VM; its `failure` string names the
appliance stage or the machine. Run `failed` carries the outcome
`browser-crashed` when the browser the run was using stopped answering CDP,
and `job-error` when the run's own work failed. A run stopped by its disk bound
also ends `failed` as `job-error`, with a detail naming the bound. No new
session or run state was added.

| Case | Injection | End state | Cleaned up |
|---|---|---|---|
| Kill the guest browser | `machine cp` a static busybox into the guest, then `kill -9` every `helium` process | run `failed` / `browser-crashed`, detail `browser exited unexpectedly: pid N is gone`; session `failed` with that cause | run process group killed; session's machine deleted; VNC port closed; authority reset to `automation`; CDP socket directory removed |
| Kill the compositor | same busybox, `kill -9` every `tinyland` process | run `failed` / `browser-crashed`, detail `compositor exited unexpectedly: signal: 9 (SIGKILL)`; session `failed` with that cause | as above |
| Kill the VM | `kill -9` the `pid` reported by `smolvm machine status --json` | run `failed` / `browser-crashed`, detail `machine bctl-<session>-<generation> stopped unexpectedly`; session `failed` with that cause | machine record deleted (or already gone); VNC port closed; authority reset; CDP socket directory removed |
| Kill the daemon | `kill -9` the daemon holding `daemon.lock`, then the next command starts a new daemon | run `interrupted` / `interrupted`; session `ready` at the same generation, adopted with its running VM | the restarted daemon stops the orphaned process group; `bctl down` deletes the adopted machine; VNC port closed; a fresh daemon means the old in-memory leases and CDP capabilities are gone |
| Fill the host disk with a run's output | an `exec` job appends past the run's 256 MiB output bound | run `failed` / `job-error`, detail `the run's output passed its 256 MiB bound; the job was stopped before it filled the disk`; session stays `ready` and the browser keeps answering | job process group killed; the failure and its output are retained for the failure-retention window, then storage GC removes both; `bctl down` deletes the machine |
| Fill the guest's writable disk | the copied busybox runs `dd if=/dev/zero of=/tmp/fill.bin bs=1M` in the container until `ENOSPC` | run stays `running` and session stays `ready`; the appliance and CDP survive the full disk; the test then cancels the run, which ends `cancelled` | `dd` exits at `ENOSPC`; the run's process group is killed by the cancel; `bctl down` deletes the machine and its 19.1 GiB overlay; VNC port closed; authority `automation` |
| Drop the viewer | a dashboard viewer takes control, opens the live view, then closes its WebSocket | the run keeps running under automation; the dropped viewer's lease is released and a second viewer can take control | no lease remains (`GET /api/sessions/S/control` is `automation`, `mine: false`); the run ends `cancelled` when the test cancels it; `bctl down` deletes the machine and closes the VNC port |
| Close the last tab | an external CDP client sends `Target.closeTarget` for the only page | run `failed` / `browser-crashed`, detail `browser exited unexpectedly: pid N is gone`; session `failed` with that cause | as for killing the browser |

## The last tab

The guest Chromium exits when its last page target closes. Evidence from a
Helium session: closing the only `page` target over CDP returns
`{"success":true}`, then `/json/list` refuses the connection and
`bctl-guest` logs `browser exited unexpectedly: pid 5 is gone`.

Decided: report a named state, do not keep a tab open. bctl's own
`bctl tab close` already refuses the last page target with `LAST_TARGET`, so
automation through bctl cannot end the browser. An external CDP client holds
the capability and speaks CDP unchanged, and the gateway deliberately does not
parse or rewrite CDP frames; blocking one method there would change the
automation contract and still lose against any other path that closes the
window. The robust behavior is the one implemented: the appliance names the
dead stage in its log, the daemon's health watch sees CDP stop answering, and
the session and its run reach the named states above.

## Retention and storage bounds

| Bound | Value | Why |
|---|---|---|
| Run log | 8 MiB per run | already set by J3; later output is discarded with a note |
| Run output | 256 MiB per run | artifacts and mirrors are small (the J3 verified mirror is a few pages); 256 MiB is several times the 94 MiB a first browser boot grew its profile by in H1, and far above a bounded crawl's output. A run that passes it is a full-disk failure, not a crawl |
| All run files | 2 GiB | about eight maximum-size failed runs, or a few dozen ordinary ones; the machine data directory H1 measured is 792 MiB and lives outside this budget |
| Failure retention | 7 days | a failed, cancelled, or interrupted run keeps its record, log, and output for a week for diagnosis and for `bctl resume` of an interrupted crawl. After that, expiry removes the record and files |

Storage GC runs at daemon start, after every run ends, and every 60 seconds. It
expires old failures, evicts the oldest ended runs until the total run
directory is under 2 GiB, and removes run directories no record names. Paused
runs are never evicted: they are resumable. Eviction is oldest first, so the
most recent failure stays readable longest. Unit tests cover expiry, eviction
order, the paused-run exemption, and orphan removal.

## The guest disk

On `helium` the container's writable layer is a 19.7 GiB overlay on `/dev/vda`:
`df -h /` inside the guest reports 19.1 GiB available. A 1 GiB `dd` takes
0.21 s (4.7 GB/s), and filling the rest takes 7.3 s at 2.6 GB/s before
`dd` exits with `No space left on device`. That is cheap enough to keep the
case rather than skip it.

A full guest writable layer does not fail the appliance: the browser profile is
a host virtio-fs mount, Chromium's shared memory is tmpfs, and `bctl-guest`'s
logs are on the 64 MiB `/run` tmpfs. So the case asserts the survived named
states (`running` and `ready`, with CDP still answering), then cancels the run
to `cancelled` and asserts the cleanup. There is no unknown or stuck state.

## Injecting a kill into the product image

`helium` has no shell by contract. The tests copy the static busybox from
SmolVM's runtime bundle into the guest with `machine cp`, then run it. This is
test-only fault injection; nothing is added to the image and no bctl CLI or
daemon contract changes. The guest-side result is the same signal a real
process death produces, and `bctl-guest` reports the stage that died.
