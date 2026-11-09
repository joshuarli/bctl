#include <unix_internal.h>

struct futex {
    heap h;
    blockq bq;
    struct spinlock lock;
};

#define futex_lock(f)   spin_lock(&(f)->lock)
#define futex_unlock(f) spin_unlock(&(f)->lock)

static u64 futex_key_function(void *x)
{
    return u64_from_pointer(x);
}

static boolean futex_key_equal(void *a, void *b)
{
    return a == b;
}

static struct futex * soft_create_futex(process p, u64 key)
{
    heap h = heap_locked(get_kernel_heaps());
    struct futex * f;
    process_vm vm = p->vm;

    spin_lock(&vm->futex_lock);

    f = table_find(vm->futices, pointer_from_u64(key));
    if (f)
        goto out;

    f = allocate(h, sizeof(struct futex));
    if (f == INVALID_ADDRESS) {
        msg_err("futex: failed to allocate structure");
        goto out;
    }

    f->h = h;
    f->bq = allocate_blockq(f->h, ss("futex"));
    if (f->bq == INVALID_ADDRESS) {
        msg_err("futex: failed to allocate blockq");
        deallocate(f->h, f, sizeof(struct futex));
        f = INVALID_ADDRESS;
        goto out;
    }

    spin_lock_init(&f->lock);
    table_set(vm->futices, pointer_from_u64(key), f);
  out:
    spin_unlock(&vm->futex_lock);
    return f;
}

static unix_context futex_wake_one(struct futex * f)
{
    unix_context t = blockq_wake_one(f->bq);
    if (t != INVALID_ADDRESS)
        return t;
    kern_pause();
    return t;
}

/*
 * Wake up to 'val' waiters
 * Return the number woken
 */
static int futex_wake_many(struct futex * f, int val)
{
    int nr_woken;

    for (nr_woken = 0; nr_woken < val; nr_woken++) {
        unix_context w = futex_wake_one(f);
        if (w == INVALID_ADDRESS)
            break;
    }

    return nr_woken;
}

boolean futex_wake_many_by_uaddr(process p, int *uaddr, int val)
{
    struct futex * f;

    process_vm vm = p->vm;
    spin_lock(&vm->futex_lock);
    f = table_find(vm->futices, (void *)uaddr);
    spin_unlock(&vm->futex_lock);
    if (!f)
        return false;

    futex_lock(f);
    futex_wake_many(f, val);
    futex_unlock(f);
    return true;
}

/*
 * futex_bh is invoked either by the bh processor in response
 * to timeout/signal delivery/etc., or by another thread in sys_futex
 *
 * Return:
 *  BLOCKQ_BLOCK_REQUIRED: top half, going to block
 *  -ETIMEDOUT: if we timed out
 *  -EINTR: if we're being nullified
 *  0: thread woken up
 */
closure_function(3, 1, sysreturn, futex_bh,
                 struct futex *, f, thread, t, timestamp, timeout,
                 u64 flags)
{
    thread t = bound(t);
    struct futex *f = bound(f);
    sysreturn rv;

    if (!(flags & BLOCKQ_ACTION_BLOCKED)) {
        futex_unlock(f);
        return BLOCKQ_BLOCK_REQUIRED;
    }

    if (flags & (BLOCKQ_ACTION_NULLIFY | BLOCKQ_ACTION_TIMEDOUT)) {
        if (flags & BLOCKQ_ACTION_NULLIFY)
            rv = bound(timeout) ? -EINTR : -ERESTARTSYS;
        else
            rv = -ETIMEDOUT;
    } else {
        rv = 0; /* no timer expire + not us --> actual wakeup */
    }

    closure_finish();
    return syscall_return(t, rv);
}

closure_function(1, 1, void, futex_requeue_handler,
                 struct futex *, dest,
                 blockq_action action)
{
    closure_member(futex_bh, action, f) = bound(dest);
}

static sysreturn get_timeout_timestamp(int futex_op, u64 val2, timestamp *ts)
{
    switch (futex_op) {
    case FUTEX_WAIT:
    case FUTEX_WAIT_BITSET:
        if (val2)
            return user_timespec_get((struct timespec *)pointer_from_u64(val2), ts);
        break;
    }
    *ts = 0;
    return 0;
}

sysreturn futex(int *uaddr, int futex_op, int val,
                u64 val2, int *uaddr2, int val3)
{
    struct futex * f;
    timestamp ts;
    int op;

    if (!memory_is_user(uaddr, sizeof(int)))
        return set_syscall_error(current, EFAULT);

    f = soft_create_futex(current->p, u64_from_pointer(uaddr));
    if (f == INVALID_ADDRESS)
        return set_syscall_error(current, ENOMEM);
    
    op = futex_op & 127; // chuck the private bit
    sysreturn ret = get_timeout_timestamp(op, val2, &ts);
    if (ret)
        return ret;
    clock_id clkid = (futex_op & FUTEX_CLOCK_REALTIME) ? CLOCK_ID_REALTIME :
            CLOCK_ID_MONOTONIC;

    context ctx = get_current_context(current_cpu());
    switch (op) {
    case FUTEX_WAIT: {
        sysreturn rv;
        futex_lock(f);
        if (context_set_err(ctx))
            rv = -EFAULT;
        else if (*uaddr != val)
            rv = -EAGAIN;
        else
            rv = blockq_check_timeout(f->bq,
                                      contextual_closure(futex_bh, f, current, ts),
                                      false, clkid, ts, false);
        futex_unlock(f);
        if (rv != -EFAULT)
            context_clear_err(ctx);
        return rv;
    }

    case FUTEX_WAKE: {
        futex_lock(f);
        int nr_woken = futex_wake_many(f, val);
        futex_unlock(f);
        return nr_woken;
    }

    case FUTEX_CMP_REQUEUE: {
        int woken, requeued;

        if (!memory_is_user(uaddr2, sizeof(int)))
            return set_syscall_error(current, EFAULT);

        sysreturn rv;
        futex_lock(f);
        if (context_set_err(ctx)) {
            rv = -EFAULT;
            goto cmp_requeue_done;
        }
        if (*uaddr != val3) {
            rv = -EAGAIN;
            goto cmp_requeue_done;
        }

        woken = futex_wake_many(f, val);

        requeued = 0;
        if (val2 > 0) {
            struct futex * new = soft_create_futex(current->p, u64_from_pointer(uaddr2));
            if (new == INVALID_ADDRESS) {
                rv = -ENOMEM;
                goto cmp_requeue_done;
            }
            requeued = blockq_transfer_waiters(new->bq, f->bq, val2,
                                               stack_closure(futex_requeue_handler, new));
        }
        rv = woken + requeued;
      cmp_requeue_done:
        futex_unlock(f);
        if (rv != -EFAULT)
            context_clear_err(ctx);
        return rv;
    }

    case FUTEX_WAKE_OP: {
        unsigned int cmparg = val3 & MASK(12);
        unsigned int oparg = (val3 >> 12) & MASK(12);
        unsigned int cmp = (val3 >> 24) & MASK(4);
        unsigned int op = (val3 >> 28) & MASK(4);
        int oldval, wake1, wake2, c;

        if (!memory_is_user(uaddr2, sizeof(int)))
            return set_syscall_error(current, EFAULT);

        struct futex *f2 = soft_create_futex(current->p, u64_from_pointer(uaddr2));
        if (f2 == INVALID_ADDRESS)
            return -ENOMEM;
        spin_lock_2(&f->lock, &f2->lock);
        if (context_set_err(ctx)) {
            ret = -EFAULT;
            goto wake_op_done;
        }
        
        switch (op) {
        case FUTEX_OP_SET:   oldval = __atomic_exchange_n(uaddr2, oparg, __ATOMIC_RELAXED); break;
        case FUTEX_OP_ADD:   oldval = __atomic_fetch_add(uaddr2, oparg, __ATOMIC_RELAXED); break;
        case FUTEX_OP_OR:    oldval = __atomic_fetch_or(uaddr2, oparg, __ATOMIC_RELAXED); break;
        case FUTEX_OP_ANDN:  oldval = __atomic_fetch_and(uaddr2, ~oparg, __ATOMIC_RELAXED); break;
        case FUTEX_OP_XOR:   oldval = __atomic_fetch_xor(uaddr2, oparg, __ATOMIC_RELAXED); break;
        default:             ret = -ENOSYS; break;
        }
        context_clear_err(ctx);
        if (ret)
            goto wake_op_done;

        wake1 = futex_wake_many(f, val);
        
        c = 0;
        switch (cmp) {
        case FUTEX_OP_CMP_EQ: c = (oldval == cmparg) ; break;
        case FUTEX_OP_CMP_NE: c = (oldval != cmparg); break;
        case FUTEX_OP_CMP_LT: c = (oldval < cmparg); break;
        case FUTEX_OP_CMP_LE: c = (oldval <= cmparg); break;
        case FUTEX_OP_CMP_GT: c = (oldval > cmparg) ; break;
        case FUTEX_OP_CMP_GE: c = (oldval >= cmparg) ; break;
        }
        
        wake2 = 0;
        if (c) {
            wake2 = futex_wake_many(f2, val2);
        }

      wake_op_done:
        futex_unlock(f2);
        futex_unlock(f);
        return ret ? ret : wake1 + wake2;
    }

    case FUTEX_WAIT_BITSET: {
        sysreturn rv;
        futex_lock(f);
        if (context_set_err(ctx))
            rv = -EFAULT;
        else if (*uaddr != val)
            rv = -EAGAIN;
        else
            rv = blockq_check_timeout(f->bq,
                                      contextual_closure(futex_bh, f, current, ts),
                                      false, clkid, ts, true);
        futex_unlock(f);
        if (rv != -EFAULT)
            context_clear_err(ctx);
        return rv;
    }

    default:
        msg_err("futex op %d not implemented", op);
        break;
    }

    return set_syscall_error(current, ENOSYS);
}

boolean init_futices(process_vm vm)
{
    heap h = heap_locked(get_kernel_heaps());
    spin_lock_init(&vm->futex_lock);
    vm->futices = allocate_table(h, futex_key_function, futex_key_equal);
    return vm->futices != INVALID_ADDRESS;
}

void deallocate_futices(process_vm vm)
{
    table_foreach(vm->futices, k, v) {
        (void)k;
        struct futex *f = v;
        deallocate_blockq(f->bq);
        deallocate(f->h, f, sizeof(struct futex));
    }
    deallocate_table(vm->futices);
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
        t = thread_from_tid(current->p, pid);
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
