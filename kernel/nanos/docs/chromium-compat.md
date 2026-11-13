# Chromium compatibility: MMU-isolated processes

Design note for giving Nanos per-process address spaces (`fork`/`clone`
without `CLONE_VM`, copy-on-write, `execve`, `wait4`). Paths are relative to
`nanos/` at hydrogen `0e5ae378` (upstream `e4b9c9e`). `file:line` refers to
that tree.

## Verdict

**Feasible. No hard blocker.** The kernel keeps no data in the user half
after boot. The syscall entry path never touches CR3. Blocked syscalls resume
in the owning syscall context, so a per-context CR3 switch covers them. Page
tables are reached through the direct map. The page-table code already
resolves its root through one hook, `get_pagetable_base(vaddr)`. The work is
broad but local: about 2,000 LoC across about 30 files for G1.1. Three items
must change before the first `fork`, or the kernel crashes or writes into the
wrong process:

1. The page cache records user mappings by virtual address alone and asserts
   that they never overlap (`src/kernel/pagecache.c:1727-1732`). A forked
   child maps the parent's program text at the same addresses
   (`src/unix/exec.c:300-303`), so the assertion halts the kernel.
2. `epoll`/`select`/`poll` write into a blocked waiter's user buffer from the
   notifier's context (`src/unix/poll.c:391-430`, `705-730`, `951-975`). With
   two address spaces, a pipe write in process B writes B's memory at A's
   addresses. That is an isolation breach, not just a crash.
3. Process exit is VM exit: `exit_group` calls `kernel_shutdown`
   (`src/unix/syscall.c:2171-2174`), and so do a default fatal signal
   (`src/unix/signal.c:1087`) and an unhandled SIGSEGV or SIGKILL
   (`src/unix/thread.c:177-203`).

## 1. Current VM model

### Page tables and CR3

| Fact | Evidence |
| --- | --- |
| x86_64 has one page table, rooted at the global `pagebase`. | `src/x86_64/page.c:3`; set from CR3 in `init_mmu` `src/x86_64/page.c:115` (kernel), or loaded by stage2 `src/x86_64/page.c:121-126` |
| Every walk uses `pagebase`, whatever the address. | `get_pagetable_base` returns `pagebase` `src/x86_64/page_machine.h:182-186`; callers `src/kernel/page.c:142`, `321`, `562`, `583`; `__physical_from_virtual_locked` walks `pagebase` directly `src/x86_64/page_machine.h:274-284` |
| CR3 is written only at boot and for full flushes. It is never switched. | `src/x86_64/page.c:18-19` (reload same value), `125`; `src/x86_64/init.s:28,148` (AP trampoline); `src/x86_64/uefi.c:30`; APs copy the BSP CR3 `src/x86_64/mp.c:124` |
| The syscall and interrupt entry paths do not touch CR3 (no KPTI). | `src/x86_64/crt0.s:196-256` |
| aarch64 already keeps separate user and kernel roots. riscv64 has one root. | `src/aarch64/page_machine.h:192-198`, `src/aarch64/page.c:61-62,82-83`; `src/riscv64/page_machine.h:1-6`, `src/riscv64/page.c:57,68-69` |
| Page-table pages come from a bump allocator in 1 MiB chunks, under one global spinlock, and are never freed. | `src/kernel/page.c:65-95`, `29`; `pt_lock` `src/kernel/page.c:6`, `src/kernel/page.h:1-4`; `unmap` never frees tables `src/kernel/page.c:383-431` |
| Table pages are reached through a linear map, not an identity map. | `pointer_from_pteaddr` `src/kernel/page.c:55-63`; `init_page_tables` with `kvmem.linear` `src/kernel/init.c:172-189` |
| Intermediate entries always carry USER and WRITABLE. Permission is decided at the leaf. | `new_level_pte` `src/x86_64/page_machine.h:172-175` |
| No page is global (no `PAGE_GLOBAL`), so every CR3 load flushes kernel TLB entries too. | `src/x86_64/page_machine.h:3-13`; CR4.PGE is set but unused `src/x86_64/mp.c:51` |
| SMEP and UMIP are on. SMAP is not, so the kernel reads user pages with plain loads. | `src/x86_64/mp.c:54-61`; `copy_from_user` is a `memcpy` under an error frame `src/unix/unix.c:120-144` |

### Address layout (x86_64)

| Range | Use | Evidence |
| --- | --- | --- |
| `0` – `0x7fffffffffff` | User (`USER_LIMIT`) | `src/x86_64/machine.h:13` |
| `0x100000000` – `USER_LIMIT` | Process `mmap` area | `src/unix/unix_internal.h:69-73` |
| below `3 GiB` | ELF load limit; PIE base `DEFAULT_PROG_ADDR` | `src/unix/unix_internal.h:80`, `src/unix/exec.c:393-404` |
| `0xffff800000000000` (`KMEM_BASE`) – `KERNEL_LIMIT` | `kvmem.r`: every kernel heap, tagged regions, bootstrap heap | `src/x86_64/kernel_machine.h:15-17`, `src/kernel/init.c:83-84,127,159-170` |
| `0xffffc00000000000` – `0xffffffff00000000` | Direct map of physical memory (`kvmem.linear`), mapped lazily in 1 GiB steps at runtime | `src/x86_64/kernel_machine.h:18-20`, `src/kernel/init.c:137-145`, `src/kernel/linear_backed_heap.c:90-96` |
| top 2 GiB below `KERNEL_LIMIT` | Kernel image, KASLR-placed | `src/kernel/init.c:68-121` |
| `0xffffffffff600000` | vsyscall page (user-executable, PML4 slot 511) | `src/x86_64/unix_machine.h:223`, `src/x86_64/unix_machine.c:36-44` |

The kernel uses the lower half only during boot. That covers the AP
trampoline at VA 0 (`src/x86_64/mp.c:6,131`, started before any user code at
`src/kernel/init.c:797-799`), the initial identity map, and the identity
image of the kernel. These are unmapped by `reclaim_regions`
(`platform/pc/service.c:120-135`, called at `src/kernel/init.c:296`) and
`platform/pc/service.c:234`. The direct-boot fix-up (`platform/pc/service.c:347-355`)
leaves 2 MiB identity leaves below the kernel without the USER bit. They are
unreachable from user mode and unused by the kernel after boot. A fresh
per-process lower half drops them from every user address space.

PVM narrows `kvmem.r` to an MSR-given PML4 range (`src/x86_64/pvm.c:40-46`,
`platform/pc/service.c:402-403`). It also needs `MSR_PVM_SWITCH_CR3` rewritten
whenever CR3 changes (`src/x86_64/pvm.c:62-69`).

### User mappings

- **Bookkeeping.** `process->vmaps` is a rangemap of `struct vmap` under
  `vmap_lock` (`src/unix/unix_internal.h:460-471,539-541,833-834`). VA
  allocation is a heap over that rangemap (`vmh_alloc`/`vmh_dealloc`
  `src/unix/mmap.c:1525-1564`, installed as `p->virtual` at
  `src/unix/mmap.c:1635-1643`).
- **Creation.** `mmap` adds a vmap only (`src/unix/mmap.c:1266-1452`); pages
  arrive on fault. Eagerly mapped: ELF `static_map` segments
  (`src/unix/exec.c:207-264`), the stack prealloc page (`src/unix/exec.c:17-23`),
  vdso, vvar and pvclock (`src/unix/vdso.c:14-63`), `process_map_physical`
  (`src/unix/mmap.c:746-758`, used by aio), and io_uring rings
  (`src/unix/io_uring.c:289`). The vdso and vvar vmaps are reserved per
  process (`src/unix/mmap.c:1645-1660`). The vsyscall vmap sits in the
  kernel half (`src/unix/mmap.c:1662-1666`).
- **Destruction.** `munmap` and `MAP_FIXED` go through
  `process_remove_range_locked` → `vmap_unmap_page_range`
  (`src/unix/mmap.c:1166-1199`). Anonymous memory is freed by PTE physical
  address (`unmap_and_free_phys`, `src/kernel/page.c:619-636`). File-backed
  memory is released to the page cache (`src/kernel/pagecache.c:2291-2328`).
  Custom mappings are only unmapped. No code tears down a whole process: no
  `free_process` exists.

### Page-fault flow (user address)

1. `common_handler` calls the current context's `fault_handler`
   (`src/x86_64/interrupt.c:223-233`). For threads, syscall contexts and
   process contexts this is the process's `unix_fault_handler`
   (`src/unix/unix.c:422-425,554`, `src/unix/syscall.c:2545`).
2. `unix_fault_handler` looks up the vmap. No vmap, or an address at or above
   `USER_LIMIT`, gives SIGSEGV `SEGV_MAPERR` (`src/unix/unix.c:318-342`). A
   protection fault calls `handle_protection_fault` (`src/unix/unix.c:251-274`).
   That function does COW only for **private file** mappings, through
   `pagecache_node_do_page_cow` (`src/kernel/pagecache.c:1820-1843`, which
   does `unmap`+`map`+`memcpy` on the current table). Anonymous memory has no
   COW.
3. A not-present fault calls `do_demand_page` (`src/unix/mmap.c:423-441`).
   - Anonymous, stack, heap and BSS: `new_zeroed_pages` maps a zeroed page,
     or a THP block up to `thp_max_size` (`src/unix/mmap.c:164-208`).
   - File-backed: a cached page is mapped in place by `mmap_filebacked_page`
     (`src/unix/mmap.c:245-275`). Otherwise a `pending_fault` is queued with
     `async_apply_bh` (`src/unix/mmap.c:431`), and its completion
     `pending_fault_complete` maps the page **from bottom-half context**
     (`src/unix/mmap.c:50-93`).
   - Shared file pages are the cache page itself. Private file pages are
     mapped read-only and copied on write. ELF writable data pages
     (`VMAP_FLAG_PROG|WRITABLE`) get a private copy at fault time
     (`src/unix/mmap.c:316-366`, `src/kernel/pagecache.c:1876-1899`).

### TLB invalidation

`src/kernel/flush.c` keeps one global list of flush entries, numbered by a
generation. `page_invalidate_sync` puts an entry on it, sets its refcount to
`total_processors` and broadcasts an IPI (`src/kernel/flush.c:152-215`). Each
CPU invalidates in its current CR3 and releases the entry
(`src/kernel/flush.c:38-93`). The runloop also catches up on every pass
(`src/kernel/schedule.c:196`), and `vmap_lock` does so while spinning
(`src/kernel/kernel.h:185-197`). The completions that free memory run after
every CPU has released the entry (`src/kernel/flush.c:119-132`).

With several address spaces and no PCID, a broadcast stays correct:
`invlpg` of an address that is absent from the loaded table is harmless, and
a CPU that loads another CR3 flushes all its (non-global) entries.

## 2. Process model

`struct process` (`src/unix/unix_internal.h:518-560`) owns `pid`, `uid`/`gid`,
`brk`/`heap_base`/`heap_map`, `stack_map`, `vdso_base`, `virtual` (the VA
heap), `vmaps` and `vmap_lock`, `mmap_min_addr`, `futices`, `files` and
`fdallocator`, `root_fs`/`cwd_fs`/`cwd`/`process_root`, `threads` (an rbtree
keyed by tid), `signals`/`sigactions`/`signalfds`, POSIX timers, itimers,
`cpu_timers`, aio, `rlimit_stack`, `syscalls` and a `fault_handler` closure.
It has no parent, children, exit status, refcount or page-table root.

`struct thread` (`src/unix/unix_internal.h:325-377`) owns the user `context`
(its frame holds all registers and the extended state), `syscall` (the
syscall context it is borrowing), `task` for the scheduler, `p`, `tid`,
`clear_tid`, `robust_list`, `thread_bq`, its signal state and mask, and
`signal_stack`.

Contexts: user threads (`CONTEXT_TYPE_THREAD`). One syscall context per CPU,
taken at syscall entry (`src/x86_64/crt0.s:220-223`) and bound to the thread
by `sc->t` (`src/unix/syscall.c:2544`). Process contexts for io_uring and aio
(`src/unix/unix.c:529-556`). Kernel contexts for the runloop and bottom
halves (`src/kernel/kernel.c:90-180`).

### Singletons that assume one process

| Global / behavior | Where | Needed change |
| --- | --- | --- |
| `processes[MAX_PROCESSES=2]`, `unix_process()` returns `processes[1]` | `src/unix/unix.c:16-17,614-615,619-623`; users `src/unix/unix.c:774-775`, `klib/acme.c:657` | pid → process table under a lock; `unix_process()` becomes "the init process" |
| PIDs: the kernel process is pid 1 and the program is pid 2. PIDs and TIDs share one id heap. A leader's tid is never freed. | `src/unix/unix.c:573,685,705`, `src/unix/exec.c:366-367`, `src/unix/thread.c:387-390,459-462` | free pids on reap; make the program pid 1 (G1.2 `pid_identity`, reparent to 1) |
| `exit_group` shuts the VM down, and so does the last thread's `exit` | `src/unix/syscall.c:2171-2187` | only the init process's exit ends the VM; other processes are torn down |
| A default fatal signal halts the VM | `src/unix/signal.c:1029-1088` (`halt_with_code` at 1087) | kill the process |
| An unhandled SIGSEGV or SIGKILL/SIGSTOP halts the VM | `src/unix/thread.c:177-203` | kill the process |
| A coredump is written at VM shutdown | `src/unix/signal.c:1019-1027,1065` | dump the dying process (switch to its address space) |
| `unix_shutdown` signals `processes[1..]` | `src/unix/unix.c:770-791` | iterate the pid table |
| `mmap_info` is a global that is **reinitialized on every process creation**, including `list_init(&mmap_info.pf_freelist)` | `src/unix/mmap.c:37-46,1609-1626,1668` | initialize once |
| The global `pf_freelist` is guarded by the **per-process** `vmap_lock` | `src/unix/mmap.c:82,98-100` | its own spinlock |
| OOM-kill state is global | `src/unix/unix.c:19-21,151-178` | acceptable (rate limit), or per process |
| One syscall table for every process; pledge, unveil and shmem patch it | `src/unix/unix.c:597`, `src/unix/syscall.c:2481-2482`, `klib/sandbox.c:46`, `klib/shmem.c:44`, `klib/pledge.c:44-48` | not G1 (per-process seccomp is G3) |
| `/proc/self/exe` is one symlink in the root fs to the manifest `program` | `src/unix/special.c:390-410` | per process (G1.3, when `execve` changes the image) |
| argv, envp and the program path are read from the manifest root tuple | `src/unix/exec.c:61-108` | `execve` takes them from user memory (G1.3) |
| Futex table: per process, keyed by the raw user VA. `FUTEX_PRIVATE_FLAG` is ignored. | `src/unix/futex.c:12-53,167,317-323` | already (address space, uaddr) while process = address space. Shared futexes need a (backing object, offset) table (decision "The futex key is not the virtual address") |
| Page cache: one global cache, and `pn->mappings` is a VA-keyed rangemap that asserts no overlap. The `shared_maps`/`private_maps` lists hold VA ranges with no table. | `src/kernel/pagecache.c:50,1714-1742,2331-2345`, `src/kernel/pagecache_internal.h:71-72,35-36,105-111` | each `pagecache_map` records its address space; `mappings` becomes a list filtered by address space |
| `truncate_file_maps(p, …)` unmaps only the caller's mappings | `src/unix/syscall.c:884,1293`, `src/unix/mmap.c:1201-1223` | walk every mapping of the node (via the page cache list) |
| FD_CLOEXEC lives in the shared `fdesc->flags`, not in the fd table | `src/unix/syscall.c:2030-2032`, `src/unix/unix_internal.h:425` | per-descriptor-slot flags (needed for G1.3 close-on-exec) |
| `init_vsyscall` allocates and remaps the vsyscall page on every `init_vdso` call | `src/unix/vdso.c:59-62`, `src/x86_64/unix_machine.c:36-64` | once at boot (the kernel half is shared) |
| `fork`, `vfork`, `execve`, `getppid` and `process_vm_*` are unregistered (`-ENOSYS`). `wait4` returns 0. `clone` without `CLONE_THREAD` returns `-ENOSYS`. | `src/unix/syscall.c:2539`, `src/x86_64/unix_machine.c:260`, `src/unix/thread.c:57-60` | G1.1 to G1.3 |
| `kill` targets only the caller's own pid | `src/unix/signal.c:612-631` (617) | G1.4 |

## 3. User memory touched outside the owning context

What is already safe: every blockq action is a contextual closure. It runs in
the blocked syscall context through `context_apply`/`context_apply_1`
(`src/unix/blockq.c:52-62,173-246`, `src/kernel/schedule.c:152-183`).
`io_uring` and `aio` operations run in a `process_context`
(`src/unix/io_uring.c:600-675`, `src/unix/aio.c:238-242`). So pipe, socket,
futex, eventfd, timerfd, signalfd, tun and blocking reads copy user data in
the right context, provided the context's resume hook loads its address
space. The table lists the sites where that does not hold.

| Site | Runs in | User access | Change |
| --- | --- | --- | --- |
| `thread_return` → `dispatch_signals` → `setup_sigframe`, before `context_switch` | kernel context (runloop task) | writes the signal frame on the user stack | switch to `t->p`'s address space at the top of `thread_return` (`src/unix/thread.c:246-256`) |
| `epoll_wait_notify` | notifier's context: another process's syscall, or a bottom half | writes `epoll_event` into the waiter's user array (`src/unix/poll.c:391-430`, buffer wrapped at `568`) | stage in a kernel buffer in `epoll_blocked`; copy out in `epoll_wait_bh` (contextual, `poll.c:578`) |
| `select_notify` | same | sets bits in the user `fd_set`s (`src/unix/poll.c:705-730`, wrapped at `905-909`) | kernel bitmaps; copy out in `select_bh` |
| `poll_notify` | same | writes `revents` into the user `pollfd` array (`src/unix/poll.c:951-975`, wrapped at `1086`) | kernel array; copy out in `poll_bh` |
| `pagecache_read_sg_finish` | I/O completion: a bottom half, or another context that filled the page | `sg_fault_in` + `sg_copy_from_buf` into user sg buffers (`src/kernel/pagecache.c:1541-1575`; sg holds user VAs, `src/unix/filesystem.c:111-117`) | when `user_ctx != ctx`, temporarily load `user_ctx`'s address space (`use_address_space`/`restore_address_space`, below) |
| `pagecache_write_sg_finish` | same | `sg_fault_in` + `sg_copy_to_buf` from user buffers (`src/kernel/pagecache.c:592-650`) | same, using `saved_ctx` |
| `aio_complete` | I/O completion context | writes `io_event` through the **user VA** of the ring (`src/unix/aio.c:138-158`); `aio_free` unmaps it (`aio.c:71-80`) | keep a kernel VA for the ring; unmap through the owner's address space |
| `pending_fault_complete` → `demand_page_internal` | bottom half | maps pages into the table and reads PTEs (`src/unix/mmap.c:50-93`, `245-275`, `183`, `206`) | pass the explicit address space (`pf->p->as`) |
| `exec_elf_finish`, `static_map`, `build_exec_stack`, `load_interp_complete` | kernel boot context; FS completion | maps and writes the new process's stack, BSS tail and zero fill (`src/unix/exec.c:17-23,207-264,318-346,357-475`) | build the image with the new address space loaded |
| O_DIRECT `physical_from_virtual(user va)`; virtio translates `sgb->buf` | syscall context; the storage submit path | walks `pagebase` (`src/unix/filesystem.c:137-151`, `src/virtio/virtio_storage.c:215`) | translate with the owner's root in `file_io_init_internal` and carry physical addresses; pin pages (pre-existing munmap-during-DMA race) |
| Page cache scans: `pagecache_scan_old_maps`, `pagecache_scan_shared_map`, `pagecache_node_unmap_pages` | timer and memory cleaner (kernel context); the caller's syscall | `traverse_ptes` over the VA in `pagecache_map`/`v` (`src/kernel/pagecache.c:896-903,1668-1671,2264-2328`) | walk with `pcm->as` |
| `exit_thread`: `clear_tid` write and `wake_robust_list` | the exiting thread's syscall context today | user writes (`src/unix/thread.c:497-507`, `src/unix/futex.c:340-372`) | safe while the exit runs in the dying process. A kill from another process (G1.4) must run them in a target context, or skip them once the address space is gone |
| `coredump` | shutdown handler (kernel) | reads the whole user image (`src/unix/coredump.c:76`, `src/unix/signal.c:1019-1027`) | load the dumped process's address space |
| gdb stub | network bottom half | reads and writes user memory (`src/gdb/gdbstub.c:464`) | load the target's address space (low priority) |

Not affected: `special_files` reads (syscall context,
`src/unix/special.c:261-275`); `strace` (syscall entry, `klib/strace.c:727`);
`umcg` (threads of one process, `klib/umcg.c:383,427`); timers (signal
delivery only); `socket`/`pipe`/`vsock` copies (contextual blockq actions,
`src/unix/pipe.c:133-153`, `src/net/netsyscall.c:693-717,2048`).

## 4. Dependency graph

Legend: **R** reuse as-is, **X** extend in place, **S** split, **N** new.

```text
(a) two address spaces, shared kernel half
  A1 N  address_space {root, refcount, cpus} + kernel_address_space
  A2 X  page.c walks take an explicit root; kernel API unchanged
  A3 X  boot: pre-populate PML4[256..511] with empty PDPTs (x86 page.c)
  A4 N  table-page freelist + lower-half teardown (page.c)
  A5 X  per-CPU current_as; lazy switch in thread/syscall/process resume
        and at the top of thread_return (thread.c, syscall.c, unix.c)
  A6 X  user-VA call sites take the AS (mmap.c, exec.c, vdso.c, aio.c,
        io_uring.c, syscall.c brk, filesystem.c O_DIRECT, pagecache.c)
  A7 X  pagecache_map carries its AS; pn->mappings rangemap -> list
  A8 X  cross-context user writes fixed (poll.c, pagecache sg, aio ring)
  A9 X  globals: mmap_info init once + pf lock; processes[] -> pid table;
        vsyscall once; init process gets its own AS
        A1 -> A2 -> A3 -> A4
        A2 -> A6 -> A7
        A1 -> A5 -> A8
        A9 independent of A2..A8, before (b)
  shootdown: R (broadcast is correct without PCID; targeting is P.1)

(b) fork, eager copy            needs (a)
  B1 X  clone(SIGCHLD) dispatch in clone_internal (thread.c)
  B2 N  process_fork: copy creds, cwd, root, sigactions, fd table, limits
  B3 N  vmap copy (allocate_vmap_locked keeps fd refs) + page copy
  B4 X  child thread from clone_frame_pstate, rax = 0
  B5 X  non-init exit: close fds, unmap all vmaps, release AS, free
        process (no VM shutdown)
        B2 -> B3 -> B4 ; B5 needs A4

(c) copy-on-write               needs (b)
  C1 N  shared-page counts for anonymous physical pages (cow_pages)
  C2 X  fork marks private writable PTEs read-only in both ASes;
        THP blocks split first (split_mapping)
  C3 X  handle_protection_fault: anonymous COW branch
  C4 X  unmap_and_free_phys: decrement shared pages instead of freeing
  C5 X  MAP_SHARED|MAP_ANONYMOUS backed by a tmpfs node (shared after fork;
        gives shared futexes a backing-object key)
        C1 -> C2 -> C3 ; C1 -> C4 ; C5 independent

(d) execve via the ELF loader   needs (b); (c) for vfork+exec
  D1 S  exec_elf_finish -> load into an existing process + new AS;
        argv/envp copied from user before teardown
  D2 S  fdesc->flags: FD_CLOEXEC moves to the fd-table slot
  D3 X  kill other threads, swap p->as, release old AS, reset handlers,
        per-process /proc/self/exe
        D1 -> D3 ; D2 -> D3

(e) wait4 / exit / zombies      needs (b)
  E1 X  process: parent, children, exit_status, state, wait blockq
  E2 X  exit paths (syscall.c exit/exit_group, signal.c default action,
        thread.c stop conditions) kill the process; init exit -> VM exit
  E3 N  wait4/waitid (WNOHANG, ECHILD), reparent to pid 1, SIGCHLD
  E4 S  process -> process + process_vm (vmaps, vmap_lock, virtual, brk,
        heap/stack maps, vdso_base, futices, as) shared by CLONE_VM
        (vfork, posix_spawn)
        E1 -> E2 -> E3 ; E4 before vfork
```

Reused as-is: `struct vmap` and its rangemap; `allocate_vmap_locked` (keeps
fd refs, `src/unix/mmap.c:583-650`); the demand-paging handlers;
`clone_frame_pstate` (copies the full general and extended state,
`src/x86_64/kernel_machine.c:62-66`); blockq; the scheduler; `flush.c`;
`pagecache_get_page_if_filled` for sharing cache pages
(`src/kernel/pagecache.c:2119-2138`); `split_mapping`
(`src/kernel/page.c:285-288`); the per-process fault-handler closure.

## 5. Smallest redesign

### Types and functions (G1.1)

```c
/* src/kernel/address_space.h (new) */
typedef struct address_space {
    u64 root;                   /* physical address of the top-level table */
    struct refcount refcount;   /* owners + CPUs that have it loaded */
    bitmap cpus;                /* CPUs that have it loaded */
    closure_struct(thunk, free);
} *address_space;

extern address_space kernel_address_space;   /* boot root, no user mappings */

address_space allocate_address_space(heap h);  /* copies kernel-half entries */
void address_space_reserve(address_space as);
void address_space_release(address_space as);  /* last ref: free lower half */
void address_space_switch(address_space as);   /* lazy: no-op if loaded */
address_space use_address_space(address_space as);   /* returns previous */
void restore_address_space(address_space prev);

void address_space_map(address_space as, u64 v, physical p, u64 len, pageflags f);
void address_space_unmap(address_space as, u64 v, u64 len);
void address_space_unmap_and_free_phys(address_space as, u64 v, u64 len);
void address_space_update_map_flags(address_space as, u64 v, u64 len, pageflags f);
void address_space_remap_pages(address_space as, u64 vnew, u64 vold, u64 len);
boolean address_space_traverse_ptes(address_space as, u64 v, u64 len, entry_handler eh);
physical address_space_physical(address_space as, u64 v);

/* arch hook, src/<arch>/page.c */
void arch_load_address_space(u64 root);   /* x86: mov cr3 (+ PVM MSR) */

/* struct cpuinfo (src/kernel/kernel.h:90) */
address_space current_as;

/* struct process (src/unix/unix_internal.h:518) */
address_space as;

/* struct pagecache_map (src/kernel/pagecache_internal.h:105) */
address_space as;               /* plus: rmnode n -> list membership */
```

Rules that make invalid states hard to reach:

- The existing `map`/`unmap`/`traverse_ptes`/… keep their signatures and
  assert a kernel-half VA. User VAs go only through `address_space_*`. This
  removes `get_pagetable_base(vaddr)` as an implicit choice of root.
- `map_level` asserts that kernel-half PML4 entries are present and never
  rewritten after boot, which is what lets one copy at
  `allocate_address_space` stay valid forever.
- A CPU holds a reference on its loaded address space. It is dropped when
  the CPU switches away, or when the runloop goes idle (`kernel_sleep`,
  `src/kernel/schedule.c:31-43`) while it holds the last reference. Tables
  are freed only on the last reference, so no CR3 can point at freed tables.
- `use_address_space` is legal only in windows that do not suspend
  (spinlock-held copies). The resume hooks overwrite CR3, so a suspended
  borrow would silently lose it.
- Thread, syscall and process contexts switch in their resume hooks
  (`src/unix/thread.c:227`, `src/unix/syscall.c:2491` after `sc->t` is set
  at `2544`, `src/unix/unix.c:522`). Kernel contexts never switch.

### Per-step size (estimates)

| Step | Files | ~LoC |
| --- | --- | --- |
| A1–A5 address-space core | `src/kernel/address_space.{c,h}` (new), `src/kernel/page.{c,h}`, `src/x86_64/page.c`, `src/x86_64/page_machine.h`, `src/kernel/kernel.h`, `src/kernel/schedule.c`, `src/unix/{thread,syscall,unix}.c`, `src/x86_64/pvm.c`, `src/{aarch64,riscv64}/page.c` + `page_machine.h` | 450 |
| A6–A7 user call sites, page cache registry | `src/unix/{mmap,exec,vdso,aio,io_uring,syscall,filesystem}.c`, `src/kernel/pagecache{.c,_internal.h}`, `src/x86_64/unix_machine.c` | 400 |
| A8 cross-context writes | `src/unix/poll.c`, `src/kernel/pagecache.c`, `src/unix/aio.c` | 250 |
| A9 globals | `src/unix/{unix,mmap,vdso}.c`, `src/unix/unix_internal.h`, `klib/acme.c` | 120 |
| B fork (eager) + non-init exit | `src/unix/{thread,unix,mmap,syscall}.c`, `src/unix/unix_internal.h` | 500 |
| vm_isolation test | `test/runtime/vm_isolation.{c,manifest}`, `test/runtime/Makefile`, `Makefile` | 250 |
| **G1.1 total** | about 30 | **about 2,000** |
| C COW (G1.2) | `src/unix/{mmap,unix}.c`, `src/kernel/page.c`, `klib/shmem.c` or `src/unix/mmap.c` for C5 | 450 |
| E wait/zombies/vfork (G1.2, G1.4) | `src/unix/{unix,syscall,signal,thread}.c`, `src/x86_64/unix_machine.c` | 700 |
| D execve (G1.3) | `src/unix/{exec,syscall,special}.c` | 500 |

### Risks

| Risk | Evidence | Mitigation |
| --- | --- | --- |
| The page cache registry is VA-keyed and asserts no overlap; scans walk the wrong table | `src/kernel/pagecache.c:1727-1732,896-903,1668-1671,2275,2318` | A7 before any fork. The vm_isolation child inherits read-only text mappings, so the test exercises it |
| The cross-context inventory comes from grep and may miss a site; a wrong-address-space write to a present page does not fault | §3 | Debug-build check in `unix_fault_handler` and `use_fault_handler` borrowers: `current_cpu()->current_as == p->as`, else halt. Review every `use_fault_handler` and `context_set_err` outside syscall entry (`rg` over `src/` and `klib/`) |
| Lazy CR3 lifetime: freeing tables still loaded on another CPU; a borrow across a suspend | §5 rules | Per-CPU reference; borrows asserted non-suspending (`context_pre_suspend` asserts no active borrow) |
| Shootdown correctness depends on no PCID and no global pages | `src/x86_64/page_machine.h:3-13`, `src/kernel/flush.c:192` | Keep broadcast. PCID and targeting move together in P.1 |
| Huge pages | THP anonymous blocks (`src/unix/mmap.c:168-181`), 1 GiB direct-map blocks (`src/x86_64/page.c:98-104`) | Eager fork copies whole blocks. COW splits them first. Kernel blocks sit under pre-populated PDPTs and are never copied |
| Low-memory guests | 256 pre-populated PDPT pages = 1 MiB; `LOW_MEMORY_THRESHOLD` 64 MiB (`src/config.h:71`) | Accept (1.6 % at 64 MiB). The alternative (copy on demand) is more code |
| vsyscall exposes kernel text to user mode | `update_map_flags` on `vsyscall_start..end` with USER+EXEC (`src/x86_64/unix_machine.c:59-63`) | Shared read-only and executable in every address space: no cross-process leak. Out of scope, but the "kernel address faults" test must avoid those pages |
| aarch64 and riscv64 must keep compiling; no cross GCC on this host | `ls /usr/bin` has only `qemu-aarch64` and `qemu-system-aarch64` | Implement `arch_load_address_space` for both (TTBR0 / satp) and leave `fork` x86_64-only (`-ENOSYS` elsewhere). Builds stay unverified until a cross toolchain is installed |
| PVM: CR3 changes need `MSR_PVM_SWITCH_CR3` | `src/x86_64/pvm.c:62-69` | Write it in `arch_load_address_space` when `ci->m.pvm`. Untested (the harness is KVM) |
| One global `pt_lock` for all tables | `src/kernel/page.c:6` | Correct; contention is a P item |
| Fork of custom or shared-anonymous mappings | io_uring and aio rings, `process_map_physical` (`src/unix/mmap.c:746-758`), `MAP_SHARED\|MAP_ANONYMOUS` is plain anonymous (`src/unix/mmap.c:1278-1279,1344-1347`) | G1.1 `fork` returns `-ENOSYS` when the parent has such a vmap (vdso, vvar and vsyscall excepted), with a test. C5 and G1.2 lift it |

## 6. G1.1 slice plan

Three commits, each passing its own checks:

1. **`feat(nanos)`: per-process page tables, one process.** A1–A7 and A9.
   The program gets its own address space; the kernel root has none. Check:
   `dev/test.sh harness` and the upstream runtime suite stay at
   **Baseline** on 1 and 2 vCPUs. This proves the resume hooks, the
   exec-into-address-space path and the page-cache registry with no fork
   yet.
2. **`fix(nanos)`: no foreign-context user access.** A8 plus the debug
   address-space check, with a runtime test: process A waits in
   `epoll_wait`, `select` and `poll` while process B writes the pipe. This
   needs commit 3's fork, so it lands after 3, or 2 and 3 are one commit.
3. **`feat(nanos)`: `fork` with eager copy.** B1–B5, plus the
   `-DVM_ISOLATION_SELFTEST_BREAK` path (the child reserves the parent's
   address space and skips the copy).

`test/runtime/vm_isolation.c` (same binary natively and on Nanos):

- The parent maps an anonymous private page at a fixed address `A`, fills it
  with `P` and forks. The child fills `A` with `C`.
- They alternate over two pipes for 10,000 rounds with `sched_yield`; each
  checks its own pattern every round.
- The child writes `A` (its own copy only). It tries `/proc/self/mem`
  (`ENOENT` on Nanos, since none exists) and `process_vm_writev(getppid(), …)`
  (`ENOSYS` on Nanos; `EPERM` natively, because Yama `ptrace_scope` = 1 on
  this host). The parent's later pattern check proves `P` survived.
- Parent → child `process_vm_writev` is **allowed natively** under Yama 1
  (the child is a descendant). The child calls `prctl(PR_SET_DUMPABLE, 0)`
  first, so the native result is `EPERM`. Nanos must accept
  `PR_SET_DUMPABLE`.
- Both read and write `0xffff800000000000` and expect SIGSEGV (not the
  vsyscall page).
- The child reports over the pipe and exits. The parent sees EOF (proving
  that the child's fd teardown ran), then prints the terminal record. There
  is no `wait4` in G1.1.

Acceptance as in plan.md: `dev/test.sh runtime vm_isolation --smp 1` and
`--smp 2` pass 20 consecutive runs each; the native run passes; the BREAK
build fails.

### Owner decisions this note needs

1. **Owns list.** G1.1 as planned owns `src/unix/{unix.c,unix_internal.h,thread.c,mmap.c}`,
   `src/kernel/page*`, `src/x86_64/page*` and the scheduler. The design also
   needs `src/unix/{syscall,exec,vdso,poll,aio,io_uring,filesystem}.c`,
   `src/kernel/{kernel.h,address_space.{c,h}}`,
   `src/x86_64/{unix_machine.c,pvm.c}`, `src/{aarch64,riscv64}/page*`,
   `klib/acme.c`, `Makefile` and `test/runtime/Makefile` (the `RUNTIME_TESTS`
   entry). The BREAK build also needs a CFLAGS hook: `rules.mk` has none
   (`rules.mk:62-106`; `TARGET_CFLAGS` is the clang target), and a
   command-line `CFLAGS` would replace the makefile's own flags.
2. **Shootdown targeting.** The prescribed bound says "shootdown targets only
   the CPUs that have the address space loaded". Without PCID, a broadcast is
   correct and targeting only saves time. The flush protocol counts
   `total_processors` (`src/kernel/flush.c:159-160,195`), so targeting means
   per-entry target counts. This note recommends keeping broadcast through G1
   (Charter: parity first) and doing targeting with PCID in P.1, while G1.1
   maintains `as->cpus`.
3. **Interim `fork` limits.** `-ENOSYS` when the parent has custom or
   `MAP_SHARED` anonymous mappings (until G1.2), and `fork` x86_64-only.
