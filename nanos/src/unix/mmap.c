#include <unix_internal.h>
#include <storage.h>

//#define VMAP_PARANOIA

//#define PF_DEBUG
#ifdef PF_DEBUG
#define pf_debug(x, ...) do {tprintf(sym(fault), 0, ss(x), ##__VA_ARGS__);} while(0)
#else
#define pf_debug(x, ...)
#endif

//#define VMAP_DEBUG
#ifdef VMAP_DEBUG
#define vmap_debug(x, ...) do {tprintf(sym(vmap), 0, ss(x), ##__VA_ARGS__);} while(0)
#else
#define vmap_debug(x, ...)
#endif

#define vmap_assert(x)                                                  \
    do {                                                                \
        if (!(x)) {                                                     \
            print_frame_trace_from_here();                              \
            if (current)                                                \
                vmap_dump(current->p->vm->vmaps);                           \
            halt("assertion " #x " failed at " __FILE__ ":%d  in %s(); halt\n", \
                 __LINE__, func_ss);                                    \
        }                                                               \
    } while(0)

typedef struct vmap_heap {
    struct heap h;  /* must be first */
    process_vm vm;
    boolean randomize;
} *vmap_heap;

static struct {
    heap h;
    heap virtual_backed;
    u64 thp_max_size;

    closure_struct(rb_key_compare, pf_compare);
    closure_struct(rbnode_handler, pf_print);

    struct list pf_freelist;
    struct spinlock pf_lock;
} mmap_info;

static status demand_page_internal(process p, context ctx, u64 vaddr, vmap vm, pending_fault *pf);

struct shared_anon {
    struct refcount refcount;
    closure_struct(thunk, free);
    struct spinlock lock;
    table pages;
};

static u64 shared_anon_key(void *index)
{
    return u64_from_pointer(index);
}

closure_func_basic(thunk, void, shared_anon_free)
{
    shared_anon sa = struct_from_closure(shared_anon, free);
    table_foreach(sa->pages, index, kvirt) {
        (void)index;
        deallocate(mmap_info.virtual_backed, kvirt, PAGESIZE);
    }
    deallocate_table(sa->pages);
    deallocate(mmap_info.h, sa, sizeof(*sa));
}

static shared_anon allocate_shared_anon(void)
{
    shared_anon sa = allocate(mmap_info.h, sizeof(*sa));
    if (sa == INVALID_ADDRESS)
        return sa;
    sa->pages = allocate_table(mmap_info.h, shared_anon_key, pointer_equal);
    if (sa->pages == INVALID_ADDRESS) {
        deallocate(mmap_info.h, sa, sizeof(*sa));
        return INVALID_ADDRESS;
    }
    spin_lock_init(&sa->lock);
    init_refcount(&sa->refcount, 1, init_closure_func(&sa->free, thunk, shared_anon_free));
    return sa;
}

static status demand_shared_anon_page(process p, u64 vaddr, vmap vm)
{
    shared_anon sa = vm->shared_anon;
    u64 page_addr = vaddr & ~PAGEMASK;
    void *index = pointer_from_u64((vm->node_offset + (page_addr - vm->node.r.start)) >> PAGELOG);
    spin_lock(&sa->lock);
    void *kvirt = table_find(sa->pages, index);
    spin_unlock(&sa->lock);
    if (!kvirt) {
        void *page = allocate(mmap_info.virtual_backed, PAGESIZE);
        if (page == INVALID_ADDRESS)
            return timm_oom;
        zero(page, PAGESIZE);
        spin_lock(&sa->lock);
        kvirt = table_find(sa->pages, index);
        if (!kvirt) {
            table_set(sa->pages, index, page);
            kvirt = page;
            page = 0;
        }
        spin_unlock(&sa->lock);
        if (page)
            deallocate(mmap_info.virtual_backed, page, PAGESIZE);
    }
    smp_write_barrier();
    address_space_map(p->vm->as, page_addr, physical_from_virtual(kvirt), PAGESIZE,
                      pageflags_from_vmflags(vm->flags));
    return STATUS_OK;
}

closure_func_basic(thunk, void, pending_fault_complete)
{
    pending_fault pf = struct_from_closure(pending_fault, complete);
    process p = pf->p;
    context ctx = pf->ctx;
    u64 vaddr = pf->addr;
    vmap_lock(p);
    vmap vm = vmap_from_vaddr(p, vaddr);
    status s;
    if (vm != INVALID_ADDRESS)
        s = demand_page_internal(p, ctx, vaddr, vm, &pf);
    else
        s = STATUS_OK;
    pagecache_node pn;
    range ra;   /* read-ahead */
    if (pf->type == PENDING_FAULT_FILEBACKED) {
        pn = pf->filebacked.pn;
        if (is_ok(s))
            /* File read-ahead must be done without holding the vmap lock, because it can suspend
             * the current context. */
            ra = irange(pf->filebacked.node_offset + PAGESIZE,
                        vm->node_offset + range_span(vm->node.r));
        else
            ra = irange(1, 0);  /* dummy invalid range */
    } else {
        pn = 0;
    }
    spin_lock(&mmap_info.pf_lock);
    list_insert_after(&mmap_info.pf_freelist, &pf->l_free);
    spin_unlock(&mmap_info.pf_lock);
    vmap_unlock(p);
    pf_debug("%s: vaddr 0x%lx, status %v\n", func_ss, vaddr, s);
    if (!is_ok(s)) {
        msg_err("page fill failed with %v", s);
    }
    demand_page_done(ctx, vaddr, s);
    context_schedule_return(ctx);
    if (pn) {
        if (range_valid(ra)) {
            if (range_span(ra) > FILE_READAHEAD_DEFAULT)
                ra.end = ra.start + FILE_READAHEAD_DEFAULT;
            pagecache_node_fetch_pages(pn, ra, 0, 0);
        }
        pagecache_node_unref(pn);
    }
}

pending_fault new_pending_fault_locked(process p, context ctx, u64 addr)
{
    pending_fault pf;
    list l;
    spin_lock(&mmap_info.pf_lock);
    if ((l = list_get_next(&mmap_info.pf_freelist)))
        list_delete(l);
    spin_unlock(&mmap_info.pf_lock);
    if (l) {
        pf = struct_from_list(l, pending_fault, l_free);
    } else {
        pf = allocate(mmap_info.h, sizeof(struct pending_fault));
        if (pf == INVALID_ADDRESS)
            return pf;
    }
    pf->addr = addr;
    pf->p = p;
    pf->ctx = ctx;
    init_closure_func(&pf->complete, thunk, pending_fault_complete);
    return pf;
}

#define VMAP_PAGE_SHRINK(v, page_addr, page_size)   do {    \
    page_size >>= 1;                                        \
    if (page_addr + page_size <= v) {                       \
        page_addr += page_size;                             \
    }                                                       \
} while (0)

closure_function(3, 3, boolean, vmap_anon_ptes,
                 u64, v, u64 *, page_addr, u64 *, page_size,
                 int level, u64 curr, pteptr entry_p)
{
    pte entry = pte_from_pteptr(entry_p);
    if (!pte_is_present(entry) || !pte_is_mapping(level, entry))
        return true;
    range cur_mapping = irangel(curr, U64_FROM_BIT(pte_order(level, entry)));
    u64 page_addr = *bound(page_addr);
    u64 page_size = *bound(page_size);
    pf_debug("  page_addr 0x%lx, page_size 0x%lx, current mapping %R\n", page_addr, page_size,
             cur_mapping);
    range ri = range_intersection(cur_mapping, irangel(page_addr, page_size));
    u64 v = bound(v);
    if (range_span(ri) == 0) {
        if (cur_mapping.end <= v)
            return true;
        else    /* we are past (page_addr + page_size), PTE traversal can stop here */
            return false;
    }
    if (cur_mapping.end <= v) {
        while (page_addr < cur_mapping.end)
            VMAP_PAGE_SHRINK(v, page_addr, page_size);
        pf_debug("  shrinking from head, page_addr 0x%lx, page_size 0x%lx\n", page_addr, page_size);
        *bound(page_addr) = page_addr;
        *bound(page_size) = page_size;
        return true;
    }
    if (cur_mapping.start > v) {
        while (page_addr + page_size > cur_mapping.start)
            VMAP_PAGE_SHRINK(v, page_addr, page_size);
        pf_debug("  shrinking from tail, page_addr 0x%lx, page_size 0x%lx\n", page_addr, page_size);
        *bound(page_addr) = page_addr;
        *bound(page_size) = page_size;
        return false;
    }

    /* the current mapping includes v */
    *bound(page_size) = 0;
    return false;
}

/* returns true if successful */
static boolean new_zeroed_pages(process p, u64 v, vmap vm, pageflags flags, void *kvirt)
{
    u64 page_addr = v & ~MASK(PAGELOG);
    u64 page_size = PAGESIZE;
    if ((kvirt == INVALID_ADDRESS) && (vm->flags & VMAP_FLAG_THP)) {
        u64 max_size = mmap_info.thp_max_size;
        while ((page_size < max_size) && (page_addr >= vm->node.r.start) &&
               (page_addr + page_size <= vm->node.r.end)) {
            page_size <<= 1;
            page_addr = v & ~(page_size - 1);
            if ((page_addr < vm->node.r.start) || (page_addr + page_size > vm->node.r.end)) {
                page_size >>= 1;
                page_addr = v & ~(page_size - 1);
                break;
            }
        }
    }
    pf_debug("%s: v 0x%lx, vmap %R, page_addr 0x%lx, page_size 0x%lx\n", func_ss, v, vm->node.r,
             page_addr, page_size);
    address_space_traverse_ptes(p->vm->as, page_addr, page_size,
                                stack_closure(vmap_anon_ptes, v, &page_addr, &page_size));
    pf_debug("  after traversing PTEs: page_addr 0x%lx, page_size 0x%lx\n", page_addr, page_size);
    if (page_size == 0) {
        /* The mapping must have been done in parallel by another CPU. */
        if (kvirt != INVALID_ADDRESS)
            deallocate(mmap_info.virtual_backed, kvirt, PAGESIZE);
        return true;
    }
    void *m;
    if (kvirt == INVALID_ADDRESS) {
        while ((m = allocate(mmap_info.virtual_backed, page_size)) == INVALID_ADDRESS) {
            if (page_size == PAGESIZE) {
                vmap_debug("%s: cannot get physical page\n", func_ss);
                return false;
            }
            VMAP_PAGE_SHRINK(v, page_addr, page_size);
        }
    } else {
        m = kvirt;
    }
    zero(m, page_size);
    smp_write_barrier();
    address_space_map(p->vm->as, page_addr, physical_from_virtual(m), page_size, flags);
    return true;
}

static void demand_page_major_fault(context ctx)
{
    count_major_fault();
    context_pre_suspend(ctx);
}

closure_func_basic(thunk, void, pending_fault_anonymous)
{
    pending_fault pf = struct_from_closure(pending_fault, async_handler);
    void *kvirt = mem_alloc(mmap_info.virtual_backed, PAGESIZE, 0);
    pf->page_kvirt = (kvirt != INVALID_ADDRESS) ? irangel(u64_from_pointer(kvirt), PAGESIZE) :
                                                  irange(0, 0);
    thunk complete = (thunk)&pf->complete;
    apply(complete);
}

static status demand_anonymous_page(process p, context ctx, u64 vaddr, vmap vm, pending_fault *pf)
{
    pageflags flags = pageflags_from_vmflags(vm->flags);
    void *kvirt = (*pf && (range_span((*pf)->page_kvirt) != 0)) ?
                  pointer_from_u64((*pf)->page_kvirt.start) : INVALID_ADDRESS;
    if (new_zeroed_pages(p, vaddr, vm, flags, kvirt))
        return STATUS_OK;
    else if (*pf)
        return timm_oom;
    pending_fault new_pf = new_pending_fault_locked(p, ctx, vaddr);
    if (new_pf != INVALID_ADDRESS) {
        new_pf->type = PENDING_FAULT_ANONYMOUS;
        init_closure_func(&new_pf->async_handler, thunk, pending_fault_anonymous);
    }
    *pf = new_pf;
    return STATUS_OK;
}

/* kvirt may cover more than one page: nothing is mapped if any of them is mapped already */
static status mmap_filebacked_page(process p, vmap vm, u64 page_addr, pageflags flags, range kvirt)
{
    u64 len = range_span(kvirt);
    page_addr &= ~(len - 1);
    u64 vmap_offset = page_addr - vm->node.r.start;
    boolean pagecache_map = true;
    void *kv = pointer_from_u64(kvirt.start);
    pagetable_lock();
    for (u64 off = 0; off < len; off += PAGESIZE)
        if (address_space_physical_locked(p->vm->as, page_addr + off) != INVALID_PHYSICAL) {
            /* The mapping must have been done in parallel by another CPU. */
            pagecache_map = false;
            break;
        }
    if (pagecache_map) {
        if (vm->flags & VMAP_FLAG_TAIL_BSS) {
            u64 bss_offset = vm->bss_offset;
            if (point_in_range(irangel(vmap_offset, len), bss_offset)) {
                u64 bss_start = bss_offset - vmap_offset;
                zero(kv + bss_start, len - bss_start);
            }
        }
        address_space_map_locked(p->vm->as, page_addr, physical_from_virtual(kv), len, flags);
    }
    pagetable_unlock();
    if (!pagecache_map)
        for (u64 off = 0; off < len; off += PAGESIZE)
            pagecache_release_page(vm->cache_node, vm->node_offset + vmap_offset + off);
    return STATUS_OK;
}

/* The largest aligned window of a shared mapping that lies within both the mapping and the file */
static u64 filebacked_page_size(vmap vm, u64 page_addr, u64 node_offset, u64 padlen,
                                boolean shared)
{
    if (!shared || (vm->flags & VMAP_FLAG_TAIL_BSS))
        return PAGESIZE;
    u64 size = PAGESIZE;
    u64 max_size = mmap_info.thp_max_size;
    while (size < max_size) {
        u64 next = size << 1;
        u64 addr = page_addr & ~(next - 1);
        u64 offset = node_offset & ~(next - 1);
        if (((page_addr - addr) != (node_offset - offset)) ||
            (addr < vm->node.r.start) || (addr + next > vm->node.r.end) ||
            (offset + next > padlen))
            break;
        size = next;
    }
    return size;
}

closure_func_basic(pagecache_page_handler, void, pending_fault_page_handler,
                   range kvirt)
{
    pending_fault pf = struct_from_closure(pending_fault, filebacked.demand_file_page);
    pf->page_kvirt = kvirt;
    thunk complete = (thunk)&pf->complete;
    apply(complete);
}

closure_func_basic(thunk, void, pending_fault_filebacked)
{
    pending_fault pf = struct_from_closure(pending_fault, async_handler);
    pagecache_page_handler h = init_closure_func(&pf->filebacked.demand_file_page,
                                                 pagecache_page_handler,
                                                 pending_fault_page_handler);
    pagecache_get_page(pf->filebacked.pn, pf->filebacked.node_offset, pf->filebacked.size,
                       pf->filebacked.private_page, h);
}

static status demand_filebacked_page(process p, context ctx, u64 vaddr, vmap vm, pending_fault *pf)
{
    pageflags flags = pageflags_from_vmflags(vm->flags);
    u64 page_addr = vaddr & ~PAGEMASK;
    u64 vmap_offset = page_addr - vm->node.r.start;
    pagecache_node pn = vm->cache_node;
    u64 node_offset = vm->node_offset + vmap_offset;
    boolean shared = (vm->flags & VMAP_FLAG_SHARED) != 0;
    boolean private_page = (vm->flags & VMAP_FLAG_PROG) && (vm->flags & VMAP_FLAG_WRITABLE);
    if (!shared && !private_page)
        flags = pageflags_readonly(flags); /* cow */

    pf_debug("   node %p (start 0x%lx), offset 0x%lx, vm flags 0x%lx, pageflags 0x%lx\n",
             pn, vm->node.r.start, node_offset, vm->flags, flags.w);

    u64 padlen = pad(pagecache_get_node_length(pn), PAGESIZE);
    pf_debug("   map length 0x%lx\n", padlen);
    if (node_offset >= padlen) {
        pf_debug("   extends past map limit 0x%lx\n", padlen);
        return timm("result", "out of range page");
    }

    range kvirt;
    status s;
    if (!*pf) {
        kvirt = pagecache_get_page_if_filled(pn, node_offset, private_page);
        if (range_span(kvirt) != 0)
            return mmap_filebacked_page(p, vm, page_addr, flags, kvirt);
        pending_fault new_pf = new_pending_fault_locked(p, ctx, vaddr);
        if (new_pf != INVALID_ADDRESS) {
            pagecache_node_ref(pn);
            new_pf->type = PENDING_FAULT_FILEBACKED;
            new_pf->filebacked.pn = pn;
            new_pf->filebacked.node_offset = node_offset;
            init_closure_func(&new_pf->async_handler, thunk, pending_fault_filebacked);
            new_pf->filebacked.private_page = private_page;
            new_pf->filebacked.size = filebacked_page_size(vm, page_addr, node_offset, padlen,
                                                           shared && !private_page);
        }
        *pf = new_pf;
        return STATUS_OK;
    }
    if (((*pf)->filebacked.pn != pn) || ((*pf)->filebacked.node_offset != node_offset))
        return STATUS_OK;
    kvirt = (*pf)->page_kvirt;
    if (range_span(kvirt) == 0)
        s = timm_oom;
    else
        s = mmap_filebacked_page(p, vm, page_addr, flags, kvirt);
    return s;
}

static status demand_page_internal(process p, context ctx, u64 vaddr, vmap vm, pending_fault *pf)
{
    if ((vm->flags & (VMAP_FLAG_MMAP | VMAP_FLAG_STACK | VMAP_FLAG_HEAP |
                      VMAP_FLAG_BSS | VMAP_FLAG_PROG)) == 0) {
        msg_err("page fault: vaddr 0x%lx matched vmap with invalid flags (0x%x)",
                vaddr, vm->flags);
        return timm("result", "vaddr 0x%lx matched vmap with invalid flags (0x%x)\n",
                vaddr, vm->flags);
    }

    pf_debug("%s: %s context, %s, vaddr %p, vm flags 0x%02lx,\n", func_ss,
             context_type_strings[ctx->type],
             string_from_mmap_type(vm->flags & VMAP_MMAP_TYPE_MASK),
             vaddr, vm->flags);
    pf_debug("   vmap %p, context %p\n", vm, ctx);

    boolean anonymous;
    if (vm->flags & VMAP_FLAG_MMAP) {
        int mmap_type = vm->flags & VMAP_MMAP_TYPE_MASK;
        switch (mmap_type) {
        case VMAP_MMAP_TYPE_ANONYMOUS:
            anonymous = true;
            break;
        case VMAP_MMAP_TYPE_FILEBACKED:
            anonymous = false;
            break;
        default:
            if (!vm->fault)
                return timm("result", "no fault handler for vaddr 0x%lx, flags 0x%x",
                            vaddr, vm->flags);
            return vm->fault(p, ctx, vaddr, vm, pf);
        }
    } else if (vm->flags & VMAP_FLAG_PROG) {
        pf_debug("   file-backed program page fault\n");
        anonymous = false;
    } else {
        pf_debug("   bss / stack / heap page fault\n");
        anonymous = true;
    }
    boolean fault_pending = !!(*pf);
    status s;
    if (anonymous && vm->shared_anon) {
        s = demand_shared_anon_page(p, vaddr, vm);
    } else if (anonymous) {
        if (!fault_pending || ((*pf)->type == PENDING_FAULT_ANONYMOUS))
            s = demand_anonymous_page(p, ctx, vaddr, vm, pf);
        else
            s = STATUS_OK;
    } else {
        if (!fault_pending || ((*pf)->type == PENDING_FAULT_FILEBACKED))
            s = demand_filebacked_page(p, ctx, vaddr, vm, pf);
        else
            s = STATUS_OK;
    }
    return s;
}

status do_demand_page(process p, context ctx, u64 vaddr, vmap vm, boolean *done)
{
    pending_fault pf = 0;
    status s = demand_page_internal(p, ctx, vaddr, vm, &pf);
    if (pf) {
        if (pf != INVALID_ADDRESS) {
            demand_page_major_fault(ctx);
            async_apply_bh((thunk)&pf->async_handler);
            *done = false;
        } else {
            s = timm_oom;
            *done = true;
        }
    } else {
        count_minor_fault();
        *done = true;
    }
    return s;
}

sysreturn futex_key_lookup(process p, u64 uaddr, boolean private, futex_key k)
{
    k->kind = FUTEX_KEY_PRIVATE;
    k->object = p->vm;
    k->offset = uaddr;
    if (private)
        return 0;
    sysreturn rv = 0;
    vmap_lock(p);
    vmap vm = vmap_from_vaddr(p, uaddr);
    if (vm == INVALID_ADDRESS) {
        rv = -EFAULT;
    } else if (vm->flags & VMAP_FLAG_SHARED) {
        u64 offset = vm->node_offset + (uaddr - vm->node.r.start);
        if (vm->shared_anon) {
            k->kind = FUTEX_KEY_SHARED_ANON;
            k->object = vm->shared_anon;
            k->offset = offset;
            refcount_reserve(&vm->shared_anon->refcount);
        } else if (vm->cache_node) {
            k->kind = FUTEX_KEY_PAGECACHE;
            k->object = vm->cache_node;
            k->offset = offset;
            pagecache_node_ref(vm->cache_node);
        }
    }
    vmap_unlock(p);
    return rv;
}

void futex_key_release(futex_key k)
{
    switch (k->kind) {
    case FUTEX_KEY_SHARED_ANON:
        refcount_release(&((shared_anon)k->object)->refcount);
        break;
    case FUTEX_KEY_PAGECACHE:
        pagecache_node_unref(k->object);
        break;
    case FUTEX_KEY_PRIVATE:
        break;
    }
}

vmap vmap_from_vaddr(process p, u64 vaddr)
{
    rangemap rm = p->vm->vmaps;
    vmap vm = (vmap)rangemap_lookup_at_or_next(rm, vaddr);
    if ((vm != INVALID_ADDRESS) && (vm->node.r.start > vaddr)) {
        /* vm does not cover this address; if it is a stack mapping, check whether it can be
         * expanded downwards. */
        if (!(vm->flags & VMAP_FLAG_STACK) || (vm->node.r.end > vaddr + p->rlimit_stack))
            return INVALID_ADDRESS;
        vmap prev = (vmap)rangemap_prev_node(rm, &vm->node);
        vaddr &= ~PAGEMASK;
        if ((prev == INVALID_ADDRESS) || (prev->node.r.end <= vaddr - PROCESS_STACK_GUARD_GAP))
            /* expand stack mapping */
            vm->node.r.start = vaddr;
        else
            /* cannot expand stack mapping because of insufficient gap with previous mapping */
            vm = INVALID_ADDRESS;
    }
    return vm;
}

void vmap_iterator(process p, vmap_handler vmh)
{
    vmap_lock(p);
    vmap vm = (vmap) rangemap_first_node(p->vm->vmaps);
    while (vm != INVALID_ADDRESS) {
        if (!apply(vmh, vm))
            break;
        vm = (vmap) rangemap_next_node(p->vm->vmaps, &vm->node);
    }
    vmap_unlock(p);
}

closure_function(1, 1, boolean, vmap_validate_node,
                 u32, flags,
                 vmap vm)
{
    u32 flags = bound(flags);
    return (vm->flags & flags) == flags;
}

closure_func_basic(range_handler, boolean, vmap_validate_gap,
                   range q)
{
    return false;
}

/* Walk a given range in the process memory map and invoke a vmap handler for each mapping
 * encountered in that range.
 * If `allow_gaps` is false, the presence of any gap in the memory map over the given range makes
 * this function return RM_ABORT; otherwise, this function returns RM_MATCH if at least (part of)
 * one mapping exists in the given range, RM_NOMATCH of no mapping exists. */
static int vmap_range_walk(process_vm vm, range q, vmap_handler node_handler, boolean allow_gaps)
{
    range_handler gap_handler = allow_gaps ?
                                0 : stack_closure_func(range_handler, vmap_validate_gap);
    process_vm_lock(vm);
    int res = rangemap_range_lookup_with_gaps(vm->vmaps, q, (rmnode_handler)node_handler,
                                              gap_handler);
    process_vm_unlock(vm);
    return res;
}

boolean vmap_validate_range(process p, range q, u32 flags)
{
    return (vmap_range_walk(p->vm, q, stack_closure(vmap_validate_node, flags), false) == RM_MATCH);
}

closure_func_basic(rmnode_handler, boolean, vmap_dump_node,
                   rmnode n)
{
    vmap curr = (vmap)n;
    rprintf("  %R, %s%s %s%s\n", curr->node.r,
            (curr->flags & VMAP_FLAG_MMAP) ? ss("mmap ") : sstring_empty(),
            string_from_mmap_type(curr->flags & VMAP_MMAP_TYPE_MASK),
            (curr->flags & VMAP_FLAG_WRITABLE) ? ss("writable ") : sstring_empty(),
            (curr->flags & VMAP_FLAG_EXEC) ? ss("exec ") : sstring_empty());
    return true;
}

void vmap_dump(rangemap pvmap)
{
    rprintf("vmaps:\n");
    rmnode_handler nh = stack_closure_func(rmnode_handler, vmap_dump_node);
    rangemap_range_lookup(pvmap, (range){0, infinity}, nh);
}

static u64 node_vstart(vmap v)
{
    return v->node_offset - v->node.r.start;
}

static boolean vmap_compare_attributes(vmap a, vmap b)
{
    return (a->flags == b->flags &&
            a->allowed_flags == b->allowed_flags &&
            a->cache_node == b->cache_node &&
            a->shared_anon == b->shared_anon &&
            a->fd == b->fd /* and bss_offset */ &&
            ((!a->cache_node && !a->shared_anon) || (node_vstart(a) == node_vstart(b))));
}

#ifdef VMAP_PARANOIA
closure_function(1, 1, boolean, vmap_paranoia_node,
                 vmap *, last,
                 rmnode n)
{
    vmap v = (vmap)n;
    vmap last = *bound(last);
    if (last && vmap_compare_attributes(last, v) && last->node.r.end == v->node.r.start) {
        msg_err("%s: check failed; adjacent nodes %p (%R) and %p (%R) share same attributes",
                func_ss, last, last->node.r, v, v->node.r);
        return false;
    }
    *bound(last) = v;
    return true;
}

static void vmap_paranoia_locked(rangemap pvmap)
{
    vmap last = 0;
    rmnode_handler nh = stack_closure(vmap_paranoia_node, &last);
    if (!rangemap_range_lookup(pvmap, (range){0, infinity}, nh)) {
        vmap_dump(pvmap);
        print_frame_trace_from_here();
        halt("%s failed\n", func_ss);
    }
}
#else
#define vmap_paranoia_locked(x)
#endif

/* TODO maybe refcount makes more sense now that we have asynchronous faults */
static boolean vmap_may_write_cache_node(vmap vm)
{
    return (vm->flags & VMAP_FLAG_SHARED) && (vm->allowed_flags & VMAP_FLAG_WRITABLE) &&
        vm->cache_node;
}

static void deallocate_vmap_locked(rangemap rm, vmap vm)
{
    vmap_debug("%s: vm %p %R\n", func_ss, vm, vm->node.r);
    if (vmap_may_write_cache_node(vm))
        pagecache_node_unmap_writable(vm->cache_node);
    if (!(vm->flags & VMAP_FLAG_TAIL_BSS) && vm->fd)
        fdesc_put(vm->fd);
    if (vm->shared_anon)
        refcount_release(&vm->shared_anon->refcount);
    deallocate(rm->h, vm, sizeof(struct vmap));
}

static vmap allocate_vmap_locked(rangemap rm, struct vmap *k)
{
    range q = k->node.r;
    vmap vm;

    vmap_debug("%s: q %R\n", func_ss, q);
    vm = (vmap)rangemap_lookup_max_lte(rm, q.start);
    vmap next;
    if (vm != INVALID_ADDRESS) {
        if (vm->node.r.end > q.start)
            return INVALID_ADDRESS;
        next = (vmap)rangemap_next_node(rm, &vm->node);
        if (vm->node.r.end == q.start && vmap_compare_attributes(vm, k)) {
            range new = irange(vm->node.r.start, q.end);
            if (next != INVALID_ADDRESS) {
                if (next->node.r.start < q.end)
                    return INVALID_ADDRESS;
                if (next->node.r.start == q.end && vmap_compare_attributes(next, k)) {
                    new.end = next->node.r.end;
                    vmap_debug("   removing %R\n", next->node.r);
                    rangemap_remove_node(rm, &next->node);
                    deallocate_vmap_locked(rm, next);
                }
            }
            vmap_debug("   extend vm %p, was %R, new %R\n", vm, vm->node.r, new);
            vmap_assert(rangemap_reinsert(rm, &vm->node, new));
        } else {
            vm = INVALID_ADDRESS;
        }
    } else {
        next = (vmap)rangemap_first_node(rm);
    }

    if (vm == INVALID_ADDRESS && next != INVALID_ADDRESS &&
        q.end == next->node.r.start && vmap_compare_attributes(next, k)) {
        vm = next;
        range new = irange(q.start, vm->node.r.end);
        vmap_debug("   advance vm %p, was %R, new %R\n", vm, vm->node.r, new);
        if (vm->cache_node || vm->shared_anon) {
            vmap_assert(vm->node.r.start > q.start);
            vm->node_offset -= vm->node.r.start - q.start;
        }
        vmap_assert(rangemap_reinsert(rm, &vm->node, new));
    }

    if (vm != INVALID_ADDRESS) {
        vmap_paranoia_locked(rm);
        return vm;
    }
    vm = allocate(rm->h, sizeof(struct vmap));
    if (vm == INVALID_ADDRESS)
        return vm;
    vmap_debug("   allocated new vmap %p\n", vm);
    rmnode_init(&vm->node, q);
    vm->flags = k->flags;
    vm->allowed_flags = k->allowed_flags;
    vm->node_offset = k->node_offset;
    vm->cache_node = k->cache_node;
    vm->shared_anon = k->shared_anon;
    vm->fd = k->fd;
    if (!rangemap_insert(rm, &vm->node)) {
        deallocate(rm->h, vm, sizeof(struct vmap));
        return INVALID_ADDRESS;
    }
    if (vm->fd && !(vm->flags & VMAP_FLAG_TAIL_BSS))
        fetch_and_add(&vm->fd->refcnt, 1);
    if (vm->shared_anon)
        refcount_reserve(&vm->shared_anon->refcount);
    if (vmap_may_write_cache_node(vm))
        vmap_assert(pagecache_node_map_writable(vm->cache_node));
    vmap_paranoia_locked(rm);
    return vm;
}

static void vmap_hold_backing(vmap k)
{
    if (k->fd && !(k->flags & VMAP_FLAG_TAIL_BSS))
        fetch_and_add(&k->fd->refcnt, 1);
    if (k->shared_anon)
        refcount_reserve(&k->shared_anon->refcount);
    if (vmap_may_write_cache_node(k))
        vmap_assert(pagecache_node_map_writable(k->cache_node));
}

static void vmap_release_backing(vmap k)
{
    if (vmap_may_write_cache_node(k))
        pagecache_node_unmap_writable(k->cache_node);
    if (k->shared_anon)
        refcount_release(&k->shared_anon->refcount);
    if (k->fd && !(k->flags & VMAP_FLAG_TAIL_BSS))
        fdesc_put(k->fd);
}

vmap allocate_vmap(process p, range q, struct vmap k)
{
    k.node.r = q;
    vmap_lock(p);
    vmap v = allocate_vmap_locked(p->vm->vmaps, &k);
    vmap_unlock(p);
    return v;
}

static inline boolean validate_mmap_range(process p, range q)
{
    return range_valid(q) && q.start >= p->vm->mmap_min_addr && q.end <= USER_LIMIT;
}

closure_function(1, 1, boolean, proc_virt_gap_handler,
                 u64 *, addr,
                 range r)
{
    *bound(addr) = r.start;
    return true;
}

u64 vmap_select_addr(u64 size, range r, boolean randomize)
{
    if (randomize)
        /* ensure there is non-zero room for randomization */
        r.end -= PAGESIZE;
    if (size > range_span(r))
        return INVALID_PHYSICAL;
    if (randomize) {
        u64 offset = (random_u64() % ((range_span(r) + 2 * PAGESIZE - size) >> PAGELOG)) << PAGELOG;
        return r.start + offset;
    }
    return r.start;
}

closure_function(3, 1, boolean, proc_virt_node_handler,
                 u64, size, boolean, randomize, u64 *, addr,
                 rmnode n)
{
    u64 candidate_addr = *bound(addr);
    if (candidate_addr == INVALID_PHYSICAL)
        return true;
    u64 limit = n->r.start;
    vmap next = (vmap)n;
    if (next->flags & VMAP_FLAG_STACK)
        limit -= PROCESS_STACK_GUARD_GAP;
    candidate_addr = vmap_select_addr(bound(size), irange(candidate_addr, limit), bound(randomize));
    *bound(addr) = candidate_addr;
    return (candidate_addr == INVALID_PHYSICAL);
}

/* Does NOT mark the returned address as allocated in the virtual heap. */
static u64 process_get_virt_range_locked(process_vm vm, u64 size, range region)
{
    assert(!(size & PAGEMASK));
    vmap_heap vmh = (vmap_heap)vm->virtual;
    u64 addr = INVALID_PHYSICAL;
    boolean randomize = vmh->randomize;
    int res = rangemap_range_lookup_with_gaps(vm->vmaps, region,
                                              stack_closure(proc_virt_node_handler, size, randomize,
                                                            &addr),
                                              stack_closure(proc_virt_gap_handler, &addr));
    if ((res == RM_MATCH) && (addr != INVALID_PHYSICAL))
        /* The gap handler set a candidate address, but there are no nodes after the last gap, thus
         * the node handler did not have a chance to select an address from the candidate address.
         */
        addr = vmap_select_addr(size, irange(addr, region.end), randomize);
    return addr;
}

u64 process_get_virt_range(process p, u64 size, range region)
{
    vmap_lock(p);
    u64 ret = process_get_virt_range_locked(p->vm, size, region);
    vmap_unlock(p);
    return ret;
}

static u64 process_allocate_range_locked(process_vm pvm, u64 size, struct vmap k, range region)
{
    u64 virt_addr = process_get_virt_range_locked(pvm, size, region);
    if (virt_addr != INVALID_PHYSICAL) {
        k.node.r = irangel(virt_addr, size);
        vmap vm = allocate_vmap_locked(pvm->vmaps, &k);
        if (vm == INVALID_ADDRESS)
            virt_addr = INVALID_PHYSICAL;
    }
    vmap_paranoia_locked(pvm->vmaps);
    return virt_addr;
}

static void process_remove_range_locked(process p, range q, boolean unmap);

void *process_map_physical(process p, u64 phys_addr, u64 size, u64 vmflags)
{
    vmap_debug("%s: phys_addr 0x%lx, size 0x%lx, vmflags 0x%lx\n",
               func_ss, phys_addr, size, vmflags);
    vmap_lock(p);
    u64 virt_addr = process_allocate_range_locked(p->vm, size, ivmap(vmflags, 0, 0, 0, 0),
                                                  PROCESS_VIRTUAL_MMAP_RANGE);
    vmap_unlock(p);
    if (virt_addr == INVALID_PHYSICAL)
        return INVALID_ADDRESS;
    address_space_map(p->vm->as, virt_addr, phys_addr, size, pageflags_from_vmflags(vmflags));
    return pointer_from_u64(virt_addr);
}

boolean adjust_process_heap(process p, range new)
{
    vmap_lock(p);
    boolean inserted = rangemap_reinsert(p->vm->vmaps, &p->vm->heap_map->node, new);
    vmap_unlock(p);
    return inserted;
}

sysreturn mremap(void *old_address, u64 old_size, u64 new_size, int flags, void *new_address)
{
    process p = current->p;
    sysreturn rv;

    old_size = pad(old_size, PAGESIZE);
    range old = irangel(u64_from_pointer(old_address), old_size);
    if ((old.start & PAGEMASK) ||
        (flags & ~(MREMAP_MAYMOVE | MREMAP_FIXED)) ||
        new_size == 0 ||
        (old_size == 0 && (flags & MREMAP_MAYMOVE) == 0)) {
        return -EINVAL;
    }

    /* begin locked portion...no direct returns */
    vmap_lock(p);
    vmap old_vmap = (vmap)rangemap_lookup(p->vm->vmaps, old.start);
    if (old_vmap == INVALID_ADDRESS || !range_contains(old_vmap->node.r, old)) {
        vmap_debug("no match, old_vmap %p, old %R\n", old_vmap, old);
        rv = -EFAULT;
        goto unlock_out;
    }

    if (old_size == 0) {
        if ((old_vmap->flags & VMAP_FLAG_SHARED) == 0) {
            rv = -EINVAL;
            goto unlock_out;
        }
        new_size = MIN(new_size, (range_span(old_vmap->node.r) -
                                  (old.start - old_vmap->node.r.start)));
        old.end = old.start + new_size;
    }

    /* only remap mmap, non-custom regions */
    struct vmap k;
    runtime_memcpy(&k, old_vmap, sizeof(k));
    if ((k.flags & VMAP_FLAG_MMAP) == 0 || (k.flags & VMAP_MMAP_TYPE_CUSTOM)) {
        rv = -EINVAL;
        goto unlock_out;
    }
    if (k.cache_node || k.shared_anon)
        k.node_offset += old.start - old_vmap->node.r.start;

    range new;
    boolean remap_old = false;
    new_size = pad(new_size, PAGESIZE);
    if (flags & MREMAP_FIXED) {
        if ((flags & MREMAP_MAYMOVE) == 0) {
            rv = -EINVAL;
            goto unlock_out;
        }
        new = irangel(u64_from_pointer(new_address), new_size);
        if ((new.start & PAGEMASK) || ranges_intersect(old, new) || !validate_mmap_range(p, new)) {
            rv = -EINVAL;
            goto unlock_out;
        }
        remap_old = true;
    } else {
        if (new_size > old_size) {
            /* grow check */
            new = irangel(old.start, new_size);
            range delta = irange(old.end, new.end);
            if (rangemap_range_intersects(p->vm->vmaps, delta)) {
                /* collision; allocate new */
                if ((flags & MREMAP_MAYMOVE) == 0) {
                    rv = -ENOMEM;
                    goto unlock_out;
                }
                u64 vnew = process_get_virt_range_locked(p->vm, new_size, PROCESS_VIRTUAL_MMAP_RANGE);
                if (vnew == (u64)INVALID_ADDRESS) {
                    msg_err("%s: failed to allocate virtual memory, size %ld", func_ss, new_size);
                    rv = -ENOMEM;
                    goto unlock_out;
                }
                new = irangel(vnew, new_size);
                vmap_debug("new alloc: %R\n", new);
                remap_old = true;
            } else {
                vmap_debug("extending: %R\n", new);
            }
        } else {
            if (new_size < old_size) {
                range delta = irange(old.start + new_size, old.end);
                vmap_debug("shrinking: remove %R, new %R\n", delta, irangel(old.start, new_size));
                process_remove_range_locked(p, delta, true);
            }
            rv = sysreturn_from_pointer(old.start);
            goto unlock_out;
        }
    }

    vmap_hold_backing(&k);
    process_remove_range_locked(p, old, false);

    /* remove mappings under fixed area */
    if (flags & MREMAP_FIXED)
        process_remove_range_locked(p, new, true);

    /* create new vmap with old attributes */
    k.node.r = new;
    vmap v = allocate_vmap_locked(p->vm->vmaps, &k);
    vmap_release_backing(&k);
    if (v == INVALID_ADDRESS) {
        msg_err("%s: failed to allocate vmap", func_ss);
        rv = -ENOMEM;
        goto unlock_out;
    }
    rv = sysreturn_from_pointer(new.start);

    if (remap_old) {
        /* remap existing portion */
        thread_log(current, "   remapping existing portion at 0x%lx (old %R)",
                   new.start, old);
        address_space_remap_pages(p->vm->as, new.start, old.start, range_span(old));
    }
    vmap_unlock(p);
    if (k.cache_node)
        pagecache_node_move_mapping(k.cache_node, p->vm->as, old, new, k.node_offset,
                                    !!(k.flags & VMAP_FLAG_SHARED));
    return rv;
  unlock_out:
    vmap_unlock(p);
    return rv;
}

closure_function(3, 3, boolean, mincore_fill_vec,
                 u64, base, u64, nr_pgs, u8 *, vec,
                 int level, u64 addr, pteptr entry)
{
    pte e = pte_from_pteptr(entry);
    u64 pgoff, i, size;

    if (pte_is_present(e) &&
        (size = pte_map_size(level, e)) != INVALID_PHYSICAL) {
        if (addr <= bound(base))
            pgoff = 0;
        else
            pgoff = ((addr - bound(base)) >> PAGELOG);

        assert(size >= PAGESIZE);
        assert((size & (size - 1)) == 0);
        size >>= PAGELOG;
        u64 foff = pgoff ? 0 : (addr & (size - 1)) >> PAGELOG;

        for (i = 0; (i < size - foff) && (pgoff + i < bound(nr_pgs)); i++)
            bound(vec)[pgoff + i] = 1;
    }

    return true;
}

closure_func_basic(range_handler, boolean, mincore_vmap_gap,
                   range r)
{
    msg_info("mincore: found gap [0x%lx, 0x%lx)", r.start, r.end);
    return false;
}

static sysreturn mincore(void *addr, u64 length, u8 *vec)
{
    u64 start, nr_pgs;

    start = u64_from_pointer(addr);
    if (start & MASK(PAGELOG))
        return -EINVAL;

    length = pad(length, PAGESIZE);
    nr_pgs = length >> PAGELOG;

    if (!fault_in_user_memory(vec, nr_pgs, true))
        return -EFAULT;

    /* -ENOMEM if any unmapped gaps in range */
    process p = current->p;
    vmap_lock(p);
    boolean found = rangemap_range_find_gaps(p->vm->vmaps, irangel(start, length),
                                             stack_closure_func(range_handler, mincore_vmap_gap)) ==
            RM_ABORT;
    vmap_unlock(p);
    if (found)
        return -ENOMEM;

    runtime_memset(vec, 0, nr_pgs);
    address_space_traverse_ptes(p->vm->as, start, length,
                                stack_closure(mincore_fill_vec, start, nr_pgs, vec));
    return 0;
}

closure_function(1, 1, boolean, vmap_update_protections_validate,
                 u32, newflags,
                 rmnode node)
{
    if (bound(newflags) & ~((vmap)node)->allowed_flags)
        return false;
    return true;
}

static void vmap_set_offsets(vmap to, vmap from, u64 delta)
{
    to->node_offset = (from->cache_node || from->shared_anon) ? from->node_offset + delta : 0;
    if (from->flags & VMAP_FLAG_TAIL_BSS) {
        assert(from->bss_offset > delta);
        to->bss_offset = from->bss_offset - delta;
    }
}

/*
   case 1: !head && !tail

   node        |------------|
   i:      <...|------------|...>    i may extend outside of node

   case 2: head && !tail

   node:     |---------------|
   i:           |---------------|

   case 3: !head && tail

   node:        |---------------|
   i:        |---------------|

   case 4: head && tail

   node:   <...|------------|...>    node may extend outside of i
   i:          |------------|
*/

static void alter_vmap_key(vmap k, vmap match, u32 flags, u64 offset_delta)
{
    k->flags = flags;
    k->allowed_flags = match->allowed_flags;
    k->cache_node = match->cache_node;
    k->shared_anon = match->shared_anon;
    if (!(flags & VMAP_FLAG_TAIL_BSS))
        k->fd = match->fd;
    vmap_set_offsets(k, match, offset_delta);
}

static void vmap_update_flags_intersection(rangemap pvmap, range q, u32 clear_mask, u32 set_mask,
                                          vmap match)
{
    vmap_debug("%s: vm %p %R prev flags 0x%x\n", func_ss, match, match->node.r, match->flags);
    if ((match->flags & clear_mask) == set_mask)
        return;

    range rn = match->node.r;
    range ri = range_intersection(q, rn);

    boolean head = ri.start > rn.start;
    boolean tail = ri.end < rn.end;

    u32 newflags = (match->flags & ~clear_mask) | set_mask;

    if (!head && !tail) {
        /* updating flags may result in adjacent maps with same attributes;
           removing and reinserting the node will take care of merging */
        rangemap_remove_node(pvmap, &match->node);
        match->flags = newflags;
        vmap_assert(allocate_vmap_locked(pvmap, match) != INVALID_ADDRESS);
        deallocate_vmap_locked(pvmap, match);
        return;
    }

    struct vmap k;
    if (head) {
        /* split non-intersecting part of node */
        vmap_assert(rangemap_reinsert(pvmap, &match->node, irange(rn.start, ri.start)));

        /* create node for intersection */
        alter_vmap_key(&k, match, newflags, ri.start - rn.start);
        k.node.r = ri;
        vmap_assert(allocate_vmap_locked(pvmap, &k) != INVALID_ADDRESS);

        if (tail) {
            /* create node at tail end */
            alter_vmap_key(&k, match, match->flags, ri.end - rn.start);
            k.node.r = irange(ri.end, rn.end);
            vmap_assert(allocate_vmap_locked(pvmap, &k) != INVALID_ADDRESS);
        }
    } else {
        /* move node start back */
        vmap_assert(rangemap_reinsert(pvmap, &match->node, irange(ri.end, rn.end)));
        alter_vmap_key(&k, match, newflags, ri.start - rn.start);
        vmap_set_offsets(match, match, ri.end - rn.start);

        /* create node for intersection */
        k.node.r = ri;
        vmap_assert(allocate_vmap_locked(pvmap, &k) != INVALID_ADDRESS);
    }
}

closure_func_basic(range_handler, boolean, vmap_update_protections_gap,
                   range r)
{
    vmap_debug("%s: gap %R\n", func_ss, r);
    msg_info("mprotect: found gap [0x%lx, 0x%lx)", r.start, r.end);
    return false;
}

static sysreturn vmap_update_protections_locked(process p, range q, u32 newflags)
{
    assert((q.start & MASK(PAGELOG)) == 0);
    assert((q.end & MASK(PAGELOG)) == 0);
    assert(range_span(q) > 0);

    vmap_debug("%s: q %R newflags 0x%x\n", func_ss, q, newflags);
    rangemap pvmap = p->vm->vmaps;
    if (!memory_is_user(pointer_from_u64(q.start), range_span(q)) ||
        (rangemap_range_find_gaps(pvmap, q,
                                  stack_closure_func(range_handler, vmap_update_protections_gap))
         == RM_ABORT))
        return -ENOMEM;

    rmnode_handler v_handler = stack_closure(vmap_update_protections_validate, newflags);
    int res = rangemap_range_lookup(pvmap, q, v_handler);
    if (res == RM_NOMATCH)
        return -ENOMEM;
    else if (res == RM_ABORT)
        return -EACCES;

    /* updating protections can lead to merging of nodes, so we cannot traverse */
    range r = q;
    while (range_span(r)) {
        vmap vm = (vmap)rangemap_lookup(pvmap, r.start);
        vmap_assert(vm != INVALID_ADDRESS);
        vmap_update_flags_intersection(pvmap, q, VMAP_FLAG_PROT_MASK, newflags, vm);
        r.start = MIN(r.end, vm->node.r.end);
    }
    address_space_update_map_flags(p->vm->as, q.start, range_span(q), pageflags_from_vmflags(newflags));
    vmap_paranoia_locked(pvmap);
    return 0;
}

sysreturn mprotect(void * addr, u64 len, int prot)
{
    if (len == 0)
        return 0;

    u64 where = u64_from_pointer(addr);
    u64 padlen = pad(len, PAGESIZE);
    if ((where & MASK(PAGELOG)))
        return -EINVAL;

    u64 new_vmflags = 0;
    if ((prot & PROT_READ))
        new_vmflags |= VMAP_FLAG_READABLE;
    if ((prot & PROT_WRITE))
        new_vmflags |= VMAP_FLAG_WRITABLE;
    if ((prot & PROT_EXEC))
        new_vmflags |= VMAP_FLAG_EXEC;

    process p = current->p;
    vmap_lock(p);
    sysreturn result = vmap_update_protections_locked(p, irangel(where, padlen), new_vmflags);
    vmap_unlock(p);
    return result;
}

/* blow a hole in the process address space intersecting q */
closure_function(3, 1, boolean, vmap_remove_intersection,
                 rangemap, pvmap, range, q, vmap_handler, unmap,
                 vmap match)
{
    rmnode node = &match->node;
    thread_log(current, "%s: q %R, r %R", func_ss, bound(q), node->r);
    rangemap pvmap = bound(pvmap);
    range rn = node->r;
    range ri = range_intersection(bound(q), rn);
    vmap_debug("%s: vm %p %R, ri %R\n", func_ss, match, rn, ri);

    /* trim match at both head and tail ends */
    boolean head = ri.start > rn.start;
    boolean tail = ri.end < rn.end;

    if (!head && !tail) {
        rangemap_remove_node(pvmap, node);
    } else if (head) {
        /* truncate node at start */
        vmap_assert(rangemap_reinsert(pvmap, node, irange(rn.start, ri.start)));

        if (tail) {
            struct vmap k;
            alter_vmap_key(&k, match, match->flags, ri.end - rn.start);
            /* create node at tail end */
            k.node.r = irange(ri.end, rn.end);
            vmap_assert(allocate_vmap_locked(pvmap, &k) != INVALID_ADDRESS);
        }
    } else {
        /* tail only: move node start back */
        vmap_assert(rangemap_reinsert(pvmap, node, irange(ri.end, rn.end)));
    }
    if (bound(unmap)) {
        struct vmap k;
        alter_vmap_key(&k, match, match->flags, ri.start - rn.start);
        k.node.r = ri;
        apply(bound(unmap), &k);
    }
    if (!head) {
        if (tail)
            match->node_offset += ri.end - rn.start;
        else
            deallocate_vmap_locked(pvmap, match);
    }
    vmap_paranoia_locked(pvmap);
    return true;
}

static void vmap_unmap_page_range(process_vm vm, vmap k, boolean del_mappings)
{
    range r = k->node.r;
    int type = k->flags & VMAP_MMAP_TYPE_MASK;
    u64 len = range_span(r);
    switch (type) {
    case VMAP_MMAP_TYPE_ANONYMOUS:
        if (k->shared_anon)
            address_space_unmap(vm->as, r.start, len);
        else
            address_space_unmap_and_free_phys(vm->as, r.start, len);
        break;
    case VMAP_MMAP_TYPE_FILEBACKED:
        pagecache_node_unmap_pages(k->cache_node, vm->as, r, k->node_offset, del_mappings);
        break;
    case VMAP_MMAP_TYPE_CUSTOM:
        address_space_unmap(vm->as, r.start, len);
        break;
    }
}

closure_function(1, 1, boolean, vmap_unmap,
                 process, p,
                 vmap v)
{
    vmap_unmap_page_range(bound(p)->vm, v, true);
    return true;
}

static void process_remove_range_locked(process p, range q, boolean unmap)
{
    vmap_debug("%s: q %R\n", func_ss, q);
    vmap_handler vh = unmap ? stack_closure(vmap_unmap, p) : 0;
    rangemap_range_lookup(p->vm->vmaps, q,
                          (rmnode_handler)stack_closure(vmap_remove_intersection, p->vm->vmaps, q, vh));
}

void truncate_file_maps(fsfile f, u64 new_length)
{
    pagecache_node_unmap_range(fsfile_get_cachenode(f), irange(pad(new_length, PAGESIZE), infinity));
}

closure_func_basic(vmap_handler, boolean, msync_vmap,
                   vmap vm)
{
    if ((vm->flags & VMAP_FLAG_SHARED) &&
        (vm->flags & VMAP_FLAG_MMAP) &&
        (vm->flags & VMAP_MMAP_TYPE_MASK) == VMAP_MMAP_TYPE_FILEBACKED) {
        vmap_assert(vm->cache_node);
        pagecache_node_scan(vm->cache_node, current->p->vm->as, vm->node.r, 0);
    }
    return true;
}

static sysreturn msync(void *addr, u64 length, int flags)
{
    int syncflags = MS_ASYNC | MS_SYNC;
    if ((flags & ~(MS_ASYNC | MS_SYNC | MS_INVALIDATE)) ||
        (flags & syncflags) == syncflags)
        return -EINVAL;

    boolean have_gap = false;
    if (flags & MS_SYNC) {
        process p = current->p;
        range q = irangel(u64_from_pointer(addr), pad(length, PAGESIZE));
        have_gap = (vmap_range_walk(p->vm, q, stack_closure_func(vmap_handler, msync_vmap), false) ==
                RM_ABORT);
    }

    /* TODO: Linux appears to only use MS_INVALIDATE to test whether a
       map is locked (and return -EBUSY if it is). We don't swap out
       pages, so mlock is a stub for us. For now, assume that the safest
       option here is to act as if all files are locked and return
       -EBUSY - but this may need to be revisited. */
    if (flags & MS_INVALIDATE)
        return -EBUSY;

    /* While an fsync for some types of filesystems makes sense here,
       it doesn't seem to be necessary for block storage - unless some
       use case would have an msync followed by a hard reset / poweroff. */
    return have_gap ? -ENOMEM : 0;
}

static sysreturn mmap(void *addr, u64 length, int prot, int flags, int fd, u64 offset)
{
    process p = current->p;

    u64 len = pad(length, PAGESIZE);
    if (len == 0)
        return -EINVAL;

    /* validate parameters and determine vmap flags */
    u64 vmflags = VMAP_FLAG_MMAP;

    int map_type = flags & MAP_TYPE_MASK;
    if (map_type == MAP_SHARED || map_type == MAP_SHARED_VALIDATE)
        vmflags |= VMAP_FLAG_SHARED;
    else if (map_type != MAP_PRIVATE)
        return -EINVAL;

    if ((prot & PROT_READ))
        vmflags |= VMAP_FLAG_READABLE;
    if ((prot & PROT_WRITE))
        vmflags |= VMAP_FLAG_WRITABLE;
    if ((prot & PROT_EXEC))
        vmflags |= VMAP_FLAG_EXEC;

    if (flags & MAP_UNINITIALIZED) {
        return -EINVAL;
    }
    if (flags & MAP_GROWSDOWN) {
        vmflags |= VMAP_FLAG_STACK;
    }

    /* ignore MAP_DENYWRITE, MAP_EXECUTABLE, MAP_LOCKED, MAP_STACK, MAP_NORESERVE */
    int valid_flags = MAP_TYPE_MASK | MAP_FIXED | MAP_ANONYMOUS |
        MAP_GROWSDOWN | MAP_DENYWRITE | MAP_EXECUTABLE | MAP_LOCKED | MAP_NORESERVE |
        MAP_POPULATE | MAP_NONBLOCK | MAP_STACK | MAP_HUGETLB | MAP_SYNC |
        MAP_FIXED_NOREPLACE | MAP_UNINITIALIZED |
#ifdef __x86_64__
        MAP_32BIT |
#endif
        (HUGETLB_FLAG_ENCODE_MASK << HUGETLB_FLAG_ENCODE_SHIFT);
    int unknown_flags = flags & ~valid_flags;
    if (unknown_flags) {
        return -EINVAL;
    }

    /* handle fixed address or hint */
    fdesc desc = 0;
    shared_anon sa = 0;
    pagecache_node writable_node = 0;
    sysreturn ret = -EINVAL;
    boolean fixed = (flags & (MAP_FIXED | MAP_FIXED_NOREPLACE)) != 0;
    range q = irangel(u64_from_pointer(addr), len);
    vmap_lock(p);
    if (fixed) {
	/* Must be page-aligned */
	if (q.start & MASK(PAGELOG)) {
	    goto out_unlock;
	}
        if (!validate_mmap_range(p, q)) {
            goto out_unlock;
        }
        if ((flags & MAP_FIXED_NOREPLACE) &&
            rangemap_range_intersects(p->vm->vmaps, q)) {
            msg_info("%s: MAP_FIXED_NOREPLACE and collision in range %R", func_ss, q);
            ret = -EEXIST;
            goto out_unlock;
        }
    } else if (q.start != 0) {
        q = irangel(pad(q.start, PAGESIZE), len);
        if (rangemap_range_intersects(p->vm->vmaps, q)) {
            thread_log(current, "   hint %R collides, reverting to allocation", q);
            q.start = 0;
        }
    }

    /* having checked parameters, procure backing resources */
    fsfile fsf = 0;
    u32 allowed_flags;
    u64 vmap_mmap_type;
    pagecache_node node = 0;
    if (flags & MAP_ANONYMOUS) {
        vmap_mmap_type = VMAP_MMAP_TYPE_ANONYMOUS;
        allowed_flags = anon_perms(p);
        if (vmflags & VMAP_FLAG_SHARED) {
            sa = allocate_shared_anon();
            if (sa == INVALID_ADDRESS) {
                sa = 0;
                ret = -ENOMEM;
                goto out_unlock;
            }
            offset = 0;
        } else {
            vmflags |= VMAP_FLAG_THP;
        }
    } else {
        desc = fdesc_get(p, fd);
        if (!desc) {
            ret = -EBADF;
            goto out_unlock;
        }
        switch (desc->type) {
        case FDESC_TYPE_REGULAR:
            thread_log(current, "   fd %d: file-backed (regular)", fd);
            vmap_mmap_type = VMAP_MMAP_TYPE_FILEBACKED;
            file f = (file)desc;
            fsf = f->fsf;
            assert(fsf);
            allowed_flags = file_perms(p, f);
            if (!(vmflags & VMAP_FLAG_SHARED)) {
                if (!(allowed_flags & VMAP_FLAG_EXEC) || !proc_is_exec_protected(p))
                    allowed_flags |= VMAP_FLAG_WRITABLE;
            } else {
                filesystem fs = fsf->fs;
                u64 seals;
                boolean sealed = fs->get_seals && (fs->get_seals(fs, fsf, &seals) == 0) &&
                    (seals & (F_SEAL_WRITE | F_SEAL_FUTURE_WRITE));
                if (allowed_flags & VMAP_FLAG_WRITABLE) {
                    pagecache_node pn = fsfile_get_cachenode(fsf);
                    if (!sealed && pagecache_node_map_writable(pn)) {
                        writable_node = pn;
                    } else if (vmflags & VMAP_FLAG_WRITABLE) {
                        ret = -EPERM;
                        goto out_unlock;
                    } else {
                        allowed_flags &= ~VMAP_FLAG_WRITABLE;
                    }
                }
            }
            if (offset & PAGEMASK) {
                ret = -EINVAL;
                goto out_unlock;
            }
            node = fsfile_get_cachenode(fsf);
            thread_log(current, "   associated with cache node %p @ offset 0x%lx", node, offset);
            break;
        default:
            thread_log(current, "   fd %d: custom", fd);
            if (!desc->mmap) {
                msg_info("%s: invalid file type %d", func_ss, desc->type);
                ret = -EINVAL;
                goto out_unlock;
            }
            vmap_mmap_type = VMAP_MMAP_TYPE_CUSTOM;
            allowed_flags = vmflags;
            break;
        }
    }
    if ((vmflags & VMAP_FLAG_PROT_MASK) & ~allowed_flags) {
        msg_info("%s: forbidden access type 0x%x (allowed 0x%x)", func_ss,
                   vmflags & VMAP_FLAG_PROT_MASK, allowed_flags);
        ret = -EACCES;
        goto out_unlock;
    }
    vmflags |= vmap_mmap_type;

    /* allocate or update old mapping */
    struct vmap k = ivmap(vmflags, allowed_flags, offset, node, desc);
    k.shared_anon = sa;
    if (!fixed && q.start == 0) {
        range alloc_region =
#ifdef __x86_64__
            (flags & MAP_32BIT) ? PROCESS_VIRTUAL_32BIT_RANGE :
#endif
            PROCESS_VIRTUAL_MMAP_RANGE;
        u64 vaddr = process_get_virt_range_locked(p->vm, len, alloc_region);
        if (vaddr == INVALID_PHYSICAL) {
            ret = -ENOMEM;
            msg_warn("%s failed to get virtual address range", func_ss);
            goto out_unlock;
        }
        q = irangel(vaddr, len);
    }
    if (vmap_mmap_type == VMAP_MMAP_TYPE_CUSTOM) {
        k.node.r = q;
        ret = apply(desc->mmap, &k, offset);
        if (ret < 0) {
            msg_info("%s: custom handler failed with %ld", func_ss, ret);
            goto out_unlock;
        }
    }
    if (fixed)
        process_remove_range_locked(p, q, vmap_mmap_type != VMAP_MMAP_TYPE_CUSTOM);
    k.node.r = q;
    vmap vm = allocate_vmap_locked(p->vm->vmaps, &k);
    vmap_assert(vm != INVALID_ADDRESS);
    vm->fault = k.fault;
    vmap_unlock(p);

    if (vmap_mmap_type == VMAP_MMAP_TYPE_FILEBACKED)
        pagecache_node_add_mapping(node, p->vm->as, irangel(q.start, len), offset,
                                   !!(vmflags & VMAP_FLAG_SHARED));

    /* as man page suggests, ignore MAP_POPULATE if MAP_NONBLOCK is specified */
    if ((flags & (MAP_POPULATE | MAP_NONBLOCK)) == MAP_POPULATE && (prot & PROT_READ)) {
        vmap_debug("   faulting in %R\n", q);
        fault_in_user_memory(pointer_from_u64(q.start), len, false);
    }
    ret = q.start;
  out:
    if (writable_node)
        pagecache_node_unmap_writable(writable_node);
    if (desc)
        fdesc_put(desc);
    if (sa)
        refcount_release(&sa->refcount);
    return ret;
  out_unlock:
    vmap_unlock(p);
    goto out;
}

static sysreturn munmap(void *addr, u64 length)
{
    process p = current->p;

    u64 where = u64_from_pointer(addr);
    if ((where & MASK(PAGELOG)) || length == 0)
        return -EINVAL;

    vmap_lock(p);
    process_remove_range_locked(p, irangel(where, pad(length, PAGESIZE)), true);
    vmap_unlock(p);
    return 0;
}

closure_function(2, 1, boolean, madvise_vmap_validate,
                 int, advice, sysreturn *, rv,
                 rmnode n)
{
    return true;
}

sysreturn madvise(void *addr, s64 length, int advice)
{
    if ((u64_from_pointer(addr) & PAGEMASK) || (length < 0))
        return -EINVAL;
    u32 clear_mask = 0, set_mask = 0;
    switch (advice) {
    case MADV_DONTNEED:
        break;
    case MADV_HUGEPAGE:
        set_mask = VMAP_FLAG_THP;
        break;
    case MADV_NOHUGEPAGE:
        clear_mask = VMAP_FLAG_THP;
        break;
    default:
        return 0;   /* ignore non-supported advice values */
    }
    process p = current->p;
    rangemap vmaps = p->vm->vmaps;
    range q = irangel(u64_from_pointer(addr), pad(length, PAGESIZE));
    sysreturn rv = 0;
    rmnode_handler vmap_handler = stack_closure(madvise_vmap_validate, advice, &rv);
    range_handler gap_handler = stack_closure_func(range_handler, vmap_validate_gap);
    vmap_lock(p);
    int res = rangemap_range_lookup_with_gaps(vmaps, q, vmap_handler, gap_handler);
    if (res == RM_MATCH) {
        while (range_span(q)) {
            vmap vm = (vmap)rangemap_lookup(vmaps, q.start);
            if (advice == MADV_DONTNEED) {
                range ri = range_intersection(q, vm->node.r);
                struct vmap k;
                alter_vmap_key(&k, vm, vm->flags, ri.start - vm->node.r.start);
                k.node.r = ri;
                vmap_unmap_page_range(p->vm, &k, false);
            } else {
                vmap_update_flags_intersection(vmaps, q, clear_mask, set_mask, vm);
            }
            q.start = MIN(q.end, vm->node.r.end);
        }
        rv = 0;
    } else {
        if (rv == 0)    /* either the address range is not mapped, or there are unmapped gaps */
            rv = -ENOMEM;
    }
    vmap_unlock(p);
    return rv;
}

/* kernel start */
extern void * START;

static u64 vmh_alloc(struct heap *h, bytes b)
{
    process_vm vm = ((vmap_heap)h)->vm;
    process_vm_lock(vm);
    u64 ret = process_allocate_range_locked(vm, b, ivmap(0, 0, 0, 0, 0),
                                            PROCESS_VIRTUAL_MMAP_RANGE);
    process_vm_unlock(vm);
    return ret;
}

void vmh_dealloc(struct heap *h, u64 a, bytes b)
{
    vmap_heap vmh = (vmap_heap)h;
    process_vm vm = vmh->vm;
    range q = irangel(a, b);
    vmap_range_walk(vm, q, stack_closure(vmap_remove_intersection, vm->vmaps, q, 0), true);
}

closure_function(1, 1, boolean, vmh_allocated_handler,
                 u64 *, allocated,
                 vmap vm)
{
    *bound(allocated) += range_span(vm->node.r);
    return true;
}

bytes vmh_allocated(struct heap *h)
{
    vmap_heap vmh = (vmap_heap)h;
    bytes allocated = 0;
    vmap_range_walk(vmh->vm, irange(0, PROCESS_VIRTUAL_HEAP_LIMIT),
                    stack_closure(vmh_allocated_handler, &allocated), true);
    return allocated;
}

bytes vmh_total(struct heap *h)
{
    return PROCESS_VIRTUAL_HEAP_LIMIT;
}

closure_function(2, 1, boolean, check_vmap_permissions,
                 u64, required_flags, u64, disallowed_flags,
                 vmap vm)
{
    u64 rf = bound(required_flags), df = bound(disallowed_flags);
    return ((vm->flags & rf) == rf && (vm->flags & df) == 0);
}

boolean validate_user_memory_permissions(process p, const void *buf, bytes length,
                                         u64 required_flags, u64 disallowed_flags)
{
    u64 addr = u64_from_pointer(buf);
    range q = irange(addr, pad(addr + length, PAGESIZE));
    int res = vmap_range_walk(p->vm, q, stack_closure(check_vmap_permissions, required_flags,
                                                  disallowed_flags),
                              false);
    return res == RM_MATCH;
}

boolean fault_in_memory(const void *buf, bytes length)
{
    context ctx = get_current_context(current_cpu());
    if (context_set_err(ctx))
        return false;
    /* Fault in non-present pages by touching each page in buffer */
    touch_memory(buf, length);
    context_clear_err(ctx);
    return true;
}

boolean fault_in_user_memory(const void *buf, bytes length, boolean writable)
{
    if (writable) {
        if (!validate_user_memory_permissions(current->p, buf, length, VMAP_FLAG_WRITABLE, 0))
            return false;
    } else {
        if (!memory_is_user(buf, length))
            return false;
    }

    return fault_in_memory(buf, length);
}

void mmap_init(tuple root)
{
    kernel_heaps kh = get_kernel_heaps();
    mmap_info.h = heap_locked(kh);
    init_futices(mmap_info.h);
    value transparent_hugepage = get(root, sym(transparent_hugepage));
    if (transparent_hugepage) {
        if (is_string(transparent_hugepage)) {
            if (!buffer_strcmp(transparent_hugepage, "never"))
                mmap_info.thp_max_size = PAGESIZE;
        }
        if (!mmap_info.thp_max_size)
            msg_err("transparent_hugepage: invalid value '%v'", transparent_hugepage);
    }
    if (!mmap_info.thp_max_size)
        mmap_info.thp_max_size = pageheap_max_pagesize();
    mmap_info.virtual_backed = kh->pages;
    list_init(&mmap_info.pf_freelist);
    spin_lock_init(&mmap_info.pf_lock);
}

closure_func_basic(thunk, void, process_vm_free)
{
    process_vm vm = struct_from_closure(process_vm, free);
    process_vm_free_mappings(vm);
    if (vm->image)
        fsfile_release(vm->image);
    if (vm->interp)
        fsfile_release(vm->interp);
    deallocate_rangemap(vm->vmaps, 0);
    deallocate(mmap_info.h, vm->virtual, sizeof(struct vmap_heap));
    address_space_release(vm->as);
    deallocate(mmap_info.h, vm, sizeof(*vm));
}

static process_vm process_vm_alloc(address_space as, u64 mmap_min_addr, boolean randomize)
{
    heap h = mmap_info.h;
    process_vm vm = allocate(h, sizeof(*vm));
    if (vm == INVALID_ADDRESS)
        return vm;
    vm->vmaps = allocate_rangemap(h);
    if (vm->vmaps == INVALID_ADDRESS)
        goto fail_vm;
    vmap_heap vmh = allocate(h, sizeof(struct vmap_heap));
    if (vmh == INVALID_ADDRESS)
        goto fail_vmaps;
    vmh->h.alloc = vmh_alloc;
    vmh->h.dealloc = vmh_dealloc;
    vmh->h.allocated = vmh_allocated;
    vmh->h.pagesize = PAGESIZE;
    vmh->h.total = vmh_total;
    vmh->vm = vm;
    vmh->randomize = randomize;
    vm->virtual = &vmh->h;
    vm->as = as;
    spin_lock_init(&vm->vmap_lock);
    vm->mmap_min_addr = mmap_min_addr;
    vm->stack_map = vm->heap_map = 0;
    vm->brk = 0;
    vm->heap_base = 0;
    vm->membarrier_private_expedited = false;
    vm->vdso_base = 0;
    vm->image = vm->interp = 0;
    init_refcount(&vm->refcount, 1, init_closure_func(&vm->free, thunk, process_vm_free));
    return vm;
  fail_vmaps:
    deallocate_rangemap(vm->vmaps, 0);
  fail_vm:
    deallocate(h, vm, sizeof(*vm));
    return INVALID_ADDRESS;
}

process_vm mmap_process_init(tuple root)
{
    u64 min_addr;
    if (!get_u64(root, sym(mmap_min_addr), &min_addr))
        min_addr = PAGESIZE;
    address_space as = allocate_address_space(heap_locked(get_kernel_heaps()));
    assert(as != INVALID_ADDRESS);
    process_vm vm = process_vm_alloc(as, min_addr, !get(root, sym(noaslr)));
    assert(vm != INVALID_ADDRESS);

    /* randomly determine vdso/vvar base and track it */
    u64 vdso_size, vvar_size;

    vdso_size = vdso_raw_length;
    vvar_size = VVAR_NR_PAGES * PAGESIZE;

    vm->vdso_base = process_get_virt_range_locked(vm, vdso_size + vvar_size, PROCESS_VIRTUAL_MMAP_RANGE);
    struct vmap k = ivmap(VMAP_FLAG_EXEC, 0, 0, 0, 0);
    k.node.r = irangel(vm->vdso_base, vdso_size);
    vmap_assert(allocate_vmap_locked(vm->vmaps, &k) != INVALID_ADDRESS);

    /* vvar goes right after the vdso */
    k = ivmap(0, 0, 0, 0, 0);
    k.node.r = irangel(vm->vdso_base + vdso_size, vvar_size);
    vmap_assert(allocate_vmap_locked(vm->vmaps, &k) != INVALID_ADDRESS);

#ifdef __x86_64__
    /* Track vsyscall page */
    k = ivmap(VMAP_FLAG_EXEC, 0, 0, 0, 0);
    k.node.r = irangel(VSYSCALL_BASE, PAGESIZE);
    vmap_assert(allocate_vmap_locked(vm->vmaps, &k) != INVALID_ADDRESS);
#endif
    return vm;
}

void register_mmap_syscalls(struct syscall *map)
{
    register_syscall(map, mincore, mincore);
    register_syscall(map, mmap, mmap);
    register_syscall(map, mremap, mremap);
    register_syscall(map, msync, msync);
    register_syscall(map, munmap, munmap);
    register_syscall(map, mprotect, mprotect);
    register_syscall(map, madvise, madvise);
}

enum fork_page_action {
    FORK_PAGE_COW,
    FORK_PAGE_COPY,
    FORK_PAGE_SHARE,
    FORK_PAGE_REFAULT,
};

#define FORK_PTE_BATCH  64

struct fork_pte {
    u64 vaddr;
    physical phys;
    u64 len;
    pageflags flags;
};

closure_function(4, 3, boolean, fork_collect_pte,
                 range, r, u64, resume, struct fork_pte *, batch, int *, count,
                 int level, u64 vaddr, pteptr entry)
{
    pte e = pte_from_pteptr(entry);
    if (!pte_is_present(e) || !pte_is_mapping(level, e))
        return true;
    range m = range_intersection(irangel(vaddr, pte_map_size(level, e)), bound(r));
    if (m.start < bound(resume))
        m.start = bound(resume);
    if (m.start >= m.end)
        return true;
    if (*bound(count) == FORK_PTE_BATCH)
        return false;
    struct fork_pte *fp = &bound(batch)[(*bound(count))++];
    fp->vaddr = m.start;
    fp->phys = page_from_pte(e) + (m.start - vaddr);
    fp->len = range_span(m);
    fp->flags = pageflags_from_pte(e);
    return true;
}

static boolean vmap_is_custom(vmap vm)
{
    return (vm->flags & VMAP_FLAG_MMAP) &&
           ((vm->flags & VMAP_MMAP_TYPE_MASK) == VMAP_MMAP_TYPE_CUSTOM);
}

static boolean vmap_is_static_program(vmap vm)
{
    return (vm->flags & VMAP_FLAG_PROG) && !vm->cache_node;
}

static boolean vmap_is_cow(vmap vm)
{
    return !(vm->flags & VMAP_FLAG_SHARED) && !vmap_is_custom(vm) && !vmap_is_static_program(vm);
}

static enum fork_page_action fork_page_action(vmap vm, u64 vaddr, physical phys)
{
    if (vmap_is_custom(vm))
        return FORK_PAGE_SHARE;
    if (vm->flags & VMAP_FLAG_SHARED)
        return FORK_PAGE_REFAULT;
    if (vmap_is_static_program(vm))
        return (vm->flags & VMAP_FLAG_WRITABLE) ? FORK_PAGE_COPY : FORK_PAGE_SHARE;
    if (vm->cache_node) {
        u64 node_offset = vm->node_offset + (vaddr - vm->node.r.start);
        if (pagecache_node_page_is_cached(vm->cache_node, node_offset, phys))
            return FORK_PAGE_REFAULT;
    }
    return FORK_PAGE_COW;
}

static boolean fork_copy_page(process_vm child, u64 va, physical src, pageflags flags)
{
    void *dst = allocate(mmap_info.virtual_backed, PAGESIZE);
    if (dst == INVALID_ADDRESS)
        return false;
    runtime_memcpy(dst, pointer_from_u64(virt_from_linear_backed_phys(src)), PAGESIZE);
    address_space_map(child->as, va, physical_from_virtual(dst), PAGESIZE, flags);
    return true;
}

static boolean fork_vmap_pages(process_vm child, process_vm parent, vmap vm)
{
    range r = vm->node.r;
    if (vmap_is_cow(vm) && !address_space_protect(parent->as, r.start, range_span(r)))
        return false;
    struct fork_pte batch[FORK_PTE_BATCH];
    u64 resume = r.start;
    while (true) {
        int count = 0;
        boolean done = address_space_traverse_ptes(parent->as, resume, r.end - resume,
                                                   stack_closure(fork_collect_pte, r, resume,
                                                                 batch, &count));
        for (int i = 0; i < count; i++) {
            struct fork_pte *fp = &batch[i];
            for (u64 off = 0; off < fp->len; off += PAGESIZE) {
                u64 va = fp->vaddr + off;
                physical src = fp->phys + off;
                switch (fork_page_action(vm, va, src)) {
                case FORK_PAGE_REFAULT:
                    break;
                case FORK_PAGE_SHARE:
                    address_space_map(child->as, va, src, PAGESIZE, fp->flags);
                    break;
                case FORK_PAGE_COW:
                    if (page_share(src)) {
                        address_space_map(child->as, va, src, PAGESIZE, fp->flags);
                        break;
                    }
                    if (!fork_copy_page(child, va, src, fp->flags))
                        return false;
                    break;
                case FORK_PAGE_COPY:
                    if (!fork_copy_page(child, va, src, fp->flags))
                        return false;
                    break;
                }
            }
            resume = fp->vaddr + fp->len;
        }
        if (done)
            return true;
    }
}

static boolean vmap_inherited(process_vm pvm, vmap vm)
{
    if (vm->flags & (VMAP_FLAG_MMAP | VMAP_FLAG_STACK | VMAP_FLAG_HEAP | VMAP_FLAG_BSS |
                     VMAP_FLAG_PROG))
        return true;
    return (vm->node.r.start == pvm->vdso_base) ||
           (vm->node.r.start == pvm->vdso_base + vdso_raw_length)
#ifdef __x86_64__
           || (vm->node.r.start == VSYSCALL_BASE)
#endif
           ;
}

static boolean vmap_is_special(vmap vm)
{
    return !(vm->flags & (VMAP_FLAG_MMAP | VMAP_FLAG_STACK | VMAP_FLAG_HEAP | VMAP_FLAG_BSS |
                          VMAP_FLAG_PROG));
}

process_vm mmap_process_fork(process_vm parent, sysreturn *rv)
{
#ifdef VM_ISOLATION_SELFTEST_BREAK
    address_space as = parent->as;
    address_space_reserve(as);
#else
    address_space as = allocate_address_space(heap_locked(get_kernel_heaps()));
    if (as == INVALID_ADDRESS) {
        *rv = -ENOMEM;
        return INVALID_ADDRESS;
    }
#endif
    process_vm child = process_vm_alloc(as, parent->mmap_min_addr,
                                        ((vmap_heap)parent->virtual)->randomize);
    if (child == INVALID_ADDRESS) {
        address_space_release(as);
        *rv = -ENOMEM;
        return INVALID_ADDRESS;
    }
    *rv = 0;
    process_vm_lock(parent);
    child->vdso_base = parent->vdso_base;
    child->brk = parent->brk;
    child->membarrier_private_expedited = parent->membarrier_private_expedited;
    child->heap_base = parent->heap_base;
    child->image = parent->image;
    if (child->image)
        fsfile_reserve(child->image);
    child->interp = parent->interp;
    if (child->interp)
        fsfile_reserve(child->interp);
    rangemap_foreach(parent->vmaps, n) {
        vmap vm = (vmap)n;
        if (!vmap_inherited(parent, vm))
            continue;
        struct vmap k;
        runtime_memcpy(&k, vm, sizeof(k));
        vmap cvm = allocate_vmap_locked(child->vmaps, &k);
        if (cvm == INVALID_ADDRESS) {
            *rv = -ENOMEM;
            goto out;
        }
        cvm->fault = vm->fault;
        if (vm == parent->heap_map)
            child->heap_map = cvm;
        if (vm == parent->stack_map)
            child->stack_map = cvm;
        if (vmap_is_special(vm) || (child->as == parent->as))
            continue;
        if (vm->cache_node &&
            (((vm->flags & VMAP_FLAG_MMAP) &&
              ((vm->flags & VMAP_MMAP_TYPE_MASK) == VMAP_MMAP_TYPE_FILEBACKED)) ||
             ((vm->flags & VMAP_FLAG_PROG) && !(vm->flags & VMAP_FLAG_WRITABLE))))
            pagecache_node_add_mapping(vm->cache_node, child->as, n->r, vm->node_offset,
                                       !!(vm->flags & VMAP_FLAG_SHARED));
        if (!fork_vmap_pages(child, parent, vm)) {
            *rv = -ENOMEM;
            goto out;
        }
    }
  out:
    process_vm_unlock(parent);
    if (*rv < 0) {
        process_vm_release(child);
        return INVALID_ADDRESS;
    }
    return child;
}

boolean mmap_cow_page(process_vm pvm, vmap vm, u64 vaddr)
{
    u64 va = vaddr & ~PAGEMASK;
    physical phys = address_space_physical(pvm->as, va);
    if (phys == INVALID_PHYSICAL)
        return true;
    phys &= ~PAGEMASK;
    pageflags flags = pageflags_from_vmflags(vm->flags);
    if (!page_is_shared(phys)) {
        address_space_update_map_flags(pvm->as, va, PAGESIZE, flags);
        return true;
    }
    void *copy = allocate(mmap_info.virtual_backed, PAGESIZE);
    if (copy == INVALID_ADDRESS)
        return false;
    runtime_memcpy(copy, pointer_from_u64(virt_from_linear_backed_phys(phys)), PAGESIZE);
    address_space_replace_page(pvm->as, va, physical_from_virtual(copy), flags);
    if (page_unshare(phys))
        deallocate_u64((heap)heap_physical(get_kernel_heaps()), phys, PAGESIZE);
    return true;
}

/* Releases every mapping of an address space that is going away, including those that munmap
   leaves alone. */
void process_vm_free_mappings(process_vm pvm)
{
    process_vm_lock(pvm);
    vmap vm;
    while ((vm = (vmap)rangemap_first_node(pvm->vmaps)) != INVALID_ADDRESS) {
        range r = vm->node.r;
        rangemap_remove_node(pvm->vmaps, &vm->node);
        if (range_span(r) && (r.end <= USER_LIMIT)) {
            if (vm->flags & VMAP_FLAG_MMAP) {
                vmap_unmap_page_range(pvm, vm, true);
            } else if ((vm->flags & VMAP_FLAG_PROG) && !(vm->flags & VMAP_FLAG_WRITABLE)) {
                if (vm->cache_node)
                    pagecache_node_unmap_pages(vm->cache_node, pvm->as, r, vm->node_offset, true);
                else
                    address_space_unmap(pvm->as, r.start, range_span(r));
            } else if (vm->flags & (VMAP_FLAG_PROG | VMAP_FLAG_STACK | VMAP_FLAG_HEAP |
                                    VMAP_FLAG_BSS)) {
                address_space_unmap_and_free_phys(pvm->as, r.start, range_span(r));
            } else {
                address_space_unmap(pvm->as, r.start, range_span(r));
            }
        }
        deallocate_vmap_locked(pvm->vmaps, vm);
    }
    pvm->heap_map = pvm->stack_map = 0;
    process_vm_unlock(pvm);
}
