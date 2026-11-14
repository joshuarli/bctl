# 9p ownership review

Reviewed at `a21c8882fbb110beb950e16426ca4a5cba6d875c`. Line references below are relative to `kernel/nanos/src/`. These are source-supported failure paths, not claims of reproduced guest failures. BCTL-17 remains open; the Rust ownership domain and its evidence gates in `rust-adoption.md` still apply.

## Findings

### Open unlink or rename-over leaves a dangling fsfile metadata pointer

`fs/9p.c:243-247` initializes an fsfile with the dentry's tuple; `fs/fs.c:1025-1030` stores that pointer in `f->md`. On successful unlink, `fs/9p.c:494-502` clears only `dentry->md`, detaches the dentry, and requests destruction even when `fsfile_refs` is nonzero. The generic caller removes the name and destroys the tuple at `fs/fs.c:797-803,527-532`. Rename over an existing target does the same at `fs/9p.c:473-481` and `fs/fs.c:883-893`. Neither clears the open fsfile's pointer. A subsequent successful write completion reads that pointer and updates its mtime at `fs/9p.c:135-148`; the update reads the tuple at `fs/fs.c:109-116`. Truncate also uses it at `fs/fs.c:312-321`.

Reproducer: open a regular file, unlink it (or rename another file over it), then write and force writeback or truncate through the still-open descriptor. The metadata update can dereference freed or reused storage. The callback can also race destruction; simply clearing the pointer without synchronizing with callbacks is insufficient.

Smallest regression: guest test both unlink and rename-over with an open target, then write, sync, truncate, and close it, checking contents through the descriptor and exercising delayed completion with a deterministic hook. Repair: keep metadata leased by each open file and in-flight callback until their last use, or detach all consumers under an audited synchronization contract before destruction; retain normal open-unlinked semantics.

### Reopen while last-close writeback is pending creates a second owner of an opened fid

`fs/9p.c:218-234` removes the zero-ref fsfile from the lookup list before asynchronous page-cache sync finishes; `kernel/pagecache.c:1290-1294,1839-1867` can initiate dirty-page writeback before completion. A relookup retains the dentry and its fid (`fs/9p.c:584-592`), but `fs/9p.c:507-531` creates another fsfile and page-cache node (`fs/fs.c:1016-1026`) while the old one still exists. Its lopen can return the explicitly anticipated `-EINVAL` for an already-open fid (`fs/9p.c:531-536`). `virtio/virtio_9p.c:208-214` writes `iounit` only on success; `fs/9p.c:237-254` does not initialize it, yet `fs/9p.c:541-542` reads it. `fs/9p.c:111-126` subsequently uses that value to split I/O. A nonzero indeterminate value can violate the negotiated transfer limit; a zero value is replaced by the default. Separate page-cache nodes also allow the new handle to read or dirty data independently of the old pending writeback.

Reproducer: dirty a file, close its last handle, hold the old writeback completion, and reopen the same path before it completes. Read or write overlapping offsets and release the old writeback after the new operation. The uninitialized transfer limit and stale/overwriting cache behavior follow from that interleaving; their particular visible outcomes depend on allocation and completion order.

Smallest regression: deterministically delay old writeback, reopen and perform overlapping read/write, then release completions in both orders and compare bytes with native Linux; check live node and fid counts. Repair: make closing a state with a single authoritative file/cache owner and a lease on the fid until operations complete; reopen must join or safely serialize with that state. Initialize and preserve the negotiated I/O limit, rather than treating an already-open response as a fresh open with an unset output.

### Fid-pressure eviction can free metadata still referenced by a parent directory

`fs/9p.c:51-65` evicts an unpinned dentry with no fsfile references and no watch when fid allocation is exhausted. `fs/9p.c:91-100` frees its metadata through `fs_cleanup_dir_entry`. That helper removes the parent's entry only if the tuple has a `..` pointer (`fs/fs.c:500-513`); `fs/fs.c:449-452` sets that pointer only for directories, and readdir likewise sets it only for `DT_DIR` (`fs/9p.c:399-405,418`). Thus a cached regular-file tuple in `children(parent)` can survive as a pointer after the eviction frees it. A later directory refresh reads the old child at `fs/9p.c:358-384` and reuses it at `fs/9p.c:399-418`, or cleanup processes it at `fs/9p.c:324-339,427-435`.

Reproducer: exhaust the fid allocator after caching and closing a regular file while its parent remains cached, causing that dentry to be selected for eviction, then refresh the parent. The normal allocator's large ID range makes this a fault-injection or reduced-capacity test, not a claim of an ordinary workload reproducing it.

Smallest regression: use a bounded test fid allocator, cache a regular child, force its eviction, then readdir and reopen it under memory reuse instrumentation. Repair: explicitly track and detach namespace/cache membership before metadata destruction, or retain metadata independently of fid eviction; do not rely on a directory-only backpointer.

### Failed clunk still returns a live server fid to the allocator

`fs/9p.c:71-75` ignores the result of `v9p_clunk` and immediately returns the numeric fid to the heap. `virtio/virtio_9p.c:659-673` can fail before sending the request on allocation failure, or return a transport/protocol error. In the former case the server definitely has not received the clunk, yet a later `p9_fid_new` can reuse the same number (`fs/9p.c:51-57`) in a walk (`fs/9p.c:575-580`). The server can then reject the walk because the fid remains in use; it may retain the old open resource. This also affects last-close teardown (`fs/9p.c:189-197`).

Smallest regression: inject allocation failure in clunk, force fid reuse, and check that no new walk uses the still-live identifier and that eventual cleanup is accounted for. Repair: distinguish confirmed release from failed or uncertain release, quarantine the identifier until the transport's contract permits reuse, and surface or handle failure without claiming exactly-once release.

## Repaired and unverified paths

The specific last-close free of a regular-file tuple while its parent still listed it was addressed by `aacbd732`: it removed immediate tuple destruction from `p9_fsf_free`. Current `fs/9p.c:189-198` retains a listed dentry and metadata when its last fsfile completes, releasing only its fid; lookup can reacquire a fid at `fs/9p.c:584-592`. The existing `kernel/tests/mini_chromium/fixtures/p9_close.c:15-37` exercises repeated close, stat, readdir and reopen; the historical commit reports a guest pass, which this review did not rerun. The eviction and overlapping-close findings above are different paths and do not negate that repair.

The raw file pointer captured by `p9_fsf_write_complete` (`fs/9p.c:135-161`) and the raw file captured by sync (`fs/9p.c:294-320,698-701`) warrant interleaving tests, but this review did not establish an independent use-after-free through every caller: page-cache and descriptor completion ownership may retain them. Likewise, `p9fsf_sync_complete` takes the filesystem lock and calls teardown (`fs/9p.c:202-215`), but a concrete lock cycle or late read completion after free was not established. These remain unverified, not additional confirmed defects. No runtime tests, fault injection, transport-disconnect analysis, or Rust boundary review were performed here.
