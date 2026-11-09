#include <unix_internal.h>
#include <seccomp.h>
#include <filesystem.h>
#include <storage.h>

enum procfs_kind {
    PROCFS_ROOT,
    PROCFS_STATIC,
    PROCFS_SELF,
    PROCFS_THREAD_SELF,
    PROCFS_PID,
    PROCFS_STATUS,
    PROCFS_CMDLINE,
    PROCFS_STAT,
    PROCFS_MAPS,
    PROCFS_EXE,
    PROCFS_FD_DIR,
    PROCFS_FD,
    PROCFS_FDINFO_DIR,
    PROCFS_FDINFO,
    PROCFS_TASK_DIR,
    PROCFS_TID,
    PROCFS_TID_STATUS,
};

typedef struct procfs_node {
    tuple md;
    inode ino;
    enum procfs_kind kind;
    int pid;
    int n;
    closure_struct(spec_file_open, open);
} *procfs_node;

static struct procfs {
    struct filesystem fs;
    table by_md;
    table by_ino;
    inode next_ino;
    procfs_node root;
} procfs;

typedef struct procfs_file {
    struct special_file_wrapper w;
    closure_struct(file_io, read);
    closure_struct(fdesc_events, events);
    closure_struct(fdesc_close, close);
    enum procfs_kind kind;
    int pid;
    int n;
} *procfs_file;

static heap procfs_heap(void)
{
    return heap_locked(get_kernel_heaps());
}

static boolean procfs_kind_is_dir(enum procfs_kind kind)
{
    switch (kind) {
    case PROCFS_ROOT:
    case PROCFS_PID:
    case PROCFS_FD_DIR:
    case PROCFS_FDINFO_DIR:
    case PROCFS_TASK_DIR:
    case PROCFS_TID:
        return true;
    default:
        return false;
    }
}

static boolean procfs_kind_is_generated_file(enum procfs_kind kind)
{
    switch (kind) {
    case PROCFS_STATUS:
    case PROCFS_CMDLINE:
    case PROCFS_STAT:
    case PROCFS_MAPS:
    case PROCFS_FDINFO:
    case PROCFS_TID_STATUS:
        return true;
    default:
        return false;
    }
}

static procfs_node procfs_node_of(tuple md)
{
    return md ? table_find(procfs.by_md, md) : 0;
}

static boolean process_reaped(process p)
{
    return list_empty(&p->l);
}

static procfs_node procfs_node_new(procfs_node parent, symbol name, enum procfs_kind kind,
                                   int pid, int n);

static void procfs_node_destroy(procfs_node node);

closure_function(1, 2, boolean, procfs_collect_children,
                 vector, v,
                 value k, value v)
{
    vector_push(bound(v), v);
    return true;
}

static void procfs_destroy_children(tuple c)
{
    vector v = allocate_vector(procfs_heap(), 8);
    assert(v != INVALID_ADDRESS);
    iterate(c, stack_closure(procfs_collect_children, v));
    tuple child;
    vector_foreach(v, child) {
        procfs_node cn = procfs_node_of(child);
        if (cn)
            procfs_node_destroy(cn);
    }
    deallocate_vector(v);
}

static void procfs_node_destroy(procfs_node node)
{
    tuple md = node->md;
    tuple c = children(md);
    if (c) {
        procfs_destroy_children(c);
        set(md, sym(children), 0);
        deallocate_value(c);
    }
    tuple parent = get_tuple(md, sym_this(".."));
    if (parent && parent != md) {
        tuple pc = children(parent);
        symbol name = pc ? tuple_get_symbol(pc, md) : 0;
        if (name)
            set(pc, name, 0);
    }
    set(md, sym_this(".."), 0);
    set(md, sym(special), 0);
    value v = get(md, sym(linktarget));
    if (v) {
        set(md, sym(linktarget), 0);
        deallocate_value(v);
    }
    v = get(md, sym(nlink));
    if (v) {
        set(md, sym(nlink), 0);
        deallocate_value(v);
    }
    set(md, sym(readonly), 0);
    set(md, sym(procfd), 0);
    table_set(procfs.by_md, md, 0);
    table_set(procfs.by_ino, pointer_from_u64(node->ino), 0);
    deallocate_value(md);
    deallocate(procfs_heap(), node, sizeof(*node));
}

static void procfs_set_linktarget(procfs_node node, buffer target)
{
    value old = get(node->md, sym(linktarget));
    set(node->md, sym(linktarget), target);
    if (old)
        deallocate_value(old);
}

static void procfs_set_nlink(procfs_node node, u64 nlink)
{
    value old = get(node->md, sym(nlink));
    set(node->md, sym(nlink), value_from_u64(nlink));
    if (old)
        deallocate_value(old);
}

static procfs_node procfs_child(procfs_node parent, symbol name)
{
    tuple c = children(parent->md);
    return c ? procfs_node_of(get_tuple(c, name)) : 0;
}

static void procfs_buffer_append_number(buffer b, u64 n)
{
    print_number(b, n, 10, 0, false);
}

static void procfs_refresh(procfs_node node);

closure_function(1, 2, boolean, procfs_prune_each,
                 vector, stale,
                 value k, value v)
{
    procfs_node cn = procfs_node_of(v);
    if (cn)
        vector_push(bound(stale), cn);
    return true;
}

static boolean procfs_int_in(vector v, int n)
{
    void *x;
    vector_foreach(v, x) {
        if ((int)u64_from_pointer(x) == n)
            return true;
    }
    return false;
}

static void procfs_sync_numbered(procfs_node dir, vector want, enum procfs_kind kind, int pid)
{
    vector existing = allocate_vector(procfs_heap(), 8);
    assert(existing != INVALID_ADDRESS);
    iterate(children(dir->md), stack_closure(procfs_prune_each, existing));
    procfs_node cn;
    vector_foreach(existing, cn) {
        if ((cn->kind == kind) && !procfs_int_in(want, cn->n))
            procfs_node_destroy(cn);
    }
    deallocate_vector(existing);
    void *x;
    vector_foreach(want, x) {
        int n = u64_from_pointer(x);
        symbol name = intern_u64(n);
        if (!procfs_child(dir, name))
            procfs_node_new(dir, name, kind, (kind == PROCFS_PID) ? n : pid, n);
    }
}

static void procfs_fds(process p, vector v)
{
    process_lock(p);
    fdtable fdt = p->fdtable;
    if (fdt)
        fdtable_reserve(fdt);
    process_unlock(p);
    if (!fdt)
        return;
    spin_lock(&fdt->lock);
    for (int fd = 0; fd < vector_length(fdt->files); fd++) {
        if (vector_get(fdt->files, fd))
            vector_push(v, pointer_from_u64((u64)fd));
    }
    spin_unlock(&fdt->lock);
    fdtable_release(fdt);
}

closure_function(1, 1, boolean, procfs_tid_each,
                 vector, v,
                 rbnode n)
{
    thread t = struct_from_field(n, thread, n);
    vector_push(bound(v), pointer_from_u64((u64)t->tid));
    return true;
}

static void procfs_tids(process p, vector v)
{
    spin_lock(&p->threads_lock);
    rbtree_traverse(p->threads, RB_INORDER, stack_closure(procfs_tid_each, v));
    spin_unlock(&p->threads_lock);
}

static void procfs_refresh(procfs_node node)
{
    vector want;
    process p;
    switch (node->kind) {
    case PROCFS_ROOT:
        want = allocate_vector(procfs_heap(), 16);
        assert(want != INVALID_ADDRESS);
        process_list_pids(want);
        procfs_sync_numbered(node, want, PROCFS_PID, 0);
        deallocate_vector(want);
        break;
    case PROCFS_FD_DIR:
    case PROCFS_FDINFO_DIR:
    case PROCFS_TASK_DIR:
        p = process_get(node->pid);
        if (!p)
            return;
        want = allocate_vector(procfs_heap(), 16);
        assert(want != INVALID_ADDRESS);
        if (node->kind == PROCFS_TASK_DIR) {
            procfs_tids(p, want);
            procfs_sync_numbered(node, want, PROCFS_TID, node->pid);
            procfs_set_nlink(node, vector_length(want) + 2);
        } else {
            procfs_fds(p, want);
            procfs_sync_numbered(node, want,
                                 (node->kind == PROCFS_FD_DIR) ? PROCFS_FD : PROCFS_FDINFO,
                                 node->pid);
        }
        process_release(p);
        deallocate_vector(want);
        break;
    case PROCFS_SELF:
    case PROCFS_THREAD_SELF: {
        buffer b = allocate_string(24);
        assert(b != INVALID_ADDRESS);
        procfs_buffer_append_number(b, current->p->pid);
        if (node->kind == PROCFS_THREAD_SELF) {
            buffer_write_cstring(b, "/task/");
            procfs_buffer_append_number(b, current->tid);
        }
        procfs_set_linktarget(node, b);
        break;
    }
    case PROCFS_EXE:
        p = process_get(node->pid);
        if (!p)
            return;
        buffer b = allocate_string(64);
        assert(b != INVALID_ADDRESS);
        process_lock(p);
        if (p->exe)
            buffer_write(b, buffer_ref(p->exe, 0), buffer_length(p->exe));
        process_unlock(p);
        process_release(p);
        procfs_set_linktarget(node, b);
        break;
    default:
        break;
    }
}

static boolean procfs_node_alive(procfs_node node)
{
    if (node->pid == 0)
        return true;
    process p = process_get(node->pid);
    if (!p)
        return false;
    boolean alive = !process_reaped(p);
    if (alive && (node->kind == PROCFS_TID || node->kind == PROCFS_TID_STATUS)) {
        thread t = thread_from_tid(p, node->n);
        if (t == INVALID_ADDRESS)
            alive = false;
        else
            thread_release(t);
    }
    if (alive && (node->kind == PROCFS_FD || node->kind == PROCFS_FDINFO)) {
        fdtable fdt;
        process_lock(p);
        fdt = p->fdtable;
        if (fdt)
            fdtable_reserve(fdt);
        process_unlock(p);
        if (fdt) {
            spin_lock(&fdt->lock);
            alive = vector_get(fdt->files, node->n) != 0;
            spin_unlock(&fdt->lock);
            fdtable_release(fdt);
        } else {
            alive = false;
        }
    }
    process_release(p);
    return alive;
}

static void procfs_cap_line(buffer b, sstring name, u64 caps)
{
    bprintf(b, "%s:\t", name);
    print_number(b, caps, 16, 16, false);
    push_u8(b, '\n');
}

static void procfs_thread_name(process p, int tid, char *name)
{
    zero(name, 16);
    thread t = thread_from_tid(p, tid);
    if (t == INVALID_ADDRESS) {
        spin_lock(&p->threads_lock);
        rbnode n = rbtree_find_first(p->threads);
        if (n != INVALID_ADDRESS)
            runtime_memcpy(name, struct_from_field(n, thread, n)->name, 15);
        spin_unlock(&p->threads_lock);
        return;
    }
    runtime_memcpy(name, t->name, 15);
    thread_release(t);
}

static int procfs_thread_count(process p)
{
    vector v = allocate_vector(procfs_heap(), 8);
    assert(v != INVALID_ADDRESS);
    procfs_tids(p, v);
    int n = vector_length(v);
    deallocate_vector(v);
    return n;
}

static void procfs_status(buffer b, process p, int tid)
{
    char name[16];
    procfs_thread_name(p, tid, name);
    boolean zombie = (p->state == PROCESS_ZOMBIE);
    int ppid = process_getppid(p);
    bprintf(b, "Name:\t%s\n", sstring_from_cstring(name, sizeof(name)));
    bprintf(b, "State:\t%s\n", zombie ? ss("Z (zombie)") : ss("R (running)"));
    bprintf(b, "Tgid:\t%d\nNgid:\t0\nPid:\t%d\nPPid:\t%d\nTracerPid:\t0\n", p->pid, tid, ppid);
    process_lock(p);
    struct creds *c = &p->creds;
    bprintf(b, "Uid:\t%d\t%d\t%d\t%d\n", c->uid, c->euid, c->suid, c->fsuid);
    bprintf(b, "Gid:\t%d\t%d\t%d\t%d\n", c->gid, c->egid, c->sgid, c->fsgid);
    buffer_write_cstring(b, "Groups:\t");
    for (u32 i = 0; i < c->ngroups; i++)
        bprintf(b, "%d ", c->groups[i]);
    push_u8(b, '\n');
    u64 inh = c->cap_inheritable, prm = c->cap_permitted, eff = c->cap_effective;
    u64 bnd = c->cap_bset, amb = c->cap_ambient;
    boolean nnp = p->no_new_privs;
    int pgid = p->pgid;
    process_unlock(p);
    bprintf(b, "NStgid:\t%d\nNSpid:\t%d\nNSpgid:\t%d\nNSsid:\t%d\n", p->pid, tid, pgid, pgid);
    bprintf(b, "Threads:\t%d\n", zombie ? 1 : procfs_thread_count(p));
    u64 ign = 0, cgt = 0;
    for (int sig = 1; sig <= NSIG; sig++) {
        void *h = p->sigactions[sig - 1].sa_handler;
        if (h == SIG_IGN)
            ign |= 1ull << (sig - 1);
        else if (h != SIG_DFL)
            cgt |= 1ull << (sig - 1);
    }
    procfs_cap_line(b, ss("SigIgn"), ign);
    procfs_cap_line(b, ss("SigCgt"), cgt);
    procfs_cap_line(b, ss("CapInh"), inh);
    procfs_cap_line(b, ss("CapPrm"), prm);
    procfs_cap_line(b, ss("CapEff"), eff);
    procfs_cap_line(b, ss("CapBnd"), bnd);
    procfs_cap_line(b, ss("CapAmb"), amb);
    u8 sec_mode = 0;
    u32 sec_filters = 0;
    thread tt = thread_from_tid(p, tid);
    if (tt != INVALID_ADDRESS) {
        sec_mode = tt->seccomp_mode;
        sec_filters = seccomp_filter_count(tt->seccomp);
        thread_release(tt);
    }
    bprintf(b, "NoNewPrivs:\t%d\nSeccomp:\t%d\nSeccomp_filters:\t%d\n", nnp ? 1 : 0, sec_mode, sec_filters);
}

static void procfs_stat(buffer b, process p)
{
    char name[16];
    procfs_thread_name(p, p->pid, name);
    boolean zombie = (p->state == PROCESS_ZOMBIE);
    bprintf(b, "%d (%s) %c %d %d %d", p->pid, sstring_from_cstring(name, sizeof(name)),
            zombie ? 'Z' : 'R', process_getppid(p), p->pgid, p->pgid);
    for (int field = 7; field <= 52; field++) {
        if (field == 20)
            bprintf(b, " %d", zombie ? 1 : procfs_thread_count(p));
        else
            buffer_write_cstring(b, " 0");
    }
    push_u8(b, '\n');
}

closure_function(3, 1, boolean, procfs_maps_each,
                 buffer, b, vmap, stack, vmap, heap_map,
                 vmap map)
{
    buffer b = bound(b);
    bprintf(b, "%016lx-%016lx %c%c%c%c 00000000 00:00 0", map->node.r.start, map->node.r.end,
            (map->flags & VMAP_FLAG_READABLE) ? 'r' : '-',
            (map->flags & VMAP_FLAG_WRITABLE) ? 'w' : '-',
            (map->flags & VMAP_FLAG_EXEC) ? 'x' : '-',
            (map->flags & VMAP_FLAG_SHARED) ? 's' : 'p');
    if (map == bound(stack))
        buffer_write_cstring(b, "\t[stack]");
    else if (map == bound(heap_map))
        buffer_write_cstring(b, "\t[heap]");
    push_u8(b, '\n');
    return true;
}

static void procfs_maps(buffer b, process p)
{
    process_lock(p);
    process_vm vm = p->vm;
    if (vm)
        process_vm_reserve(vm);
    process_unlock(p);
    if (!vm)
        return;
    process_vm_lock(vm);
    vmap_handler h = stack_closure(procfs_maps_each, b, vm->stack_map, vm->heap_map);
    vmap m = (vmap)rangemap_first_node(vm->vmaps);
    while (m != INVALID_ADDRESS) {
        apply(h, m);
        m = (vmap)rangemap_next_node(vm->vmaps, &m->node);
    }
    process_vm_unlock(vm);
    process_vm_release(vm);
}

static void procfs_octal(buffer b, u64 v)
{
    push_u8(b, '0');
    print_number(b, v, 8, 0, false);
}

static fdesc procfs_get_fdesc(process p, int fd, u32 *fd_flags)
{
    process_lock(p);
    fdtable fdt = p->fdtable;
    if (fdt)
        fdtable_reserve(fdt);
    process_unlock(p);
    if (!fdt)
        return 0;
    spin_lock(&fdt->lock);
    fdesc f = vector_get(fdt->files, fd);
    if (f)
        fetch_and_add(&f->refcnt, 1);
    spin_unlock(&fdt->lock);
    if (f && fd_flags)
        *fd_flags = fdtable_get_fd_flags(fdt, fd);
    fdtable_release(fdt);
    return f;
}

static boolean fdesc_is_file(fdesc f)
{
    switch (f->type) {
    case FDESC_TYPE_REGULAR:
    case FDESC_TYPE_DIRECTORY:
    case FDESC_TYPE_SPECIAL:
    case FDESC_TYPE_SYMLINK:
        return true;
    default:
        return false;
    }
}

static void procfs_fdinfo(buffer b, process p, int fd)
{
    u32 fd_flags = 0;
    fdesc f = procfs_get_fdesc(p, fd, &fd_flags);
    if (!f)
        return;
    u64 pos = fdesc_is_file(f) ? ((file)f)->offset : 0;
    u64 flags = f->flags | ((fd_flags & FD_CLOEXEC) ? O_CLOEXEC : 0);
    bprintf(b, "pos:\t%ld\nflags:\t", pos);
    procfs_octal(b, flags);
    buffer_write_cstring(b, "\nmnt_id:\t0\n");
    fdesc_put(f);
}

static sysreturn procfs_generate(procfs_file pf, buffer b)
{
    process p = process_get(pf->pid);
    if (!p)
        return -ESRCH;
    sysreturn rv = 0;
    if (process_reaped(p)) {
        rv = -ESRCH;
        goto out;
    }
    switch (pf->kind) {
    case PROCFS_STATUS:
        procfs_status(b, p, p->pid);
        break;
    case PROCFS_TID_STATUS:
        procfs_status(b, p, pf->n);
        break;
    case PROCFS_CMDLINE:
        process_lock(p);
        if (p->cmdline)
            buffer_write(b, buffer_ref(p->cmdline, 0), buffer_length(p->cmdline));
        process_unlock(p);
        break;
    case PROCFS_STAT:
        procfs_stat(b, p);
        break;
    case PROCFS_MAPS:
        procfs_maps(b, p);
        break;
    case PROCFS_FDINFO:
        procfs_fdinfo(b, p, pf->n);
        break;
    default:
        rv = -EINVAL;
    }
  out:
    process_release(p);
    return rv;
}

closure_func_basic(file_io, sysreturn, procfs_file_read,
                   void *dest, u64 len, u64 offset, context ctx, boolean bh, io_completion completion)
{
    procfs_file pf = struct_from_closure(procfs_file, read);
    file f = &pf->w.f;
    boolean is_file_offset = (offset == infinity);
    buffer b = allocate_buffer(procfs_heap(), 512);
    sysreturn nr;
    if (b == INVALID_ADDRESS) {
        nr = -ENOMEM;
        goto out;
    }
    nr = procfs_generate(pf, b);
    if (nr == 0) {
        context c = get_current_context(current_cpu());
        if (!context_set_err(c)) {
            nr = buffer_read_at(b, is_file_offset ? f->offset : offset, dest, len);
            context_clear_err(c);
        } else {
            nr = -EFAULT;
        }
        if ((nr > 0) && is_file_offset)
            f->offset += nr;
    }
    deallocate_buffer(b);
  out:
    return io_complete(completion, nr);
}

closure_func_basic(fdesc_events, u32, procfs_file_events,
                   thread t)
{
    return EPOLLIN;
}

closure_func_basic(fdesc_close, sysreturn, procfs_file_close,
                   context ctx, io_completion completion)
{
    procfs_file pf = struct_from_closure(procfs_file, close);
    file_release(&pf->w.f);
    return io_complete(completion, 0);
}

closure_func_basic(spec_file_open, sysreturn, procfs_node_open,
                   file f)
{
    procfs_node node = struct_from_closure(procfs_node, open);
    procfs_file pf = (procfs_file)f;
    pf->kind = node->kind;
    pf->pid = node->pid;
    pf->n = node->n;
    f->f.read = init_closure_func(&pf->read, file_io, procfs_file_read);
    f->f.events = init_closure_func(&pf->events, fdesc_events, procfs_file_events);
    f->f.close = init_closure_func(&pf->close, fdesc_close, procfs_file_close);
    return 0;
}

static const struct {
    sstring name;
    enum procfs_kind kind;
} procfs_pid_entries[] = {
    { ss_static_init("status"), PROCFS_STATUS },
    { ss_static_init("cmdline"), PROCFS_CMDLINE },
    { ss_static_init("stat"), PROCFS_STAT },
    { ss_static_init("maps"), PROCFS_MAPS },
    { ss_static_init("exe"), PROCFS_EXE },
    { ss_static_init("fd"), PROCFS_FD_DIR },
    { ss_static_init("fdinfo"), PROCFS_FDINFO_DIR },
    { ss_static_init("task"), PROCFS_TASK_DIR },
};

static procfs_node procfs_node_new(procfs_node parent, symbol name, enum procfs_kind kind,
                                   int pid, int n)
{
    heap h = procfs_heap();
    procfs_node node = allocate(h, sizeof(*node));
    assert(node != INVALID_ADDRESS);
    node->md = allocate_tuple();
    assert(node->md != INVALID_ADDRESS);
    node->ino = procfs.next_ino++;
    node->kind = kind;
    node->pid = pid;
    node->n = n;
    table_set(procfs.by_md, node->md, node);
    table_set(procfs.by_ino, pointer_from_u64(node->ino), node);
    if (procfs_kind_is_dir(kind)) {
        set(node->md, sym(children), allocate_tuple());
        set(node->md, sym_this(".."), parent ? parent->md : node->md);
    }
    if (procfs_kind_is_generated_file(kind)) {
        set(node->md, sym(special), init_closure_func(&node->open, spec_file_open,
                                                      procfs_node_open));
        set(node->md, sym(special_alloc_size), pointer_from_u64(sizeof(struct procfs_file)));
        set(node->md, sym(readonly), null_value);
    }
    if (kind == PROCFS_FD)
        set(node->md, sym(procfd), null_value);
    if (kind == PROCFS_SELF || kind == PROCFS_THREAD_SELF) {
        buffer b = allocate_string(1);
        assert(b != INVALID_ADDRESS);
        push_u8(b, '0');
        set(node->md, sym(linktarget), b);
    }
    if (parent)
        set(children(parent->md), name, node->md);
    if (kind == PROCFS_PID || kind == PROCFS_TID) {
        int count = (kind == PROCFS_PID) ? _countof(procfs_pid_entries) : 1;
        for (int i = 0; i < count; i++)
            procfs_node_new(node, sym_sstring(procfs_pid_entries[i].name),
                            procfs_pid_entries[i].kind == PROCFS_STATUS && kind == PROCFS_TID ?
                            PROCFS_TID_STATUS : procfs_pid_entries[i].kind, pid, n);
    }
    if (kind != PROCFS_SELF && kind != PROCFS_THREAD_SELF)
        procfs_refresh(node);
    return node;
}

static tuple procfs_lookup(filesystem fs, tuple parent, string name)
{
    procfs_node pn = procfs_node_of(parent);
    if (!pn || !procfs_node_alive(pn))
        return 0;
    procfs_refresh(pn);
    tuple t = lookup(parent, intern(name));
    procfs_node node = procfs_node_of(t);
    if (!node || !procfs_node_alive(node))
        return 0;
    if (node != pn)
        procfs_refresh(node);
    return t;
}

static inode procfs_get_inode(filesystem fs, tuple md)
{
    procfs_node node = procfs_node_of(md);
    return node ? node->ino : 0;
}

static tuple procfs_get_meta(filesystem fs, inode n)
{
    procfs_node node = table_find(procfs.by_ino, pointer_from_u64(n));
    if (!node || !procfs_node_alive(node))
        return 0;
    if (procfs_kind_is_dir(node->kind))
        procfs_refresh(node);
    return node->md;
}

static int procfs_get_fsfile(filesystem fs, tuple md, fsfile *f)
{
    *f = 0;
    return procfs_node_of(md) ? 0 : -ENOENT;
}

static int procfs_create(filesystem fs, tuple parent, string name, tuple md, fsfile *f)
{
    return -EACCES;
}

static int procfs_unlink(filesystem fs, tuple parent, string name, tuple md, boolean *destruct_md)
{
    return -EACCES;
}

static int procfs_rename(filesystem fs, tuple old_parent, string old_name, tuple old_md,
                         tuple new_parent, string new_name, tuple new_md, boolean exchange,
                         boolean *destruct_md)
{
    return -EXDEV;
}

static u64 procfs_get_freeblocks(filesystem fs)
{
    return 0;
}

boolean procfs_fd_link(tuple n, int *pid, int *fd)
{
    procfs_node node = procfs_node_of(n);
    if (!node || node->kind != PROCFS_FD || !get(n, sym(procfd)))
        return false;
    *pid = node->pid;
    *fd = node->n;
    return true;
}

static fdesc procfs_fd_target(int pid, int fd, sysreturn *rv)
{
    process self = current->p;
    if (pid != self->pid && !capable(self, CAP_SYS_PTRACE)) {
        *rv = -EACCES;
        return 0;
    }
    process p = process_get(pid);
    if (!p) {
        *rv = -ENOENT;
        return 0;
    }
    fdesc f = procfs_get_fdesc(p, fd, 0);
    process_release(p);
    if (!f)
        *rv = -ENOENT;
    return f;
}

sysreturn procfs_fd_open(int pid, int fd, int flags)
{
    if ((flags & O_NOFOLLOW) && !(flags & O_PATH))
        return -ELOOP;
    sysreturn rv;
    fdesc target = procfs_fd_target(pid, fd, &rv);
    if (!target)
        return rv;
    if (target->type != FDESC_TYPE_REGULAR && target->type != FDESC_TYPE_DIRECTORY) {
        rv = -ENXIO;
        goto out;
    }
    file tf = (file)target;
    filesystem fs = tf->fs;
    filesystem_lock(fs);
    tuple md = fs->get_meta(fs, tf->n);
    if (!md && target->type == FDESC_TYPE_REGULAR && tf->n == fs->get_inode(fs, fs->root))
        md = fs->root;
    if (!md) {
        filesystem_unlock(fs);
        rv = -ENOENT;
        goto out;
    }
    int accmode = flags & O_ACCMODE;
    if (accmode == 3) {
        rv = -EINVAL;
    } else if (accmode != O_RDONLY && target->type == FDESC_TYPE_DIRECTORY) {
        rv = -EISDIR;
    } else if (accmode != O_RDONLY && !(file_meta_perms(current->p, md) & ACCESS_PERM_WRITE)) {
        rv = -EACCES;
    } else {
        fsfile fsf = tf->fsf;
        if (fsf)
            fsfile_reserve(fsf);
        rv = unix_file_new(fs, md, target->type, flags & ~(O_CREAT | O_EXCL | O_TRUNC | O_TMPFILE),
                           fsf);
        if (rv < 0 && fsf)
            fsfile_release(fsf);
    }
    filesystem_put_meta(fs, md);
  out:
    fdesc_put(target);
    return rv;
}

sysreturn procfs_fd_stat(int pid, int fd, struct stat *s)
{
    sysreturn rv;
    fdesc target = procfs_fd_target(pid, fd, &rv);
    if (!target)
        return rv;
    rv = fdesc_stat(target, s);
    fdesc_put(target);
    return rv;
}

sysreturn procfs_fd_readlink(int pid, int fd, buffer b)
{
    sysreturn rv;
    fdesc target = procfs_fd_target(pid, fd, &rv);
    if (!target)
        return rv;
    rv = 0;
    u64 id = u64_from_pointer(target) >> 6;
    switch (target->type) {
    case FDESC_TYPE_REGULAR:
    case FDESC_TYPE_DIRECTORY:
    case FDESC_TYPE_SPECIAL:
    case FDESC_TYPE_SYMLINK: {
        file f = (file)target;
        tuple md = filesystem_get_meta(f->fs, f->n);
        boolean unnamed = (target->type == FDESC_TYPE_REGULAR) && (!md || is_dir(md));
        if (md)
            filesystem_put_meta(f->fs, md);
        if (unnamed) {
            buffer_write_cstring(b, "/memfd: (deleted)");
            break;
        }
        if (!buffer_extend(b, PATH_MAX)) {
            rv = -ENOMEM;
            break;
        }
        int len = file_get_path(f->fs, f->n, buffer_end(b), buffer_space(b));
        if (len > 0)
            buffer_produce(b, len - 1);
        else
            rv = -ENOENT;
        break;
    }
    case FDESC_TYPE_SOCKET:
        bprintf(b, "socket:[%ld]", id);
        break;
    case FDESC_TYPE_PIPE:
        bprintf(b, "pipe:[%ld]", id);
        break;
    case FDESC_TYPE_STDIO:
        buffer_write_cstring(b, "/dev/console");
        break;
    case FDESC_TYPE_EPOLL:
        buffer_write_cstring(b, "anon_inode:[eventpoll]");
        break;
    case FDESC_TYPE_EVENTFD:
        buffer_write_cstring(b, "anon_inode:[eventfd]");
        break;
    case FDESC_TYPE_SIGNALFD:
        buffer_write_cstring(b, "anon_inode:[signalfd]");
        break;
    case FDESC_TYPE_TIMERFD:
        buffer_write_cstring(b, "anon_inode:[timerfd]");
        break;
    case FDESC_TYPE_IORING:
        buffer_write_cstring(b, "anon_inode:[io_uring]");
        break;
    case FDESC_TYPE_INOTIFY:
        buffer_write_cstring(b, "anon_inode:inotify");
        break;
    default:
        bprintf(b, "anon_inode:[%ld]", id);
    }
    fdesc_put(target);
    return rv;
}

boolean procfs_add_static(sstring name, spec_file_open open, u64 alloc_size)
{
    filesystem_lock(&procfs.fs);
    procfs_node node = procfs_node_new(procfs.root, sym_sstring(name), PROCFS_STATIC, 0, 0);
    set(node->md, sym(special), open);
    set(node->md, sym(special_alloc_size), pointer_from_u64(alloc_size));
    filesystem_unlock(&procfs.fs);
    return true;
}

void procfs_init(filesystem root_fs)
{
    heap h = procfs_heap();
    status s = filesystem_init(&procfs.fs, h, 0, 1, true);
    assert(is_ok(s));
    procfs.by_md = allocate_table(h, identity_key, pointer_equal);
    procfs.by_ino = allocate_table(h, identity_key, pointer_equal);
    assert(procfs.by_md != INVALID_ADDRESS && procfs.by_ino != INVALID_ADDRESS);
    procfs.next_ino = 1;
    procfs.fs.lookup = procfs_lookup;
    procfs.fs.get_inode = procfs_get_inode;
    procfs.fs.get_meta = procfs_get_meta;
    procfs.fs.get_fsfile = procfs_get_fsfile;
    procfs.fs.create = procfs_create;
    procfs.fs.unlink = procfs_unlink;
    procfs.fs.rename = procfs_rename;
    procfs.fs.get_freeblocks = procfs_get_freeblocks;
    procfs.root = procfs_node_new(0, 0, PROCFS_ROOT, 0, 0);
    procfs.fs.root = procfs.root->md;
    procfs_node_new(procfs.root, sym(self), PROCFS_SELF, 0, 0);
    procfs_node_new(procfs.root, sym_this("thread-self"), PROCFS_THREAD_SELF, 0, 0);

    int fss = filesystem_mkdirpath(root_fs, 0, ss("/proc"), true);
    assert(fss == 0 || fss == -EEXIST);
    tuple proc_dir;
    filesystem fs = root_fs;
    fss = filesystem_get_node(&fs, root_fs->get_inode(root_fs, filesystem_getroot(root_fs)),
                              ss("/proc"), FS_NODE_FOLLOW, &proc_dir, 0, 0);
    assert(fss == 0);
    inode mount_dir = fs->get_inode(fs, proc_dir);
    filesystem_put_node(fs, proc_dir);
    assert(fs == root_fs);
    fss = filesystem_mount(root_fs, mount_dir, &procfs.fs);
    assert(fss == 0);
    storage_set_procfs_mount(procfs.fs.root, mount_dir);
}
