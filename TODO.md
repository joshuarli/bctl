Prefix: BCTL
Next: 9

### BCTL-1 · P2 · Ordinary SmolVM shutdown immediately kills the guest workload

- **Area:** Concurrency
- **Where:** `crates/bctl/src/daemon.rs:874`, `crates/bctl/src/daemon.rs:1159`, `crates/runtime-smolvm/src/lib.rs:245`, `crates/runtime-smolvm/src/lib.rs:280`, `third_party/smolvm/src/cli/vm_common.rs:2844`, `third_party/smolvm/crates/smolvm-agent/src/workload.rs:219`
- **Problem:** Normal `bctl down` reaches `machine delete -f`. The vendored deletion path overrides the workload stop timeout with zero, and the guest agent consequently sends SIGKILL without first running the workload's graceful stop handler. For the Helium image the workload is `bctl-guest run`, while `/profile` remains mounted from a persistent host directory. Deletion therefore bypasses the guest's browser, compositor, and seat shutdown sequence even though its data outlives the VM. A probe using the unchanged workload module and real process identity and signal handling confirmed SIGKILL and an absent shutdown marker with the zero override. The normal-timeout control ran its TERM handler and exited 0. The public command path is source-traced; actual browser-profile loss was not measured in a VM.
- **Recommendation:** Complete a bounded graceful browser and guest stop before deleting the machine on ordinary `down`. Keep forced removal as an explicit fallback whose result the caller can distinguish. Align the deadline with the appliance's shutdown sequence, including inherited signal masks in headed children. Verify graceful handler completion and persisted browser state through `bctl down`, then verify the bounded fallback for an unresponsive guest.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M (days)

### BCTL-3 · P2 · The Linux Nanos pin omits completed boot and shutdown repairs

- **Area:** Dependencies
- **Where:** `runtime/build-nanos.sh:4`, `runtime/build-nanos.sh:22`, `runtime/build-nanos.sh:121`
- **Problem:** The Linux builder still exports Hydrogen `47bc02559361df7021afd6ddd74b91204c63a011`. The reviewed Hydrogen head, `e4aad9cc70c1a8ca2716fd0ffdc04930bf56c64c`, is four commits ahead, including two kernel repairs motivated by bctl loops: `5ece982a` faults page-cache read destinations for writing before copying into them, and `e4aad9cc` allocates locked-heap management closures through the locked heap. The Git comparison confirms that the pinned tree omits both changes. Updating the sibling Hydrogen checkout does not change this exported build. The integration gap is proven by the pin and commit ancestry; this review did not repeat the guest crash and hang reproductions.
- **Recommendation:** Qualify a Hydrogen revision containing both repairs with the existing bctl startup, shutdown, persistence, and external-client checks, then advance this pin and rebuild the kernel/image tuple. Record the tested consumer revision instead of treating the sibling repository's head as the installed kernel. Evaluate the arm64 pin separately on its supported host.
- **Fix with:** `blast-radius`
- **Effort:** S (hours)
