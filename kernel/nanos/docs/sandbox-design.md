# Sandbox design: Chromium IPC (G2) and the renderer sandbox (G3)

Design for gates G2 and G3. Paths are relative to `nanos/`; `file:line` is the
tree at hydrogen `5e25afca`, before G1 lands. Types from G1 are the ones
`docs/chromium-compat.md` proposes: `address_space` (A1), `process_vm`
(E4: vmaps, `futices`, `as`, shared under `CLONE_VM`), the process
refcount, `ppid` and zombie state (G1.1/E1), the fd table shared under
`CLONE_FILES` (G1.2) and `FD_CLOEXEC` moved into the fd-table slot (D2).
`mini_chromium.c` is `../tests/mini_chromium/mini_chromium.c`; "contract"
is `../docs/chromium-contract.md`.

## Verdict

**Feasible. No hard blocker.** About 7,700 LoC including about 2,950 LoC
of runtime tests, in seven slices. Five structural changes carry most of
the risk:

1. `AF_UNIX` carries no control data at all. `unixsock_sendmsg` ignores
   `msg_control` and `flags` (`src/unix/socket.c:927-940`), and
   `unixsock_recvmsg` never writes `msg_controllen` or `msg_flags`
   (`socket.c:942-961`). Messages need a kernel-side control record.
2. There are two syscall dispatchers, not one: `syscall_handler`
   (`src/unix/syscall.c:2556-2608`) and `strace_syscall`
   (`klib/strace.c:607-686`, installed through the `syscall` pointer at
   `klib/strace.c:743`). Seccomp needs both to call one shared function.
3. Path lookup always starts absolute paths at the global root, and stops
   `..` only there (`src/fs/fs.c:1206-1219`, `1172-1173`). A per-process
   root has to be threaded into that walk.
4. `/proc` is a set of static tuples in the root filesystem
   (`src/unix/special.c:261-275`, `390-426`). Per-process status, `fd/`,
   `task/`, `ns/` and the id maps need a real procfs with dynamic lookup,
   following the 9p pattern (`src/fs/9p.c:558-600`).
5. Credentials are two `u32`s (`src/unix/unix_internal.h:521`,
   `src/unix/unix.c:589-590`). There are no capabilities, no namespaces and
   no per-process root or pid translation.

### Findings against plan.md and the contract

| # | Finding | Evidence | Recommendation |
| --- | --- | --- | --- |
| F1 | Linux never validates seccomp return actions at load time, so returning `EINVAL` for `KILL`/`TRACE`/`LOG`/`USER_NOTIF` (Decisions, "Contract-driven scope") is not Linux behaviour. The probe's own filter returns `SECCOMP_RET_KILL_PROCESS` from its arch and x32 checks (`mini_chromium.c:786-791`). The G3.2 row of the plan lists `KILL_THREAD`/`KILL_PROCESS`, so the plan contradicts itself. | Linux `seccomp_check_filter` checks opcodes only. Unknown actions run as `KILL_PROCESS` | Accept any `RET` value at load. Run `KILL_*` (cheap once G1.4 exists). `TRACE` → `-ENOSYS` (no tracer). `LOG` → allow. `USER_NOTIF` → `-ENOSYS` (no listener can exist). Unsupported **flags** (`NEW_LISTENER`, `LOG`, `WAIT_KILLABLE_RECV`) → `EINVAL` |
| F2 | The probe check `proc_status` cannot pass at G3.1: it requires `Seccomp: 2` | `mini_chromium.c:954-955` | Move it to G3.2's acceptance |
| F3 | The G3.3 checks `userns_map`, `userns_caps_scoped` and `fs_restricted` are emitted only by `check_zygote_sandbox`. That check creates the zygote with `CLONE_NEWUSER\|CLONE_NEWPID\|CLONE_NEWNET` and the renderer with `CLONE_NEWPID` | `mini_chromium.c:1334`, `1294`, `1383-1398` | Accept G3.3 through `test/runtime/userns`. All zygote checks go to G3.4 |
| F4 | The G2.3 checks `pipe_xproc` and `socket_shutdown` do not exist in the probe | `rg` over `mini_chromium.c`; the checks run are listed at `1421-1441` | Accept G2.3 through `test/runtime/ipc_xproc` plus `epoll_eventfd` |
| F5 | `ptrace_denied` (plan G3.4) is a seccomp `ERRNO` result in `check_browser_renderer` and passes at G3.2 | `mini_chromium.c:941-945`, `1060` | Move it to G3.2 |
| F6 | The plan says a PID namespace makes `/proc` show only that namespace. On Linux it does not unless `/proc` is remounted, and Chromium never changes the mount namespace (contract §2). The single procfs stays in root-namespace numbering, as on Linux. `NSpid` carries the per-level ids | contract §2 "The mount namespace is never changed" | Keep procfs in root-namespace pids. The renderer cannot reach `/proc` after the chroot anyway |
| F7 | The plan wants per-netns abstract `AF_UNIX` names. Nanos has no abstract namespace: `unixsock_get_path` takes `sun_path` as a C string, so an abstract name is an empty path. The contract does not use abstract sockets | `socket.c:396-407`; contract §7 item 8 | Drop it from G3.4. With no abstract namespace there is nothing to scope |
| F8 | `TSYNC` must really work, not only be accepted. Helium's GPU process installs its filter with `TSYNC` (G5) | contract "TSYNC" row, `gpu_main.cc:528` | Implement full `TSYNC` in G3.2 |
| F9 | `MSG_DONTWAIT` is ignored on `AF_UNIX`, but Helium issues 1,438 `recvmsg(…, MSG_DONTWAIT)`. A blocking socket would block | `socket.c:879-961`; `strace-full.txt` | G2.1 |
| F10 | A `SOCK_SEQPACKET` message is capped at `PAGESIZE`, so larger ones get `EMSGSIZE`. Chromium allows zygote messages up to 64 KiB. The largest `sendmsg` observed was 1,808 B | `socket.c:11`, `294-301`; `content/common/zygote/zygote_commands_linux.h:27` | Raise the limit to `so_rcvbuf` in G2.1 (a risk, not a blocker) |
| F11 | `wake_robust_list` has no loop limit and no owner check. A cyclic user list hangs the exiting thread in the kernel (Charter: egregious, a hang from user input) | `src/unix/futex.c:340-372` | Fix in its own commit in G2.2 (`ROBUST_LIST_LIMIT` 2048; check owner tid; `cmpxchg`) |
| F12 | `/proc/<pid>/cmdline` (read by G5's `guest_init`) would read another process's user memory. That breaks the Charter rule | `../tests/helium/guest_init.c:669` | G1.3 keeps the kernel copy of argv it makes before teardown (D1). procfs serves that copy |
| F13 | Line drift in plan.md: `setgroups`/`setuid`/`setgid`/`capset` are `syscall_ignore` at `syscall.c:2489-2493`, not 2460-2464. `prctl` returning 0 is at `syscall.c:2347`, not 2304 (that line is now `wait4`) | `src/unix/syscall.c` | Cosmetic |
| F14 | Contract gap: G5's `guest_init` needs `readdir("/proc")`, `/proc/<pid>/{cmdline,status,root}` and `readlink /proc/<pid>/ns/*`. These are not in contract §7 | `../tests/helium/guest_init.c:656-720`, `980-1003` | Covered by the procfs design (G3.1, G3.3) |
| F15 | Plan G2.3 owns no `syscall.c`, but `membarrier` (contract §7 item 10) needs a registration there. G2.2 needs the `F_ADD_SEALS` call site (`syscall.c:2080-2084`). New files (`procfs.c`, `seccomp.c`, `namespace.c`) need entries in `platform/{pc,virt,riscv-virt}/Makefile` (`platform/pc/Makefile:77-78`) | — | Add those files to the slices' Owns lists |

## 1. G2 inventory

### What already holds across processes

| Facility | Evidence | Note |
| --- | --- | --- |
| Socket, pipe and eventfd data copies run in the owning context | `socket.c:161-276` (read, contextual), `429-460` (write); G1 note §3 | Sender bytes are copied in the sender's context and receiver bytes in the receiver's. SCM data follows the same rule |
| `socketpair` for `STREAM`, `DGRAM` and `SEQPACKET` | `socket.c:46-57`, `1035-1062` | Message boundaries are kept for `SEQPACKET` (`socket.c:247-262`) |
| `EPIPE` on a write to a closed peer | `socket.c:446-449`, `src/unix/pipe.c:207` | No `SIGPIPE` anywhere (`rg SIGPIPE`: only the default action at `src/unix/signal.c:1039`) |
| memfd plus seals; `F_SEAL_SHRINK`/`GROW` on `ftruncate`; write seals on `write` and on a shared writable `mmap` | `klib/shmem.c:12-35`, `src/unix/syscall.c:1283-1288`, `611-616`, `src/unix/mmap.c:1363-1371` | `seal_enforcement` works once fds cross |
| A read-only fd refuses a writable `MAP_SHARED` (`EACCES`) | `file_perms` `src/unix/unix_internal.h:645-654`, used at `mmap.c:1356` | Needed for the `/proc/self/fd/N` read-only reopen |
| Cross-process `MAP_SHARED` of a file: shared pages are the page-cache page | G1 note §1 (`src/unix/mmap.c:245-275`) | After G1's A7 (page-cache mappings per address space) |
| `eventfd2` rejects unknown flags with `EINVAL`, so `eventfd2(0, ~0)` fails | `src/unix/eventfd.c:181-183` | — |
| `memfd_create("", ~0)` gives `EINVAL` | `klib/shmem.c:5-6,14-15` | Only when the `tmpfs` and `shmem` klibs are loaded (`../dev/test.sh:110`); the Helium image must load them too |
| futex `WAIT`, `WAKE`, `CMP_REQUEUE`, `WAKE_OP`, `WAIT_BITSET` | `src/unix/futex.c:176-306` | Keyed per process by VA only |

### Gaps

| Contract item | Nanos today | Wrong or missing | Smallest change | Slice |
| --- | --- | --- | --- | --- |
| `SCM_RIGHTS` send | `unixsock_sendmsg` → `f.writev` (`socket.c:927-940`) | `msg_control` ignored | Parse cmsgs in the sender context (`scm_parse`, below). Take a `fdesc` ref per fd via `fdesc_get` (`unix_internal.h:705-713`). Attach to the message | G2.1 |
| `SCM_RIGHTS` receive, `MSG_CTRUNC`, `MSG_CMSG_CLOEXEC` | `unixsock_recvmsg` (`socket.c:942-961`); `unixsock_read_bh` (`socket.c:161-276`) | No control output; `msg_controllen`/`msg_flags` never written | In the receiver's `unixsock_read_bh` context: emit `SCM_CREDENTIALS` first, then install fds with the receiver's fd allocator (the `accept_bh` pattern, `socket.c:757-759`). Drop the ones that do not fit and set `MSG_CTRUNC` | G2.1 |
| up to 128 fds per message (Mojo), 16-fd receive buffers | — | — | `SCM_MAX_FD` = 253 per message, else `EINVAL`. Fds beyond the receive buffer are released, with `MSG_CTRUNC` | G2.1 |
| malformed `cmsg_len` | — | — | `EINVAL` when `cmsg_len < sizeof(cmsghdr)` or it overruns `msg_controllen` (`scm_rights_negative`, `mini_chromium.c:701-710`); `msg_controllen > INT_MAX` → `ENOBUFS`; unknown `SOL_SOCKET` type → `EINVAL`; other levels skipped | G2.1 |
| bad fd in `SCM_RIGHTS` | — | — | `EBADF`; refs already taken are released | G2.1 |
| stream needs ≥ 1 payload byte | `unixsock_write_check` returns 0 for a zero-length stream write (`socket.c:296-297`) | — | Matches Linux: control data is dropped with a zero-length stream write | — |
| stream message boundaries with rights | the stream read merges messages (`socket.c:263`) | Fds could be attributed to the wrong read | A stream read stops after a message that carries rights. It also stops before a message whose credentials differ while `SO_PASSCRED` is set (Linux `unix_stream_read_generic`) | G2.1 |
| in-flight fd lifetime | `unixsock_dealloc` frees the queue without draining it (`socket.c:115`, `1016`) | Messages and their fd refs leak | Drain under the lock into a local list, unlock, then `fdesc_put` each ref (the peer may be one of the fds) | G2.1 |
| `SO_PASSCRED` set/get | `unixsock_setsockopt`: only `SO_REUSEADDR`, else `EOPNOTSUPP` (`socket.c:820-838`); getsockopt only `SO_TYPE`, else `ENOPROTOOPT` plus `msg_err` (`socket.c:841-877`) | Crashpad's call fails | Per-socket `passcred`; `getsockopt(SO_PASSCRED)` returns it | G2.1 |
| implicit `SCM_CREDENTIALS` | — | — | At send, if either end has `passcred`, record the sender: process ref, real uid and gid (Linux `unix_maybe_add_creds` uses `current_uid_gid`, the real ids). Explicit `SCM_CREDENTIALS` is checked: pid = own tgid, uid ∈ {r,e,s}, gid ∈ {r,e,s}, unless `CAP_SYS_ADMIN`/`CAP_SETUID`/`CAP_SETGID`; else `EPERM` | G2.1 |
| pid translated into the receiver's PID namespace | — | — | At receive: `pid = process_visible_pid(sender, receiver)`, 0 if not visible or reaped. Before G3.4 that is `sender->pid`. G3.4 swaps the body for namespace translation. Uid and gid are mapped into the receiver's user namespace (overflow id 65534) | G2.1, G3.4 |
| `SO_PEERCRED` | getsockopt falls to `ENOPROTOOPT` (`socket.c:871-874`) | — | Snapshot at `socketpair` (both ends get the creator), at `listen` (the listener) and at `connect_bh` (`socket.c:644-680`: the connector gets the listener's, the accepted end gets the connector's): process ref, effective uid and gid. Translate at `getsockopt` | G2.1 |
| `MSG_NOSIGNAL`, `MSG_DONTWAIT`, `MSG_TRUNC` | `unixsock_sendto`/`sendmsg`/`recvmsg` ignore `flags` (`socket.c:879-961`); `sendto_prepare` is netsock-only (`src/net/netsyscall.c:1817-1840`) | F9; `MSG_TRUNC` never reported | Bind `flags` into the read and write closures: `MSG_DONTWAIT` gives `EAGAIN` like `SOCK_NONBLOCK`. A truncated `SEQPACKET`/`DGRAM` read sets `msg_flags \|= MSG_TRUNC` and with `MSG_TRUNC` in `flags` returns the full length. Unknown send flags → `EOPNOTSUPP` (`MSG_OOB`, `MSG_EOR` on `SEQPACKET` accepted) | G2.1 |
| `SIGPIPE` | none | `EPIPE` without `SIGPIPE` | Send `SIGPIPE` to the writing thread on `EPIPE` unless `MSG_NOSIGNAL`, from `socket.c:446-449` and `pipe.c:207` | G2.3 |
| `shutdown(SHUT_RD/WR/RDWR)` | no `sock.shutdown` for `AF_UNIX`, so `shutdown` gives `EOPNOTSUPP` (`netsyscall.c:1343-1353`) | Chromium's sandbox IPC (`content/browser/sandbox_host_linux.cc:30-46`) and Mojo (`SHUT_RDWR`) fail | `unixsock_shutdown`: `how` ∉ {0,1,2} → `EINVAL`. Set own `shut_rd`/`shut_wr` and the peer's opposite. Reads at `shut_rd` with an empty queue return 0. Writes at `shut_wr` give `EPIPE`. Wake both blockqs | G2.3 |
| `EPOLLRDHUP`/`EPOLLHUP` | `unixsock_events` sets `HUP` only when the peer is gone (`socket.c:523-556`) | No `RDHUP` | `EPOLLRDHUP\|EPOLLIN` when the peer shut its write side or is gone; `EPOLLHUP` when both directions are shut | G2.3 |
| memfd flags | `MEMFD_KNOWN_FLAGS` = `CLOEXEC\|ALLOW_SEALING`; `MFD_CLOEXEC` is accepted but not applied (`klib/shmem.c:28` passes `O_RDWR\|O_TMPFILE`) | The fd leaks across `execve` | Apply `O_CLOEXEC`. Add `MFD_NOEXEC_SEAL` (`0x8`): seals = `F_SEAL_EXEC` (`0x20`, added to `FS_KNOWN_SEALS`, `src/unix/filesystem.c:8-9`), and it implies sealing. Name > 249 bytes → `EINVAL` | G2.2 |
| `F_ADD_SEALS` on a read-only fd | `fcntl` passes the fsfile only (`syscall.c:2080-2084`) | Linux gives `EPERM` | `fsfile_add_seals(file f, …)` checks `fdesc_is_writable` | G2.2 |
| `F_SEAL_WRITE` with another process's writable map | checks only `current->p` vmaps (`filesystem.c:1040-1047`) | Misses other address spaces | Walk the node's page-cache mapping list (G1 A7), counting `SHARED` vmaps whose `allowed_flags` include `WRITABLE` → `EBUSY` | G2.2 |
| `open("/proc/self/fd/N", O_RDONLY)` of a memfd | no `/proc/self/fd` | — | procfs magic link that reopens: a new `file` on the same `fs`, inode and fsfile (`fsfile_reserve`) with the requested access mode. A later writable `MAP_SHARED` gets `EACCES` from `file_perms` | G3.1 |
| shared futex keys | per-process table keyed by VA; `FUTEX_PRIVATE_FLAG` dropped (`futex.c:12-53`, `167`) | Cross-process wake misses (`futex_xproc`) | See below | G2.2 |
| `FUTEX_REQUEUE`, `FUTEX_WAKE_BITSET`, the bitset value | ops 3 and 10 fall to `ENOSYS` (`futex.c:308-312`); `WAIT_BITSET` ignores `val3` (`futex.c:291-306`) | — | `REQUEUE` = `CMP_REQUEUE` without the compare. Store the bitset in the waiter's closure; `WAKE_BITSET` wakes waiters whose bitset intersects. Bitset 0 → `EINVAL` | G2.2 |
| robust lists | `wake_robust_list` (`futex.c:340-372`), called even when the head is NULL (`src/unix/thread.c:506`) | F11; `OWNER_DIED` set without the owner check | Skip a NULL head. At most 2048 entries. Act only when `(*uaddr & FUTEX_TID_MASK) == tid`: `cmpxchg` to `(old & FUTEX_WAITERS) \| FUTEX_OWNER_DIED`, and wake one if `WAITERS` was set. Wake through the shared key when the word lies in a shared mapping | G2.2 |
| `membarrier(PRIVATE_EXPEDITED)` | `SYS_membarrier` is defined (`src/x86_64/unix_syscalls.h:325`) but unregistered (`ENOSYS`) | contract §7 item 10 | `QUERY` → `PRIVATE_EXPEDITED\|REGISTER_PRIVATE_EXPEDITED` (`8\|16`). `REGISTER_PRIVATE_EXPEDITED` sets `process_vm->membarrier_registered`. `PRIVATE_EXPEDITED`: unregistered → `EPERM`; else an empty broadcast through `src/kernel/flush.c:152-215`, which completes only after every CPU has taken the IPI. `flags != 0` or other commands → `EINVAL` | G2.3 |
| epoll, eventfd and poll across processes | blockq actions are contextual; the notifier writes into the waiter's buffer (`src/unix/poll.c:391-430`) | — | G1.1 commit 2 (A8) fixes it. G2.3 only tests it | G2.3 |

### SCM data structures (G2.1)

```c
typedef struct unix_cred {
    process p;
    u32 uid, gid;
} *unix_cred;

typedef struct unixsock_scm {
    struct unix_cred cred;
    boolean has_cred;
    u16 nfds;
    fdesc fds[];
} *unixsock_scm;

typedef struct unixsock_msg {
    buffer b;
    struct sockaddr_un from_addr;
    unixsock_scm scm;
} *unixsock_msg;
```

| Field (`struct unixsock`, `socket.c:14-33`) | Meaning |
| --- | --- |
| `boolean passcred` | `SO_PASSCRED` |
| `struct unix_cred peercred` | `SO_PEERCRED` snapshot (holds a process ref) |
| `u8 shutdown` | `RCV_SHUTDOWN` / `SEND_SHUTDOWN` (G2.3) |

| Function | Context | Does |
| --- | --- | --- |
| `sysreturn scm_parse(const struct msghdr *m, unixsock s, unixsock_scm *out)` | sender syscall context, under `context_set_err` | Validates the cmsgs and takes the fd refs and credentials. On error it releases everything it took |
| `void scm_release(unixsock_scm scm)` | any | `fdesc_put` each fd; `process_release(cred.p)` |
| `sysreturn scm_deliver(unixsock_scm scm, struct msghdr *m, int flags, process receiver)` | receiver syscall context | Writes `SCM_CREDENTIALS` then `SCM_RIGHTS` as 16-byte `cmsghdr` (`u64 cmsg_len`, high half zero for musl), `CMSG_ALIGN` 8. Sets `msg_controllen` and `msg_flags`. On `EFAULT` it uninstalls the fds it installed |
| `int process_visible_pid(process target, process viewer)` | any | 0 if reaped or invisible. G3.4 makes it namespace-aware |

The fd lifetime rule: a message holds one `fdesc` reference per fd from
`scm_parse` until `scm_deliver` installs it, the fd is dropped
(`MSG_CTRUNC`, a full fd table), or the queue is drained on close. The
receiver installs each fd into **its own** fd table, from its own syscall
context (the receiver's `unixsock_read_bh` is a contextual closure). So the
kernel never touches another process's fd table or user memory. With G1.3's
D2, `MSG_CMSG_CLOEXEC` sets the slot flag. A reference cycle (a socket sent
over its own pair) leaks until the VM exits. Linux breaks such cycles with
`unix_gc`. Nothing in the contract creates one.

### Shared futex keys (G2.2)

| Key | When | Table |
| --- | --- | --- |
| `(process_vm, uaddr)` | `FUTEX_PRIVATE_FLAG`, or the address is in a private or anonymous-private vmap | `process_vm->futices` (today `p->futices`, `futex.c:22-53`) |
| `(pagecache_node, node_offset + (uaddr - vm->node.r.start))` | no private flag and the vmap is `VMAP_FLAG_SHARED` (file-backed, memfd, or G1.2 C5's tmpfs-backed `MAP_SHARED\|MAP_ANONYMOUS`) | one global table under a spinlock. An entry holds an `fsfile` ref and is freed when its blockq is empty |

`futex_key_lookup(process_vm vm, u64 uaddr, boolean private, struct futex_key *k)`
resolves the vmap under `vmap_lock` (`mmap.c`). Every op, including the
`clear_tid` wake (`thread.c:497-500`) and robust-list wakes, goes through
it. Futex objects for private keys are freed once their blockq is empty
(today they are never freed, `futex.c:22-53`).

## 2. G3.1 credentials, prctl, procfs

### Credentials

```c
struct creds {
    u32 uid, euid, suid, fsuid;
    u32 gid, egid, sgid, fsgid;
    u32 ngroups;
    u32 *groups;
    u64 cap_permitted, cap_effective, cap_inheritable, cap_bset;
    user_namespace user_ns;
};
```

`struct process` (`unix_internal.h:518-560`) replaces `u32 uid, gid` with
`struct creds creds`, guarded by `p->lock`. Readers take a snapshot by
value. Ids are kernel ids, meaning ids in the initial user namespace; before
G3.3 every namespace is the initial one. `create_process`
(`unix.c:589-590`) seeds `uid=euid=suid=fsuid` from the manifest `uid`
(`ab823eb4`). Root (euid 0 in the initial namespace) starts with all 41
capabilities (`CapBnd 0x1ffffffffff`). Any other uid starts with none.
Credentials are per process, not per thread. musl's `__synccall` applies
`set*id` to every thread, so libc callers cannot tell the difference. A raw
per-thread `setuid` can.

`no_new_privs`, the seccomp mode and the seccomp filter live in
`struct thread` (`unix_internal.h:325-377`), as on Linux. They are copied
by `clone` and fork and kept by `execve`.

| Syscall | Rule (Linux) | Error |
| --- | --- | --- |
| `getuid`/`geteuid`/`getgid`/`getegid`/`getresuid`/`getresgid`/`getgroups` | `from_kuid(current ns)`, unmapped → 65534 | `getres*`/`getgroups` `EFAULT`; `getgroups` size < n → `EINVAL` |
| `setuid(u)` | `CAP_SETUID` in own ns → set r,e,s,fs; else `u ∈ {r,s}` → set e,fs | unmapped `u` → `EINVAL`; else `EPERM` |
| `setreuid`, `setresuid` (gid variants alike) | unprivileged: each new id (≠ -1) ∈ {r,e,s}; `setreuid` also updates `suid` per Linux | `EPERM`, `EINVAL` |
| `setgroups(n, list)` | `CAP_SETGID` in own ns, and the ns allows setgroups (G3.3) | `n > 65536` → `EINVAL`; `EPERM` |
| caps on an id change | r,e,s all leave 0 → clear permitted and effective. euid 0 → non-0 → clear effective. euid non-0 → 0 → effective = permitted (no `SECBIT_KEEP_CAPS`) | — |
| caps on `execve` (G1.3 hook) | euid equal to the ns root's kernel id (uid 0 mapped) → full set; otherwise permitted = effective = 0 (no file caps, no ambient) | — |
| `capget(hdr, data)` | versions `0x19980330` (1 struct), `0x20071026` and `0x20080522` (2 structs). `pid` 0 or own; another pid → that process (`ESRCH`). NULL `data` → probe only | bad version → write `0x20080522` into `hdr`, `EINVAL`. Today it zeroes one struct and ignores the version (`syscall.c:2291-2302`) |
| `capset` | pid 0 or own, else `EPERM`. new permitted ⊆ old permitted; effective ⊆ new permitted; inheritable ⊆ old inheritable ∪ (old permitted if `CAP_SETPCAP`), and ∩ bset | `EPERM`, `EINVAL`, `EFAULT` |
| `capable(cap)` / `ns_capable(ns, cap)` | effective in the target ns; or the target is a descendant ns owned by the caller's euid whose parent is the caller's ns (Linux `cap_capable` walk, G3.3) | — |

Registrations change at `syscall.c:2487-2494` (`syscall_ignore` goes away).
New registrations: `getresuid`, `getresgid`, `setresuid`, `setresgid`,
`setreuid`, `setregid`, `setfsuid`, `setfsgid`, `getgroups`, `getegid`,
`geteuid`. The x86 `getegid` alias is at `src/x86_64/unix_machine.c:265-266`.

### prctl (`syscall.c:2333-2348`)

| Option | Behaviour |
| --- | --- |
| `PR_SET_NAME` 15 / `PR_GET_NAME` 16 | kept |
| `PR_SET_NO_NEW_PRIVS` 38 | `arg2 == 1` and `arg3..5 == 0`, else `EINVAL`. Sets `t->no_new_privs`. It cannot be cleared |
| `PR_GET_NO_NEW_PRIVS` 39 | `arg2..5 == 0`, else `EINVAL`; returns 0/1 |
| `PR_SET_DUMPABLE` 4 / `PR_GET_DUMPABLE` 3 | `arg2 ∈ {0,1}`, else `EINVAL` (G1.1 adds `p->dumpable`) |
| `PR_SET_PTRACER` `0x59616d61` | Yama semantics: 0 clears, `-1` (`PR_SET_PTRACER_ANY`) or a pid visible in the caller's ns is stored in `p->ptracer`; an unknown pid → `EINVAL`. Nothing consumes it because `ptrace` stays `ENOSYS` |
| `PR_SET_PDEATHSIG` 1 / `PR_GET_PDEATHSIG` 2 | signal 0..64, else `EINVAL`. Delivered from G1.4's reparent path; cleared on fork |
| `PR_SET_SECCOMP` 22 / `PR_GET_SECCOMP` 21 | G3.2 |
| `PR_SET_VMA` `0x53564d41` | `EINVAL` (tolerated, contract §4) |
| `PR_GET/SET_SPECULATION_CTRL` 52/53 | `EINVAL`, the Linux answer when the arch has no control. Chromium treats it as non-fatal (`sandbox_bpf.cc:282-305`) |
| `PR_CAPBSET_READ` 23 | returns the bit from `cap_bset`; cap > 40 → `EINVAL` |
| anything else | `EINVAL` |

### procfs (`src/unix/procfs.c`, new)

A `struct filesystem` with dynamic `lookup` and `get_meta`, following
`p9_lookup` (`src/fs/9p.c:558-600`). It is mounted on `/proc` of the root
fs. `storage_get_mountpoint` (`src/kernel/storage.c:230`) learns the procfs
mount so `..` leaves it (`fs.c:1172-1196`). Inodes are procfs ids looked up
in a table. `get_meta` of a reaped pid's inode returns 0, which turns every
later lookup through it into `ENOENT`. That makes "chroot into a dead
helper's `fdinfo`" work without dangling tuples. The static entries
(`/proc/meminfo`, `/proc/mounts`, `/proc/self/maps`, `/proc/self/exe`,
`special.c:261-275`, `390-413`) move into procfs. `fill_stat`
(`syscall.c:1497-1530`) never sets `st_nlink`. It gains
`st_nlink = get(n, sym(nlink))` when that is present.

| Entry | Content | Slice |
| --- | --- | --- |
| `/proc` readdir | `self`, `thread-self`, one directory per live tgid (root-ns numbers, F6), the moved static files | G3.1 |
| `self` | symlink to the caller's root-ns tgid | G3.1 |
| `<pid>/status` | `Name`, `Umask`, `State`, `Tgid`, `Ngid`, `Pid`, `PPid`, `TracerPid`, `Uid`/`Gid` (r e s fs, translated into the opener's user ns), `FDSize`, `Groups`, `NStgid`/`NSpid`/`NSpgid`/`NSsid` (one id per level, root first; G3.4), `Threads`, `VmRSS` (G1.2's address-space RSS), signal masks, `CapInh`/`CapPrm`/`CapEff`/`CapBnd`/`CapAmb` (`%016lx`), `NoNewPrivs`, `Seccomp`, `Seccomp_filters` (leader thread) | G3.1 (seccomp fields G3.2) |
| `<pid>/task/` | one directory per tid; `nlink` = 2 + threads (`proc_self_task`, `proc_task_nlink`; `sandbox/linux/services/thread_helpers.cc:40-48`) | G3.1 |
| `<pid>/task/<tid>/status` | per-thread `NoNewPrivs`/`Seccomp` | G3.2 |
| `<pid>/fd/` | one magic link per fd. `readlink`: path, `socket:[ino]`, `pipe:[ino]`, `anon_inode:[eventfd]`, `/memfd:name (deleted)`. Follow (`stat`, `fstatat` without `AT_SYMLINK_NOFOLLOW`): `fstat` of that fdesc. `open`: reopen as in the gaps table (regular and memfd files; other types → `ENXIO`). Only the process itself, or root with `CAP_SYS_PTRACE`, else `EACCES` | G3.1 |
| `<pid>/fdinfo/` | one file per fd: `pos:`, `flags:` (octal), `mnt_id: 0` | G3.1 |
| `<pid>/exe` | the per-process image path (G1.3 D3) | G3.1 |
| `<pid>/cmdline` | the kernel copy of argv (F12) | G3.1 |
| `<pid>/maps` | moved from `special.c:153-189`, for any pid | G3.1 |
| `<pid>/root` | symlink to the stored root path (`/proc/<root-ns pid>/fdinfo` after Chromium's chroot) | G3.3 |
| `<pid>/ns/{user,pid,net,ipc,mnt,uts}` | magic links `user:[<inum>]` etc. ipc, mnt and uts point at singleton initial namespaces | G3.3 |
| `<pid>/{uid_map,gid_map,setgroups}` | §4 | G3.3 |

A magic link is a tuple tagged `sym(procfd)` (fd number and owning pid).
`filesystem_follow_links` (`fs.c:1312`) stops at it. `open_internal` and
the stat paths then act on the fdesc instead of resolving `linktarget`.

## 3. G3.2 seccomp-BPF

### Entry points

| Path | Where | Filtered by |
| --- | --- | --- |
| `syscall` instruction (x86_64) | `syscall_enter` `src/x86_64/crt0.s:196-226` → `jmp [syscall]` → `syscall_handler` `syscall.c:2556` | `syscall_dispatch` |
| strace klib | replaces the `syscall` pointer with `strace_syscall` (`klib/strace.c:607-686`, `739-745`). It duplicates the handler lookup at `672-679` | the same `syscall_dispatch`: strace keeps its enter and exit hooks around the call |
| PVM | `pvm_syscall` → the `syscall` pointer (`src/x86_64/pvm.c:115-126`) | covered by the pointer |
| vsyscall page | `vsyscall_gettimeofday`/`vsyscall_time` call `do_syscall` at CPL3 (`src/x86_64/unix_machine.c:12-22`); `vgetcpu` makes no syscall (`:24-31`) | the syscall instruction path. `si_call_addr` is the vsyscall text |
| `int $0x80` | no gate: only vector 3 is DPL3 (`src/x86_64/interrupt.c:87-98`) | `#GP` → `SIGSEGV`. No 32-bit user code segment exists (`src/x86_64/init.s:74-76` is null), so there is no compat `syscall`/`sysenter` path |
| io_uring ops | run kernel functions directly (`src/unix/io_uring.c`) | not filtered, as on Linux. Filters deny `io_uring_*` themselves |
| klib-swapped handlers (`swap_syscall_handler` `syscall.c:2695-2700`; `klib/shmem.c:44`, `klib/sandbox.c:46`, `klib/umcg.c:522`) | table entries | filtered, because the check runs before the table lookup |
| pledge/unveil | `klib/sandbox.c:7-21` wraps table entries. State is kernel-global (`klib/pledge.c:43-47`) | **Not reusable** as a seccomp hook: it is global rather than per thread, and it never sees unregistered numbers. It keeps working after seccomp |

### Dispatch change

```c
void syscall_dispatch(thread t, syscall_context sc, context_frame f);
```

`syscall_dispatch` holds today's `syscall.c:2585-2604` body: the shutdown
check, then `seccomp_check`, then the `call >= SYS_MAX` check, then table
lookup and call. It is called by `syscall_handler` and by `strace_syscall`.
The filter sees `nr = (int)rax` (with the x32 bit) and `arch =
AUDIT_ARCH_X86_64` (`0xc000003e`). `instruction_pointer` is
`f[FRAME_RIP]` (the address after `syscall`). `args[0..5]` are
`f[SYSCALL_FRAME_ARG0..5]`. Dispatch also switches to `(u32)rax`, as Linux
does, so the filter and the dispatcher read the same number. A restarted
syscall (`syscall_restart_arch_fixup`, `src/unix/signal.c:375`) re-enters
and is filtered again.

### Data structures (`src/unix/seccomp.{c,h}`, new)

```c
struct sock_filter { u16 code; u8 jt; u8 jf; u32 k; };

typedef struct seccomp_filter {
    struct refcount refcount;
    struct seccomp_filter *prev;
    u32 path_insns;
    u16 len;
    closure_struct(thunk, free);
    struct sock_filter insns[];
} *seccomp_filter;

/* struct thread */
u8 seccomp_mode;
boolean no_new_privs;
seccomp_filter seccomp;

boolean seccomp_check(thread t, context_frame f, sysreturn *rv);
sysreturn seccomp_validate(struct sock_filter *insns, u16 len);
u32 seccomp_run(seccomp_filter chain, const struct seccomp_data *sd, u16 *data);
sysreturn seccomp(u32 op, u32 flags, void *uargs);
sysreturn seccomp_prctl(u64 mode, void *prog);
void seccomp_fork(thread child, thread parent);
void seccomp_thread_exit(thread t);
```

A filter is immutable once installed. Its refcount counts the threads
that point at it plus the child filters whose `prev` it is. `path_insns` is
`len + 4` plus `prev->path_insns`; more than 32,768 gives `ENOMEM`. A
thread reads only its own `t->seccomp`, without a lock. `TSYNC` writes
other threads' pointers under `threads_lock`. The old filter is then an
ancestor of the new one, so it stays alive while a thread is still running
it.

### Validator (Linux `bpf_check_classic` + `seccomp_check_filter`)

| Rule | Error |
| --- | --- |
| `len == 0` or `len > 4096` | `EINVAL` |
| last instruction is not `BPF_RET` | `EINVAL` |
| allowed opcodes only: `LD W ABS`, `LD W LEN`, `LDX W LEN`, `LD IMM`, `LDX IMM`, `LD MEM`, `LDX MEM`, `ST`, `STX`, `ALU {ADD,SUB,MUL,DIV,AND,OR,XOR,LSH,RSH} {K,X}`, `ALU NEG`, `MISC TAX/TXA`, `JMP JA`, `JMP {JEQ,JGT,JGE,JSET} {K,X}`, `RET {K,A}` | anything else (`MOD`, `LD H/B`, `IND`, `MSH`) → `EINVAL` |
| `LD W ABS`: `k < sizeof(seccomp_data)` (64) and `k % 4 == 0` | `EINVAL` (`misaligned`, `beyond_data` at `mini_chromium.c:814-815`) |
| `JA`: `k < len - pc - 1`; conditional: `pc + 1 + jt < len` and `pc + 1 + jf < len` (forward only, since offsets are unsigned) | `EINVAL` (`out_of_range` at `:812`) |
| `MEM`/`ST`/`STX`: `k < 16`; each `M[k]` must be written on every path before it is read | `EINVAL` |
| `DIV` by `K == 0`; `LSH`/`RSH` with `K >= 32` | `EINVAL` |
| `LD W LEN` / `LDX W LEN` | rewritten to an immediate of 64 |

The interpreter (about 110 LoC) is a `switch` over the allowed opcodes, with
`A`, `X` and `M[16]` on the stack. `DIV` by `X == 0` returns 0
(Linux). Loads read the kernel `struct seccomp_data` only.

### seccomp(2) and prctl

| Call | Behaviour (order as in Linux) |
| --- | --- |
| `SECCOMP_SET_MODE_STRICT` (0) | flags ≠ 0 or `uargs` ≠ NULL → `EINVAL`. Mode 1: only `read`, `write`, `exit`, `rt_sigreturn`; anything else kills the process with `SIGKILL` |
| `SECCOMP_SET_MODE_FILTER` (1) | 1. flags ⊄ {`TSYNC` 1, `SPEC_ALLOW` 4, `TSYNC_ESRCH` 16} → `EINVAL`. 2. `copy_from_user(fprog)` fails → `EFAULT`. This covers the NULL-program probes: `prctl(PR_SET_SECCOMP, 2, NULL)` and `seccomp(1, TSYNC, NULL)` both give `EFAULT` (`mini_chromium.c:1243-1245`; contract §2 step 9, §3 e). 3. `len` 0 or > 4096 → `EINVAL`. 4. neither `t->no_new_privs` nor `ns_capable(user_ns, CAP_SYS_ADMIN)` → `EACCES`. 5. copying the instructions fails → `EFAULT`. 6. validate → `EINVAL`. 7. path > 32,768 → `ENOMEM`. 8. a strict-mode thread → `EINVAL`. 9. `TSYNC`: every other thread's filter must be an ancestor of (or equal to) the caller's, else return that thread's tid (`-ESRCH` with `TSYNC_ESRCH`). On success every thread gets the new filter, mode 2, and the caller's NNP if set |
| `SECCOMP_GET_ACTION_AVAIL` (2) | reads a `u32`: `KILL_PROCESS`, `KILL_THREAD`, `TRAP`, `ERRNO`, `TRACE`, `LOG`, `ALLOW` → 0; `USER_NOTIF` → `EOPNOTSUPP` (no listener support); fault → `EFAULT` |
| `SECCOMP_GET_NOTIF_SIZES` (3), other ops | `EINVAL` |
| `prctl(PR_SET_SECCOMP, 1)` / `(…, 2, prog)` | strict / `SET_MODE_FILTER` with flags 0 |
| `prctl(PR_GET_SECCOMP)` | the mode; in strict mode the call itself is killed (Linux) |
| `SPEC_ALLOW` | accepted. Nanos has no SSBD control, so a filter installed without it is equivalent to Linux booted with `spec_store_bypass_disable=prctl` |

### Actions

All filters in the chain run. The lowest
`(s32)(ret & SECCOMP_RET_ACTION_FULL)` wins, and the newest filter wins
ties (Linux order: `KILL_PROCESS` < `KILL_THREAD` < `TRAP` < `ERRNO` <
`USER_NOTIF` < `TRACE` < `LOG` < `ALLOW`). Unknown values are
`KILL_PROCESS`.

| Action | Effect in `syscall_dispatch` |
| --- | --- |
| `ALLOW`, `LOG` | run the handler |
| `ERRNO` | skip; return `-min(data, 4095)` |
| `TRAP` | skip; set `f[FRAME_RAX] = nr` (Linux `syscall_rollback`); force `SIGSYS` with `si_code = SYS_SECCOMP` (1), `si_errno = data`, `sifields.sigsys = { call_addr = f[FRAME_RIP], syscall = nr, arch = AUDIT_ARCH_X86_64 }` (the union already exists, `src/unix/system_structs.h:490-494`) |
| `TRACE`, `USER_NOTIF` | skip; return `-ENOSYS` |
| `KILL_THREAD` | the last live thread: as `KILL_PROCESS`; otherwise the thread exits |
| `KILL_PROCESS`, unknown | mode becomes "dead"; `RAX = nr`; the process dies of `SIGSYS` with a core-dump status (G1.4) |

"Force" is a new `force_signal(thread, siginfo)` in `signal.c`. It unblocks
the signal, resets `SIG_IGN` to `SIG_DFL`, then calls
`deliver_signal_to_thread` (`signal.c:267`). Today fault signals are
queued without forcing (`src/unix/unix.c:189-227`). The register state
then matches Chromium's handler (contract §3 "SIGSYS trap handler
contract"). `setup_sigframe` copies the frame, so `gregs[REG_RIP] ==
si_call_addr` and `gregs[REG_RAX] == si_syscall`
(`src/x86_64/unix_machine.c:117-165`). The handler stores its result in
`gregs[REG_RAX]`, and `rt_sigreturn` restores `RAX` and `RIP` from the frame
and returns without a return value of its own (`signal.c:382-395`,
`unix_machine.c:169-215`). `check_syscall_restart` (`signal.c:358-380`)
acts only on `-ERESTART*`, so `RAX = nr` is not mistaken for a restart.

Inheritance: `clone(CLONE_THREAD)` (`thread.c:47-97`) and G1.2's fork copy
`seccomp_mode`, the `seccomp` pointer (taking a reference) and
`no_new_privs` from the calling thread. `execve` keeps them. Thread exit
releases the reference.

## 4. G3.3/G3.4 namespaces, chroot

### Objects (`src/unix/namespace.{c,h}`, new)

```c
struct id_extent { u32 first, lower_first, count; };
struct id_map { u32 nr; boolean written; struct id_extent e[5]; };

typedef struct user_namespace {
    struct refcount refcount;
    struct user_namespace *parent;
    u32 level, owner, group, inum;
    struct id_map uid_map, gid_map;
    boolean setgroups_allowed;
    struct spinlock lock;
} *user_namespace;

typedef struct pid_namespace {
    struct refcount refcount;
    struct pid_namespace *parent;
    user_namespace user_ns;
    u32 level, inum, nr_live;
    id_heap ids;
    table pids;
    process reaper;
    boolean dying;
    blockq exit_bq;
    struct spinlock lock;
} *pid_namespace;

typedef struct pid {
    struct refcount refcount;
    u32 level;
    thread t;
    struct upid { pid_namespace ns; u32 nr; } numbers[];
} *pid;

typedef struct net_namespace {
    struct refcount refcount;
    user_namespace user_ns;
    boolean initial;
    u32 inum;
} *net_namespace;

typedef struct fs_struct {
    struct refcount refcount;
    struct spinlock lock;
    filesystem root_fs, cwd_fs;
    inode root, cwd;
    buffer root_path;
} *fs_struct;
```

| `struct process` field | Replaces / shared by |
| --- | --- |
| `fs_struct fs` | `root_fs`, `cwd_fs`, `cwd` (`unix_internal.h:526-530`); shared under `CLONE_FS`, copied on fork |
| `creds.user_ns` | — |
| `pid tgid_pid`, `pid_namespace pid_ns_for_children` | `pid` (`unix_internal.h:520`); `pid_ns_for_children` is set by `unshare(CLONE_NEWPID)` |
| `net_namespace net_ns` | — |
| `struct thread`: `pid pid` | the global tid. `uh->processes` (`unix.c:690`) becomes the initial pid namespace's id heap |

Initial objects: `init_user_ns` (identity map `0 0 4294967295`, nesting
level 0), `init_pid_ns`, `init_net` and one `fs_struct`. Kernel ids are
initial-namespace ids, so before G3.3 everything behaves as today.

### User namespaces (G3.3)

| Operation | Rule (Linux) | Error |
| --- | --- | --- |
| `clone`/`unshare(CLONE_NEWUSER)` | new ns: parent = caller's, owner = caller's euid, `setgroups_allowed` inherited. The child gets the full capability set in it, inheritable 0. Ids are unchanged and unmapped until a map is written (they read 65534) | `CLONE_THREAD` or `CLONE_FS` with it → `EINVAL`; `unshare` with >1 thread or a shared `fs_struct` → `EINVAL`; caller chrooted → `EPERM`; level 32 → `EUSERS` |
| write `uid_map`/`gid_map` | one `write` at offset 0, ≤ `PAGESIZE - 1` bytes, ≤ 5 lines of `first lower count`, no overlap, `count > 0`. The writer needs `CAP_SETUID`/`CAP_SETGID` in the target ns. The opener's ns must be the target or its parent. Lower ids are mapped through the parent's map. Unprivileged writers may map only one id, equal to the opener's euid/egid, and for `gid_map` only after `setgroups` is `deny`; else `CAP_SET[UG]ID` in the parent is needed | second write → `EPERM` (`userns_map`'s `second_write_eperm`); format → `EINVAL`; permission → `EPERM` |
| read `uid_map` | lower ids are relative to the opener's ns, or to its parent when the opener is in the target ns | — |
| `setgroups` file | `allow`/`deny`. Opening it for write needs `CAP_SYS_ADMIN` in the ns (`EACCES`). `deny` after `gid_map` is written → `EPERM`; `allow` after `deny` → `EPERM` | — |
| `setgroups(2)` | needs `gid_map` written and `setgroups_allowed` | `EPERM` |
| `getuid` & co. | `from_kuid(creds.user_ns)` | — |
| caps | `ns_capable` walk (§2); creating a pid or net ns needs `CAP_SYS_ADMIN` in the user ns that will own it | `EPERM` |

### chroot and the per-process root (G3.3)

| Change | Where |
| --- | --- |
| absolute paths and `..` clamping use the process root: `fs_path_helper` gains `get_root(filesystem *fs, tuple *t)`. In a syscall or process context it returns `current->p->fs->root`, elsewhere the global root. `filesystem_resolve_internal` uses it at `fs.c:1208-1219`, and the `..` test at `fs.c:1172-1173` compares with it | `src/fs/fs.c`, `src/kernel/storage.c:230` |
| `resolve_dir` starts absolute paths at the process root | `src/unix/filesystem.h:1-23` |
| `chdir`/`fchdir`/`getcwd` act on `p->fs` under `fs->lock`; `getcwd` prints relative to the root, or `(unreachable)` outside it | `src/unix/filesystem.c:207-231`, `syscall.c:1240-1270`, `1817` (`getcwd`) |
| `chroot(path)`: `CAP_SYS_CHROOT` in the user ns, else `EPERM`; not a directory → `ENOTDIR`; resolved through the current root. It sets `fs->root` and `root_path` (the procfs path with root-ns pids, for `/proc/<pid>/root`). The cwd is left alone (Linux) | new in `syscall.c` |
| `clone(CLONE_FS)` shares `p->fs` (reserve); otherwise it is copied | G1.2 fork in `thread.c` |
| `AF_UNIX` path lookup goes through the same walk | `socket.c:410-424`, `605-606` |

Chromium's sequence then works as on Linux. The `CLONE_FS` helper runs
`chroot("/proc/self/fdinfo/")`: `self` resolves to the helper, and the
shared `fs_struct` changes for the zygote too. The helper exits and is
reaped, after which procfs `get_meta` for that inode returns 0. Every path
then gives `ENOENT`, and `/proc` is no longer a directory (`chroot_helper`,
`fs_restricted`).

### PID namespaces (G3.4)

| Rule | Implementation |
| --- | --- |
| `alloc_pid(ns)` gives one number per level, from the leaf up to the root; the first in a new ns is 1 and becomes `ns->reaper` | `namespace.c`; `create_thread`/fork take a `pid` instead of `mem_alloc_u64` (`unix.c:573`, `thread.c:387-390`) |
| `pid_nr_ns(pid, ns)` returns the number at `ns->level` if `numbers[ns->level].ns == ns`, else 0; `pid_vnr` = the caller's ns | used by `getpid` `syscall.c:2151`, `gettid` `thread.c:7`, `getppid` (G1.2), the clone return and `CLONE_PARENT_SETTID`/`CHILD_SETTID` (`thread.c:86-89`), `wait4`/`waitid` pid arguments and results and `si_pid`, `SIGCHLD` and `kill` `si_pid`, `kill`/`tgkill`/`tkill`/`rt_*sigqueueinfo` (`signal.c:584-700`), `sched_*`/`prlimit64`/`get_robust_list` pid arguments (`lookup_thread` `syscall.c:2214-2224`, `syscall.c:1778`, `futex.c:374-397`), `process_visible_pid` (G2.1), `PR_SET_PTRACER`, procfs `NSpid` |
| lookup by number: `find_pid_ns(nr, ns)`; not found → `ESRCH` (`kill_outside_denied`) | replaces `thread_from_tid` |
| musl's cached tid after `clone(CLONE_NEWPID)` must read 1 | `gettid` is ns-local (contract §6, Alpine `musl-tid-caching.patch`) |
| orphans reparent to `ns->reaper` | G1.2's reparent-to-1 generalised |
| signals to the ns init from inside the ns with `SIG_DFL` are dropped; `SIGKILL`/`SIGSTOP` from an ancestor ns are delivered | send path in `signal.c` |
| init exits: `ns->dying`; `SIGKILL` every member; the init's exit blocks on `exit_bq` until `nr_live == 1`, auto-reaping its children; later forks into the ns → `ENOMEM` | `namespace.c` + G1.2 exit path |
| `getppid` of an ns init → 0 | — |

### Network namespaces (G3.4)

A new netns has no interfaces and a down loopback. Every `AF_INET`/`AF_INET6`
socket records its creation `net_ns`. In a non-initial netns, `socket()`
builds an **isolated inet socket** that never reaches lwIP:

| Op | Result |
| --- | --- |
| `connect`, `sendto`/`sendmsg` with a destination | `ENETUNREACH` (`netns_denied`) |
| `bind` to `INADDR_ANY`/`in6addr_any` | 0 (recorded) |
| `bind` to any other address | `EADDRNOTAVAIL` |
| `listen`, `getsockname`, `setsockopt`/`getsockopt` of `SOL_SOCKET` basics, `close` | work; `accept` blocks or gives `EAGAIN` |
| `socket(AF_NETLINK)`, `socket(AF_VSOCK)` | `EAFNOSUPPORT`; neither is reachable without interfaces or a host channel |
| `AF_UNIX` path sockets | unchanged (they cross netns on Linux too) |

The gate sits in `socket()` (`src/net/netsyscall.c:1475-1496`), which
creates the isolated object. The plan's `src/net/` ownership is enough. A
socket created in the initial netns and passed in by `SCM_RIGHTS` keeps
working, as on Linux.

### clone/unshare flags Chromium uses (contract §2, §1)

| Flags | Caller | Result |
| --- | --- | --- |
| `CLONE_NEWUSER\|SIGCHLD` | browser probe (`credentials.cc:267-285`) | new process in a new userns |
| `CLONE_NEWUSER\|CLONE_NEWPID\|CLONE_NEWNET\|SIGCHLD` | browser → zygote (`namespace_sandbox.cc:171-211`) | the pid and net namespaces are owned by the new userns. The `CAP_SYS_ADMIN` check is against that new userns, so it succeeds |
| `unshare(CLONE_NEWUSER)` | probe child; zygote (`credentials.cc:296-302`, `320-342`) | nested ns (two deep, `userns_map`'s `nested`) |
| `CLONE_FS\|SIGCHLD` | chroot helper (`credentials.cc:97,113`) | shared `fs_struct` |
| `CLONE_NEWPID\|SIGCHLD` | zygote → renderer (`zygote_linux.cc:429-432`) | child is pid 1 (`pidns_isolation`) |
| `SIGCHLD` (fork), the musl thread mask, `CLONE_VM\|CLONE_VFORK\|SIGCHLD` | G1 | — |
| `CLONE_NEWNS`, `CLONE_NEWIPC`, `CLONE_NEWUTS`, `CLONE_NEWCGROUP`, `CLONE_NEWTIME`, in `clone`, `clone3` or `unshare` | nobody | `EINVAL`, what Linux returns when that namespace type is compiled out |
| `CLONE_NEWPID\|CLONE_THREAD`, `CLONE_NEWUSER\|CLONE_THREAD`, `CLONE_NEWUSER\|CLONE_FS`, `CLONE_NEWNS\|CLONE_FS` | — | `EINVAL` |
| `setns`, `mount`, `ptrace`, `process_vm_*` | nobody | stay unregistered (`ENOSYS`) |

`clone3` shares `clone_internal` (`thread.c:47`, `119-146`), so one flag
check covers both.

## 5. Dependency graph and slice plan

```text
G1.1 AS ─ G1.2 fork/wait/process_vm/fd table/C5 ─ G1.3 exec/cloexec slot ─ G1.4 kill/process signals
                                                     │
G2.1 SCM data, creds, MSG flags ◄────────────────────┤ (probe spawns via exec)
  ├─ G2.2 shared futex, memfd/seals, robust ◄── G1.2 C5
  └─ G2.3 shutdown, RDHUP, SIGPIPE, membarrier
G3.1 creds/caps/prctl/procfs ◄── G1.3 (NNP across exec, exe, argv copy), G1.2 (VmRSS, ppid)
  ├─ G3.2 seccomp ◄── G1.4 (KILL_PROCESS, forced signals)
  └─ G3.3 userns, fs_struct, chroot, ns links, id maps   (no code dependency on G3.2)
       └─ G3.4 pidns, netns, PID-1 rules ◄── G2.1 (process_visible_pid)
```

G3.3 does not depend on G3.2's code. The two touch the same files, so they
stay serial (plan: one writer).

| Slice | Implements | Owns | Probe checks turned green | New runtime test | ~LoC |
| --- | --- | --- | --- | --- | --- |
| G2.1 | SCM data structures; `SCM_RIGHTS` send/receive/`CTRUNC`/`CMSG_CLOEXEC`; queue drain; stream boundaries; `SO_PASSCRED`, implicit and explicit `SCM_CREDENTIALS`, `SO_PEERCRED`, `process_visible_pid`; `MSG_DONTWAIT`/`MSG_TRUNC`/unknown flags; `msg_controllen`/`msg_flags`; `SEQPACKET` size limit raised to `so_rcvbuf` | `src/unix/socket.c`, `src/unix/unix_internal.h`, `src/unix/unix.c` (fd allocator with slot flags, if G1.3 has not added one), `test/runtime/scm_rights.*` | `scm_rights`, `peer_credentials`, `scm_rights_negative`; it also unblocks `shared_memory_fd`, `seal_enforcement`, `private_stays_private` (they ride on fd passing) | `scm_rights`: 1 to 253 fds on `STREAM` and `SEQPACKET`; 128 fds; 254 → `EINVAL`; `EBADF`; bad `cmsg_len` → `EINVAL`; a small control buffer → `MSG_CTRUNC`, and the dropped fds are closed (the peer sees EOF); a full receiver fd table (`RLIMIT_NOFILE`) → `CTRUNC`; close with fds in flight frees them (EOF observed); `SO_PEERCRED` for socketpair and for connect/accept; `SO_PASSCRED` on either end; explicit creds with another pid → `EPERM`; `MSG_DONTWAIT` → `EAGAIN`; `SEQPACKET` `MSG_TRUNC`; a stream read stops at a rights message | 480 + 420 test |
| G2.2 | shared futex keys and freeing; `REQUEUE`, `WAKE_BITSET`, bitsets; robust-list fix (own commit, F11); memfd `CLOEXEC`/`NOEXEC_SEAL`/name length; `F_ADD_SEALS` on read-only fd; cross-AS `F_SEAL_WRITE` `EBUSY`; cross-AS truncate unmapping if G1 left it | `klib/shmem.c`, `src/unix/{mmap.c,futex.c,filesystem.c}`, `src/unix/syscall.c` (`F_ADD_SEALS` call site), `src/kernel/pagecache*`, `test/runtime/shmem_xproc.*` | `futex_xproc`; keeps `shared_memory_fd`, `seal_enforcement`, `private_stays_private`, `mojo_probes` | `shmem_xproc`: memfd sent to a child, both map it, writes cross both ways; seals: `SHRINK`/`GROW` in the receiver → `EPERM`; `F_SEAL_WRITE` while a child maps it writable → `EBUSY`, after unmap ok, then a writable map → `EPERM`; read-only fd → writable `MAP_SHARED` `EACCES`, `F_ADD_SEALS` `EPERM`; shared futex wait/wake/`CMP_REQUEUE`/`WAKE_BITSET` across processes on memfd and `MAP_SHARED\|ANONYMOUS`; `FUTEX_PRIVATE_FLAG` on a shared page stays private; robust list: owner dies → `OWNER_DIED` and wake, non-owner entry left alone, a cyclic list terminates; `MFD_CLOEXEC` closed on exec | 300 + 380 test |
| G2.3 | `unixsock_shutdown`; `EPOLLRDHUP`/`HUP`; `SIGPIPE` on `EPIPE` (unix, pipe); `membarrier` | `src/unix/{socket.c,pipe.c,poll.c,eventfd.c,syscall.c}`, `test/runtime/ipc_xproc.*` | `epoll_eventfd` (probably green after G1.1 A8); no probe check covers shutdown or pipes (F4) | `ipc_xproc`: epoll wakes on another process's eventfd/pipe/socket write; `EPOLLRDHUP` after the peer's `SHUT_WR`; `EPOLLHUP` on peer exit; `SHUT_RD` read gives 0; `SHUT_WR` write gives `EPIPE` + `SIGPIPE`, none with `MSG_NOSIGNAL`; `shutdown(fd, 3)` → `EINVAL`; blocking reader woken by a writer in another process; `membarrier` query/register/expedited, unregistered → `EPERM`, flags → `EINVAL` | 230 + 350 test |
| G3.1 | `struct creds`; `set*id`/`get*id`/groups; capabilities and exec rule; `prctl` table; procfs core (mount, pid dirs, `self`, `status`, `task/` with nlink, `fd/` magic links and reopen, `fdinfo/`, `exe`, `cmdline`, `maps`, moved static files); `st_nlink` | `src/unix/{syscall.c,special.c,unix.c,unix_internal.h,exec.c}`, `src/unix/procfs.c` (new), `src/kernel/storage.c`, `platform/*/Makefile`, `test/runtime/creds.*` | `credentials`, `nnp_state`, `proc_self_task` (`mojo_probes` stays green) | `creds`: root drops to 1000 (r,e,s, caps cleared, `CapEff 0`); `setuid(0)` → `EPERM`; `setresuid` permutations; `setgroups` unprivileged → `EPERM`; `capget` versions and the bad-version write-back; `capset` raise → `EPERM`; NNP set, get, kept across fork and exec, cannot be cleared (`EINVAL`); dumpable; `PR_SET_PTRACER` ok and bad pid → `EINVAL`; unknown option → `EINVAL`; `/proc/self/status` and `/proc/<child>/status` fields; `/proc/self/task` nlink with 1 and 3 threads; `/proc/self/fd` listing and `fstatat` types; memfd reopen `O_RDONLY` then writable map → `EACCES`, read sees writes; `/proc` readdir lists live pids only | 1,280 + 420 test |
| G3.2 | `syscall_dispatch`; validator; interpreter; `seccomp(2)`, `PR_SET/GET_SECCOMP`, strict mode; `TSYNC`; actions; `force_signal`; inheritance; `Seccomp`/`Seccomp_filters` | `src/unix/seccomp.{c,h}` (new), `src/unix/{syscall.c,signal.c,thread.c,procfs.c}`, `klib/strace.c`, `src/x86_64/` (none expected), `platform/*/Makefile`, `test/runtime/seccomp.*` | `seccomp_enforcement`, `seccomp_trap`, `bpf_rejects_malformed`, `seccomp_inherit`, `seccomp_requires_nnp`, `proc_status` (F2), `ptrace_denied` (F5) | `seccomp`: the invalid corpus (backward/out-of-range jump, no `RET`, misaligned and beyond-data load, `len` 0 and 4097, `MOD`, uninitialised `M[]`) → `EINVAL`; NULL program with each flag → `EFAULT`; no NNP, unprivileged → `EACCES`; unknown flag → `EINVAL`; `ERRNO` value capped at 4095; `TRAP` siginfo and `gregs` (`RIP`, `RAX == nr`, result written back); `SIGSYS` blocked or ignored is still forced; `KILL_PROCESS` status `SIGSYS`; `KILL_THREAD` on a second thread; stacking precedence; unregistered syscall filtered (`ptrace`); x32 bit; fork, clone and exec inherit; `TSYNC` success, divergent thread → its tid, `TSYNC_ESRCH` → `ESRCH`; strict mode allows only read, write, exit and sigreturn; the same run under the strace klib | 600 + 480 test |
| G3.3 | user namespaces; `uid_map`/`gid_map`/`setgroups`; `ns_capable`; `fs_struct`; root-aware lookup; `chroot`; `/proc/<pid>/{root,ns/*}`; clone/unshare flag table | `src/unix/namespace.{c,h}` (new), `src/unix/{syscall.c,thread.c,unix.c,unix_internal.h,filesystem.c,filesystem.h,procfs.c,socket.c,exec.c}`, `src/fs/fs.c`, `src/kernel/storage.c`, `platform/*/Makefile`, `test/runtime/userns.*` | none on its own (F3) | `userns`: unprivileged `clone(NEWUSER)`; ids read 65534 before the map; map order `setgroups`, `gid_map`, `uid_map`; second write → `EPERM`; a foreign id → `EPERM`; `gid_map` before `deny` → `EPERM`; `deny` after `gid_map` → `EPERM`; nested two deep with zero caps; full caps inside and none in the parent; exec as non-root drops caps; `CLONE_FS` helper chroot to `/proc/self/fdinfo/`, then `/etc/passwd` and the binary fail and `/proc` is not a directory; `..` stays at the root; `chroot` without the cap → `EPERM`; `unshare(NEWUSER)` multithreaded → `EINVAL`; `NEWNS`/`NEWUTS`/`NEWIPC` → `EINVAL`; `/proc/self/ns/*` links differ after unshare | 900 + 450 test |
| G3.4 | `struct pid`, pid namespaces and every translation site; PID-1 rules and ns teardown; net namespaces and isolated inet sockets; real `process_visible_pid`; procfs `NSpid` | `src/unix/{namespace.c,thread.c,signal.c,syscall.c,socket.c,procfs.c,unix.c,futex.c}`, `src/net/netsyscall.c` (gate only), `src/unix/{netlink.c,vsock.c}` (gate only), `test/runtime/pidns_netns.*` | `zygote_launch`, `scm_credentials_translated`, `seccomp_null_probes`, `userns_map`, `chroot_helper`, `pidns_isolation`, `userns_caps_scoped`, `kill_outside_denied`, `netns_denied`, `fs_restricted`, `proc_task_nlink`, `zygote_renderer_ipc`, `zygote_renderer_seccomp` | `pidns_netns`: child of `NEWPID` sees `getpid() == gettid() == 1`, `getppid() == 0`; the parent sees the root pid; `SCM_CREDENTIALS` pid seen from both sides; an invisible sender → 0; `kill` of an outside pid → `ESRCH`; `SIGTERM` to init from inside with `SIG_DFL` is dropped, `SIGKILL` from the parent is not; init death kills a grandchild and a later fork → `ENOMEM`; `waitid(P_ALL)` reaper; `NSpid` levels; netns: `socket` ok, `connect` 1.1.1.1 and 127.0.0.1 → `ENETUNREACH`, `bind(ANY)` ok, `bind(127.0.0.1)` → `EADDRNOTAVAIL`, `AF_NETLINK` → `EAFNOSUPPORT`, a path `AF_UNIX` socket still connects, a fd passed from the initial netns still works | 900 + 450 test |

Totals: about 4,700 LoC of kernel code and about 2,950 LoC of tests. The
milestone suite runs after G2.3 and G3.4 (plan **Sequence**).

### Risks

| Risk | Evidence | Mitigation |
| --- | --- | --- |
| A forgotten pid-translation site leaks root-ns pids or finds a process outside the ns | §4 site list; `thread_from_tid` callers | Delete `thread_from_tid` and the global tid. Every lookup must go through `find_pid_ns`. `pidns_netns` asserts each listed syscall from inside a ns |
| The root-aware lookup callback reads `current` in contexts where no process applies (bottom halves, storage mounts) | `fs.c:1206-1219` is shared with mkfs and kernel callers | `get_root` returns the global root outside syscall and process contexts. Kernel callers pass explicit cwd tuples as today |
| procfs inode lifetime: an fd or root left on a reaped pid's directory | 9p-style dynamic tuples | Inodes are table ids. `get_meta` returns 0 after reap. The tuples are freed under the procfs lock. Test chroot-then-reap |
| In-flight fd cycles leak | no `unix_gc` | Accepted. Nothing in the contract sends a socket over itself. Documented in the commit body |
| Dropping fds on `MSG_CTRUNC` runs `close` handlers inside the receiver's `unixsock_read_bh` with the socket lock held | `socket.c:161-276` | Drop them after `unixsock_unlock`, with `fdesc_put` (ctx 0, `unix_internal.h:691-695`) |
| `TSYNC` racing thread creation | `clone_internal` copies filters (`thread.c:47-97`) | Copy under `threads_lock`, the same lock `TSYNC` holds |
| `KILL_PROCESS`, PID-1 teardown and `force_signal` depend on G1.4's process kill | plan G1.4 | Hard prerequisite: G3.2 does not start before G1.4 |
| `SEQPACKET` messages larger than `so_rcvbuf` | F10 | Same bound as Linux's `sk_sndbuf` |
| Existing programs relying on the silent 0 from `prctl`/`setuid`/`capset` | plan Decisions "The `prctl` contract changes" | List them in the final report. The upstream suite (`ftrace.c:116` uses `PR_SET_NAME`, which is kept) is unaffected |
| aarch64/riscv64 entry paths not compiled here | G1 note risks | `syscall_dispatch` is arch-neutral. The arch-specific parts are `seccomp_data` filling and `AUDIT_ARCH` (x86_64 only); other arches return `EINVAL` from `seccomp` |
| G5 extras not in the contract: netlink `KOBJECT_UEVENT` with `SO_ATTACH_FILTER` in the browser | `strace-full.txt:2340,63186` | Out of G2/G3. Raise it at G5 if Helium does not tolerate the failure |

No item here needs anything the plan forbids. Every new cross-process path
copies through kernel memory, and each process's user memory is touched only
in that process's own context. The one exception is
`/proc/<pid>/cmdline`, which F12 removes.
