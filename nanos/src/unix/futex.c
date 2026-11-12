#include <unix_internal.h>

#define FUTEX_BITSET_MATCH_ANY  0xffffffff
#define FUTEX_OP_OPARG_SHIFT    8

struct futex {
    struct futex_key key;
    struct spinlock lock;
    struct list waiters;
    u64 refcount;
};

typedef struct futex_waiter {
    closure_struct(blockq_action, bh);
    blockq bq;
    struct list l;
    struct futex *f;
    thread t;
    u32 bitset;
    boolean timed;
} *futex_waiter;

static struct {
    heap h;
    struct spinlock lock;
    table futices;
} futex_table;

#define futex_lock(f)   spin_lock(&(f)->lock)
#define futex_unlock(f) spin_unlock(&(f)->lock)

static u64 futex_key_hash(void *x)
{
    futex_key k = x;
    return (u64_from_pointer(k->object) >> 4) ^ (k->offset * 0x9e3779b97f4a7c15ull);
}

static boolean futex_key_equal(void *a, void *b)
{
    futex_key ka = a, kb = b;
    return ka->object == kb->object && ka->offset == kb->offset;
}

void init_futices(heap h)
{
    futex_table.h = h;
    spin_lock_init(&futex_table.lock);
    futex_table.futices = allocate_table(h, futex_key_hash, futex_key_equal);
    assert(futex_table.futices != INVALID_ADDRESS);
}

static struct futex *futex_get(futex_key k)
{
    spin_lock(&futex_table.lock);
    struct futex *f = table_find(futex_table.futices, k);
    if (f) {
        f->refcount++;
        spin_unlock(&futex_table.lock);
        futex_key_release(k);
        return f;
    }
    f = allocate(futex_table.h, sizeof(*f));
    if (f != INVALID_ADDRESS) {
        f->key = *k;
        spin_lock_init(&f->lock);
        list_init(&f->waiters);
        f->refcount = 1;
        table_set(futex_table.futices, &f->key, f);
    }
    spin_unlock(&futex_table.lock);
    if (f == INVALID_ADDRESS)
        futex_key_release(k);
    return f;
}

static void futex_put_n(struct futex *f, u64 n)
{
    spin_lock(&futex_table.lock);
    assert(f->refcount >= n);
    f->refcount -= n;
    boolean last = f->refcount == 0;
    if (last)
        table_set(futex_table.futices, &f->key, 0);
    spin_unlock(&futex_table.lock);
    if (last) {
        assert(list_empty(&f->waiters));
        futex_key_release(&f->key);
        deallocate(futex_table.h, f, sizeof(*f));
    }
}

static void futex_put(struct futex *f)
{
    futex_put_n(f, 1);
}

static sysreturn futex_lookup(process p, int *uaddr, boolean private, struct futex **fp)
{
    if (u64_from_pointer(uaddr) & (sizeof(u32) - 1))
        return -EINVAL;
    if (!memory_is_user(uaddr, sizeof(u32)))
        return -EFAULT;
    struct futex_key k;
    sysreturn rv = futex_key_lookup(p, u64_from_pointer(uaddr), private, &k);
    if (rv)
        return rv;
    struct futex *f = futex_get(&k);
    if (f == INVALID_ADDRESS)
        return -ENOMEM;
    *fp = f;
    return 0;
}

static struct futex *futex_lock_waiter(futex_waiter w)
{
    while (true) {
        spin_lock(&futex_table.lock);
        struct futex *f = w->f;
        f->refcount++;
        spin_unlock(&futex_table.lock);
        futex_lock(f);
        if (w->f == f)
            return f;
        futex_unlock(f);
        futex_put(f);
    }
}

closure_func_basic(blockq_action, sysreturn, futex_bh,
                   u64 flags)
{
    futex_waiter w = struct_from_closure(futex_waiter, bh);
    if (!(flags & BLOCKQ_ACTION_BLOCKED)) {
        futex_unlock(w->f);
        return BLOCKQ_BLOCK_REQUIRED;
    }
    struct futex *f = futex_lock_waiter(w);
    sysreturn rv = 0;
    if (list_inserted(&w->l)) {
        list_delete(&w->l);
        if (flags & BLOCKQ_ACTION_NULLIFY)
            rv = w->timed ? -EINTR : -ERESTARTSYS;
        else if (flags & BLOCKQ_ACTION_TIMEDOUT)
            rv = -ETIMEDOUT;
    }
    futex_unlock(f);
    futex_put_n(f, 2);
    thread t = w->t;
    blockq bq = w->bq;
    deallocate(futex_table.h, w, sizeof(*w));
    rv = syscall_return(t, rv);
    deallocate_blockq(bq);
    return rv;
}

static int futex_wake_locked(struct futex *f, int n, u32 bitset)
{
    int woken = 0;
    list_foreach(&f->waiters, l) {
        if (woken >= n)
            break;
        futex_waiter w = struct_from_list(l, futex_waiter, l);
        if (!(w->bitset & bitset))
            continue;
        list_delete(&w->l);
        blockq_wake_one(w->bq);
        woken++;
    }
    return woken;
}

static int futex_requeue_locked(struct futex *f, struct futex *dest, int n)
{
    int moved = 0;
    list_foreach(&f->waiters, l) {
        if (moved >= n)
            break;
        futex_waiter w = struct_from_list(l, futex_waiter, l);
        if (dest != f) {
            list_delete(&w->l);
            list_insert_before(&dest->waiters, &w->l);
            w->f = dest;
        }
        moved++;
    }
    return moved;
}

static sysreturn futex_read(int *uaddr, u32 *val)
{
    context ctx = get_current_context(current_cpu());
    if (context_set_err(ctx))
        return -EFAULT;
    *val = __atomic_load_n((u32 *)uaddr, __ATOMIC_SEQ_CST);
    context_clear_err(ctx);
    return 0;
}

static sysreturn futex_wait(int *uaddr, boolean private, u32 val, u64 timeout_arg, boolean absolute,
                            clock_id clkid, u32 bitset)
{
    if (!bitset)
        return -EINVAL;
    timestamp ts = 0;
    if (timeout_arg) {
        sysreturn rv = user_timespec_get(pointer_from_u64(timeout_arg), &ts);
        if (rv)
            return rv;
    }
    if (!fault_in_user_memory(uaddr, sizeof(u32), false))
        return -EFAULT;
    struct futex *f;
    sysreturn rv = futex_lookup(current->p, uaddr, private, &f);
    if (rv)
        return rv;
    futex_waiter w = allocate(futex_table.h, sizeof(*w));
    if (w == INVALID_ADDRESS) {
        futex_put(f);
        return -ENOMEM;
    }
    w->bq = allocate_blockq(futex_table.h, ss("futex"));
    if (w->bq == INVALID_ADDRESS) {
        deallocate(futex_table.h, w, sizeof(*w));
        futex_put(f);
        return -ENOMEM;
    }
    context ctx = get_current_context(current_cpu());
    __closure(u64_from_pointer(ctx) | CLOSURE_COMMON_CTX_IS_CONTEXT, &w->bh, sizeof(w->bh), futex_bh);
    w->f = f;
    w->t = current;
    w->bitset = bitset;
    w->timed = timeout_arg != 0;
    futex_lock(f);
    u32 cur;
    rv = futex_read(uaddr, &cur);
    if (!rv && cur != val)
        rv = -EAGAIN;
    if (!rv && timeout_arg && !ts)
        rv = -ETIMEDOUT;
    if (rv) {
        futex_unlock(f);
        deallocate_blockq(w->bq);
        deallocate(futex_table.h, w, sizeof(*w));
        futex_put(f);
        return rv;
    }
    list_insert_before(&f->waiters, &w->l);
    return blockq_check_timeout(w->bq, (blockq_action)&w->bh, false, clkid, ts, absolute);
}

static sysreturn futex_wake(process p, int *uaddr, boolean private, int n, u32 bitset)
{
    if (!bitset)
        return -EINVAL;
    struct futex *f;
    sysreturn rv = futex_lookup(p, uaddr, private, &f);
    if (rv)
        return rv;
    futex_lock(f);
    int woken = futex_wake_locked(f, n, bitset);
    futex_unlock(f);
    futex_put(f);
    return woken;
}

static void futex_lock_pair(struct futex *f, struct futex *f2)
{
    if (f == f2)
        futex_lock(f);
    else
        spin_lock_2(&f->lock, &f2->lock);
}

static void futex_unlock_pair(struct futex *f, struct futex *f2)
{
    if (f != f2)
        futex_unlock(f2);
    futex_unlock(f);
}

static sysreturn futex_requeue(int *uaddr, boolean private, int nr_wake, int nr_requeue, int *uaddr2,
                               boolean compare, u32 val3)
{
    if (nr_wake < 0 || nr_requeue < 0)
        return -EINVAL;
    struct futex *f, *f2;
    sysreturn rv = futex_lookup(current->p, uaddr, private, &f);
    if (rv)
        return rv;
    rv = futex_lookup(current->p, uaddr2, private, &f2);
    if (rv) {
        futex_put(f);
        return rv;
    }
    if (compare && !fault_in_user_memory(uaddr, sizeof(u32), false)) {
        rv = -EFAULT;
        goto out;
    }
    futex_lock_pair(f, f2);
    if (compare) {
        u32 cur;
        rv = futex_read(uaddr, &cur);
        if (!rv && cur != val3)
            rv = -EAGAIN;
    }
    int moved = 0;
    if (!rv) {
        rv = futex_wake_locked(f, nr_wake, FUTEX_BITSET_MATCH_ANY);
        moved = futex_requeue_locked(f, f2, nr_requeue);
        rv += moved;
    }
    /* A requeued waiter can wake and drop its references on f2 as soon as f2 is unlocked, so its
     * references must move with it before then. */
    if (moved && f != f2) {
        spin_lock(&futex_table.lock);
        f2->refcount += moved;
        f->refcount -= moved;
        spin_unlock(&futex_table.lock);
    }
    futex_unlock_pair(f, f2);
  out:
    futex_put(f2);
    futex_put(f);
    return rv;
}

static sysreturn futex_wake_op(int *uaddr, boolean private, int nr_wake, int nr_wake2, int *uaddr2,
                               u32 val3)
{
    unsigned int op = (val3 >> 28) & MASK(4);
    unsigned int cmp = (val3 >> 24) & MASK(4);
    int oparg = ((s32)(val3 << 8)) >> 20;
    int cmparg = ((s32)(val3 << 20)) >> 20;
    if (op & FUTEX_OP_OPARG_SHIFT) {
        oparg = 1 << (oparg & 31);
        op &= ~FUTEX_OP_OPARG_SHIFT;
    }
    if (op > FUTEX_OP_XOR || cmp > FUTEX_OP_CMP_GE)
        return -ENOSYS;
    struct futex *f, *f2;
    sysreturn rv = futex_lookup(current->p, uaddr, private, &f);
    if (rv)
        return rv;
    rv = futex_lookup(current->p, uaddr2, private, &f2);
    if (rv) {
        futex_put(f);
        return rv;
    }
    if (!fault_in_user_memory(uaddr2, sizeof(u32), true)) {
        rv = -EFAULT;
        goto out;
    }
    futex_lock_pair(f, f2);
    int oldval;
    context ctx = get_current_context(current_cpu());
    if (context_set_err(ctx)) {
        rv = -EFAULT;
    } else {
        switch (op) {
        case FUTEX_OP_SET:  oldval = __atomic_exchange_n(uaddr2, oparg, __ATOMIC_SEQ_CST); break;
        case FUTEX_OP_ADD:  oldval = __atomic_fetch_add(uaddr2, oparg, __ATOMIC_SEQ_CST); break;
        case FUTEX_OP_OR:   oldval = __atomic_fetch_or(uaddr2, oparg, __ATOMIC_SEQ_CST); break;
        case FUTEX_OP_ANDN: oldval = __atomic_fetch_and(uaddr2, ~oparg, __ATOMIC_SEQ_CST); break;
        default:            oldval = __atomic_fetch_xor(uaddr2, oparg, __ATOMIC_SEQ_CST); break;
        }
        context_clear_err(ctx);
    }
    if (!rv) {
        boolean c;
        switch (cmp) {
        case FUTEX_OP_CMP_EQ: c = oldval == cmparg; break;
        case FUTEX_OP_CMP_NE: c = oldval != cmparg; break;
        case FUTEX_OP_CMP_LT: c = oldval < cmparg; break;
        case FUTEX_OP_CMP_LE: c = oldval <= cmparg; break;
        case FUTEX_OP_CMP_GT: c = oldval > cmparg; break;
        default:              c = oldval >= cmparg; break;
        }
        rv = futex_wake_locked(f, nr_wake, FUTEX_BITSET_MATCH_ANY);
        if (c)
            rv += futex_wake_locked(f2, nr_wake2, FUTEX_BITSET_MATCH_ANY);
    }
    futex_unlock_pair(f, f2);
  out:
    futex_put(f2);
    futex_put(f);
    return rv;
}

sysreturn futex(int *uaddr, int futex_op, int val, u64 val2, int *uaddr2, int val3)
{
    int cmd = futex_op & ~(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);
    boolean private = (futex_op & FUTEX_PRIVATE_FLAG) != 0;
    clock_id clkid = CLOCK_ID_MONOTONIC;
    if (futex_op & FUTEX_CLOCK_REALTIME) {
        if (cmd != FUTEX_WAIT_BITSET)
            return -ENOSYS;
        clkid = CLOCK_ID_REALTIME;
    }
    switch (cmd) {
    case FUTEX_WAIT:
        return futex_wait(uaddr, private, val, val2, false, clkid, FUTEX_BITSET_MATCH_ANY);
    case FUTEX_WAIT_BITSET:
        return futex_wait(uaddr, private, val, val2, true, clkid, val3);
    case FUTEX_WAKE:
        return futex_wake(current->p, uaddr, private, val, FUTEX_BITSET_MATCH_ANY);
    case FUTEX_WAKE_BITSET:
        return futex_wake(current->p, uaddr, private, val, val3);
    case FUTEX_REQUEUE:
        return futex_requeue(uaddr, private, val, val2, uaddr2, false, 0);
    case FUTEX_CMP_REQUEUE:
        return futex_requeue(uaddr, private, val, val2, uaddr2, true, val3);
    case FUTEX_WAKE_OP:
        return futex_wake_op(uaddr, private, val, val2, uaddr2, val3);
    default:
        return -ENOSYS;
    }
}

boolean futex_wake_many_by_uaddr(process p, int *uaddr, int val)
{
    return futex_wake(p, uaddr, false, val, FUTEX_BITSET_MATCH_ANY) > 0;
}

#define FUTEX_WAITERS       0x80000000
#define FUTEX_OWNER_DIED    0x40000000
#define FUTEX_TID_MASK      0x3fffffff
#define ROBUST_LIST_LIMIT   2048

typedef struct robust_list {
    struct robust_list *next;
} *robust_list;

typedef struct robust_list_head {
    robust_list list;
    long futex_offset;
    robust_list list_op_pending;
} *robust_list_head;

static robust_list robust_entry(robust_list l)
{
    return pointer_from_u64(u64_from_pointer(l) & ~1ull);
}

static void robust_futex_owner_died(thread t, robust_list entry, long futex_offset, boolean pending)
{
    u32 *uaddr = (u32 *)((u8 *)entry + futex_offset);
    if ((u64_from_pointer(uaddr) & 3) || !memory_is_user(uaddr, sizeof(*uaddr)))
        return;
    u32 old = __atomic_load_n(uaddr, __ATOMIC_RELAXED);
    while (true) {
        if (pending && old == 0) {
            futex_wake_one_by_uaddr(t->p, (int *)uaddr);
            return;
        }
        if ((old & FUTEX_TID_MASK) != t->tid)
            return;
        u32 new = (old & FUTEX_WAITERS) | FUTEX_OWNER_DIED;
        if (__atomic_compare_exchange_n(uaddr, &old, new, false, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED))
            break;
    }
    if (old & FUTEX_WAITERS)
        futex_wake_one_by_uaddr(t->p, (int *)uaddr);
}

void wake_robust_list(thread t)
{
    robust_list_head h = t->robust_list;
    if (!h || !memory_is_user(h, sizeof(*h)))
        return;
    context ctx = get_current_context(current_cpu());
    if (context_set_err(ctx))
        return;
    robust_list l = robust_entry(h->list);
    long futex_offset = h->futex_offset;
    robust_list pending = robust_entry(h->list_op_pending);
    for (int n = 0; l != (robust_list)h && n < ROBUST_LIST_LIMIT; n++) {
        if (!memory_is_user(l, sizeof(*l)))
            break;
        robust_list next = robust_entry(l->next);
        if (l != pending)
            robust_futex_owner_died(t, l, futex_offset, false);
        l = next;
    }
    if (pending)
        robust_futex_owner_died(t, pending, futex_offset, true);
    context_clear_err(ctx);
}

sysreturn get_robust_list(int pid, void *head, u64 *len)
{
    struct robust_list_head **hp = head;
    if (!memory_is_user(hp, sizeof(*hp)) ||
        !memory_is_user(len, sizeof(*len)))
        return -EFAULT;

    thread t = 0;
    if (pid == 0) {
        t = current;
    } else {
        t = thread_from_tid_ns(current->p, pid, current->p->pid_ns);
        if (t == INVALID_ADDRESS)
            return -ESRCH;
    }
    context ctx = get_current_context(current_cpu());
    if (context_set_err(ctx))
        return -EFAULT;
    *hp = t->robust_list;
    if (pid != 0)
        thread_release(t);
    *len = sizeof(**hp);
    context_clear_err(ctx);
    return 0;
}

sysreturn set_robust_list(void *head, u64 len)
{
    if (len != sizeof(struct robust_list_head))
        return -EINVAL;
    if (!validate_process_memory(current->p, head, len, false))
        return -EFAULT;
    current->robust_list = head;

    return 0;
}
