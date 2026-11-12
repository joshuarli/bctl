Prefix: BCTL
Next: 9

### BCTL-1 · P2 · Ordinary SmolVM shutdown immediately kills the guest workload

- **Area:** Concurrency
- **Where:** `crates/bctl/src/daemon.rs:874`, `crates/bctl/src/daemon.rs:1159`, `crates/runtime-smolvm/src/lib.rs:245`, `crates/runtime-smolvm/src/lib.rs:280`, `third_party/smolvm/src/cli/vm_common.rs:2844`, `third_party/smolvm/crates/smolvm-agent/src/workload.rs:219`
- **Problem:** Normal `bctl down` reaches `machine delete -f`. The vendored deletion path overrides the workload stop timeout with zero, and the guest agent consequently sends SIGKILL without first running the workload's graceful stop handler. For the Helium image the workload is `bctl-guest run`, while `/profile` remains mounted from a persistent host directory. Deletion therefore bypasses the guest's browser, compositor, and seat shutdown sequence even though its data outlives the VM. A probe using the unchanged workload module and real process identity and signal handling confirmed SIGKILL and an absent shutdown marker with the zero override. The normal-timeout control ran its TERM handler and exited 0. The public command path is source-traced; actual browser-profile loss was not measured in a VM.
- **Recommendation:** Complete a bounded graceful browser and guest stop before deleting the machine on ordinary `down`. Keep forced removal as an explicit fallback whose result the caller can distinguish. Align the deadline with the appliance's shutdown sequence, including inherited signal masks in headed children. Verify graceful handler completion and persisted browser state through `bctl down`, then verify the bounded fallback for an unresponsive guest.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M (days)

### BCTL-2 · P2 · Failed sessions release profile ownership before their VM is removed

- **Area:** Data
- **Where:** `crates/core/src/store.rs:34`, `crates/core/src/store.rs:200`, `crates/bctl/src/daemon.rs:302`
- **Problem:** `fail_session` transitions a Ready session to Failed before calling `runtime.remove`. Failed is outside the database's `one_writer_per_profile` index, so another session can acquire the same profile while the old VM is still stopping. If removal fails, the old VM can remain alive after ownership and its observed endpoints have been discarded. A probe executed the real schema and update statements: the second writer was rejected while the old row was Ready, then admitted immediately after the Failed transition, before any removal. The daemon ordering establishes that window; simultaneous browser writes were not exercised. Nanos image locking can reject a second disk opener, but it does not repair the daemon's ownership and recovery state.
- **Recommendation:** Retain a profile-owning state until VM termination is confirmed. Serialize failure cleanup with start and stop operations, and retain enough machine state to retry a failed removal. Test a blocked removal and an erroring removal while another start requests the profile. Only the successful teardown may release ownership.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M (days)

### BCTL-3 · P2 · The Linux Nanos pin omits completed boot and shutdown repairs

- **Area:** Dependencies
- **Where:** `runtime/build-nanos.sh:4`, `runtime/build-nanos.sh:22`, `runtime/build-nanos.sh:121`
- **Problem:** The Linux builder still exports Hydrogen `47bc02559361df7021afd6ddd74b91204c63a011`. The reviewed Hydrogen head, `e4aad9cc70c1a8ca2716fd0ffdc04930bf56c64c`, is four commits ahead, including two kernel repairs motivated by bctl loops: `5ece982a` faults page-cache read destinations for writing before copying into them, and `e4aad9cc` allocates locked-heap management closures through the locked heap. The Git comparison confirms that the pinned tree omits both changes. Updating the sibling Hydrogen checkout does not change this exported build. The integration gap is proven by the pin and commit ancestry; this review did not repeat the guest crash and hang reproductions.
- **Recommendation:** Qualify a Hydrogen revision containing both repairs with the existing bctl startup, shutdown, persistence, and external-client checks, then advance this pin and rebuild the kernel/image tuple. Record the tested consumer revision instead of treating the sibling repository's head as the installed kernel. Evaluate the arm64 pin separately on its supported host.
- **Fix with:** `blast-radius`
- **Effort:** S (hours)

### BCTL-4 · P2 · Failed Linux Nanos startup leaves QEMU and its helpers running

- **Area:** Concurrency
- **Where:** `crates/runtime-nanos/src/supervisor.rs:102`, `crates/runtime-nanos/src/supervisor.rs:121`, `crates/runtime-nanos/src/supervisor.rs:188`, `crates/runtime-nanos/src/lib.rs:267`, `crates/runtime-nanos/src/supervisor_lock.rs:29`
- **Problem:** The launcher starts the supervisor in a separate process group, but on its 30-second startup timeout it kills and waits for only the supervisor. The supervisor's QEMU, vsock backend, and file server survive. The adapter then removes their machine and socket directories. Supervisor death releases its lock, so the lock fallback cannot identify these descendants as an active supervisor to stop. A probe imported the unchanged supervisor, lock, profile, and egress modules and supplied executable fixtures: helpers published their expected markers while QEMU stayed alive without QMP. The real launch path failed after 30 seconds, with all three children still alive. The probe cleaned up its own process group; no real VM was booted.
- **Recommendation:** Terminate and reap the complete owned process group on failed launch, with bounded escalation. Give helpers cleanup ownership across early returns. Remove recovery directories only after termination is established. Reproduce a live QEMU process that never publishes QMP and assert no process or profile lock remains after the failed start.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M (days)

### BCTL-5 · P2 · Fragmented CDP response headers can fail a healthy session

- **Area:** Errors
- **Where:** `crates/runtime/src/lib.rs:73`, `crates/runtime/src/lib.rs:89`, `crates/bctl/src/daemon.rs:245`
- **Problem:** `version_browser` returns None when its first read lacks the complete HTTP header terminator, although the rest of a valid response may arrive in the next read. Both TCP and Unix-socket health probes use it. Three consecutive false failures reach the daemon's session-failure and removal path. A framing probe ran the original function with only the derive annotations removed and a decoder-call witness substituted: an unsplit response reached decoding, while all 70 split positions inside its valid headers returned after one read, leaving the remainder unread and never reaching decoding. The probe covers framing; JSON decoding and live-browser teardown remain untested.
- **Recommendation:** Continue reading until the header terminator exists, then parse the headers and complete body. Add a focused check using a valid version response split across reads and assert the returned browser version, including splits inside the terminator.
- **Fix with:** `error-model`
- **Effort:** S (hours)

### BCTL-6 · P2 · Cancellation can leave Grab verification running indefinitely

- **Area:** Concurrency
- **Where:** `crates/bctl/src/runs.rs:685`, `crates/bctl/src/runs.rs:710`, `crates/bctl/src/runs.rs:719`
- **Problem:** The verification loop records the cancellation time and sends TERM, but never checks an escalation deadline. Its KILL occurs only after `try_wait` reports exit. A verifier that ignores TERM therefore prevents cancellation, session stop, storage failure, or browser failure from completing the attempt. A probe extracted the unchanged `verify` and `spawn_held` functions and ran a real TERM-ignoring verifier: cancellation was still unfinished after 16 seconds and the only delivered signal was TERM. The fixture's own KILL allowed the loop to return Cancelled and reap the child. Logging and ledger effects were bounded shims; the production cancellation loop and subprocess launch were unchanged.
- **Recommendation:** Reuse the bounded termination policy already applied to the main job process. Escalate and reap a verifier after the grace interval, including wait-error exits, while preserving the intended attempt outcome. Verify a TERM-ignoring verifier through cancellation and session stop.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** S (hours)

### BCTL-7 · P2 · A failed Nanos image rebuild deletes the previous usable image

- **Area:** Build
- **Where:** `images/nanos-helium/build.sh:243`, `images/nanos-helium/build.sh:246`, `images/nanos-helium/build.sh:249`
- **Problem:** The pack step passes the final image path to Hydrogen's image builder before the replacement tuple is ready, and either pack failure cleanup deletes that path. A failed rebuild can therefore remove the previously usable `.img` while leaving its old kernel and profile template. A probe executed the unchanged pack/publish block with an existing image, kernel, and template. The image helper preserved the old output, and the next mkfs failed. The block exited 1, deleted the old image, and preserved the other two old artifacts. This directly proves loss on failure without requiring a VM or browser archive.
- **Recommendation:** Build the intermediate and final replacement images under separate temporary names. Keep the previous published artifacts until every pack operation succeeds, and clean up only the new temporary artifacts on failure. Verify a failure at each pack step leaves the previous image tuple usable and unchanged.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** S (hours)

### BCTL-8 · P2 · The committed SmolVM source fails its own import identity check

- **Area:** Build
- **Where:** `third_party/UPSTREAM.lock:5`, `scripts/update_smolvm.py:491`, `scripts/update_smolvm.py:621`, `scripts/update_smolvm.py:792`
- **Problem:** At `a098bbbe`, a clean tree matching every committed blob and executable bit hashes to `sha256:4261c3201f7d8bb22796739355e1e4e68d77899678189c45c59021aa67f18d3e`, but the lock records `sha256:44d7e0aa11f446be58c2000c7f6f034b4cd7e70294ee98052587163daf20e896`. Calling the real `existing_source` function raises `existing vendored source differs from UPSTREAM.lock`. There are no extra files under the vendor tree. `import_source` performs this check before fetching or staging an update, so the documented updater refuses the committed checkout. The host build checks patch digests and applicability but does not run this whole-tree identity check.
- **Recommendation:** Reconcile the committed vendor tree, ordered patch set, and lock identity from one clean authoritative import. Preserve required local changes in the patch inputs before updating the identity. Make the existing build/import validation share the same definition of the source it accepts, and verify that the committed checkout passes `existing_source` before an update starts.
- **Fix with:** `single-source-of-truth`
- **Effort:** S (hours)
