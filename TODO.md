Prefix: BCTL
Next: 9

### BCTL-3 · P2 · The Linux Nanos pin omits completed boot and shutdown repairs

- **Area:** Dependencies
- **Where:** `runtime/build-nanos.sh:4`, `runtime/build-nanos.sh:22`, `runtime/build-nanos.sh:121`
- **Problem:** The Linux builder still exports Hydrogen `47bc02559361df7021afd6ddd74b91204c63a011`. The reviewed Hydrogen head, `e4aad9cc70c1a8ca2716fd0ffdc04930bf56c64c`, is four commits ahead, including two kernel repairs motivated by bctl loops: `5ece982a` faults page-cache read destinations for writing before copying into them, and `e4aad9cc` allocates locked-heap management closures through the locked heap. The Git comparison confirms that the pinned tree omits both changes. Updating the sibling Hydrogen checkout does not change this exported build. The integration gap is proven by the pin and commit ancestry; this review did not repeat the guest crash and hang reproductions.
- **Recommendation:** Qualify a Hydrogen revision containing both repairs with the existing bctl startup, shutdown, persistence, and external-client checks, then advance this pin and rebuild the kernel/image tuple. Record the tested consumer revision instead of treating the sibling repository's head as the installed kernel. Evaluate the arm64 pin separately on its supported host.
- **Fix with:** `blast-radius`
- **Effort:** S (hours)
