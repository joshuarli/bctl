/* TODO: implement drain */

#include <kernel.h>
#include <flush_profile.h>

#ifdef KERNEL
struct spinlock pt_lock;
#endif

//#define PAGE_INIT_DEBUG
//#define PAGE_DEBUG
//#define PAGE_UPDATE_DEBUG
//#define PAGE_TRAVERSE_DEBUG
//#define PAGE_DUMP_ALL

#if defined(PAGE_DEBUG) && !defined(BOOT)
#define page_debug(x, ...) do {tprintf(sym(page), 0, ss("%s: " x), func_ss, ##__VA_ARGS__);} while(0)
#else
#define page_debug(x, ...)
#endif

#ifdef PAGE_INIT_DEBUG
#define page_init_debug(x) early_debug(x)
#define page_init_debug_u64(x) early_debug_u64(x)
#else
#define page_init_debug(x)
#define page_init_debug_u64(x)
#endif

#define PAGEMEM_ALLOC_SIZE  PAGEHEAP_LOWMEM_PAGESIZE

#ifdef KERNEL
#define assert_kernel_range(v, len) do {                                            \
        if (pagemem.kernel_root_sealed && ((v) < USER_LIMIT))                       \
            halt("%s: user address 0x%lx (length 0x%lx) given to the kernel page table\n", \
                 func_ss, (u64)(v), (u64)(len));                                    \
    } while (0)
#else
#define assert_kernel_range(v, len)
#endif

static struct {
    range current_phys;
    heap pageheap;
    range pagevirt;
    u64 physbase;
    u64 levelmask;              /* bitmap of levels allowed to map */
    u64 free_tables;
    boolean kernel_root_sealed;
} pagemem;

#ifdef KERNEL
static struct {
    u32 *extra_mappings;
    u64 pages;
} shared_pages;

u64 dax_phys_start = INVALID_PHYSICAL;
u64 dax_phys_end = 0;
u64 dax_kvirt_start = 0;

void page_register_dax(physical phys, u64 size, u64 kvirt)
{
    dax_phys_start = phys;
    dax_phys_end = phys + size;
    dax_kvirt_start = kvirt;
}

boolean page_share(physical phys)
{
    if (phys_is_dax(phys))
        return true;
    u64 n = phys >> PAGELOG;
    if (n >= shared_pages.pages)
        return false;
    fetch_and_add_32(&shared_pages.extra_mappings[n], 1);
    return true;
}

boolean page_is_shared(physical phys)
{
    if (phys_is_dax(phys))
        return true;
    u64 n = phys >> PAGELOG;
    return (n < shared_pages.pages) && (*(volatile u32 *)&shared_pages.extra_mappings[n] != 0);
}

boolean page_unshare(physical phys)
{
    if (phys_is_dax(phys))
        return false;
    u64 n = phys >> PAGELOG;
    if (n >= shared_pages.pages)
        return true;
    u32 *c = &shared_pages.extra_mappings[n];
    u32 v = *(volatile u32 *)c;
    while (v) {
        if (compare_and_swap_32(c, v, v - 1))
            return false;
        v = *(volatile u32 *)c;
    }
    return true;
}

static void free_unshared_phys(heap h, range r)
{
    u64 run = r.start;
    for (u64 p = r.start; p < r.end; p += PAGESIZE) {
        if (!page_unshare(p)) {
            if (p > run)
                deallocate(h, run, p - run);
            run = p + PAGESIZE;
        }
    }
    if (r.end > run)
        deallocate(h, run, r.end - run);
}

closure_function(1, 1, boolean, shared_pages_limit,
                 u64 *, end,
                 range r)
{
    if (r.end > *bound(end))
        *bound(end) = r.end;
    return true;
}

void init_shared_pages(heap h)
{
    u64 end = 0;
    pageheap_range_foreach(stack_closure(shared_pages_limit, &end));
    u64 pages = end >> PAGELOG;
    shared_pages.extra_mappings = allocate_zero(h, pages * sizeof(u32));
    assert(shared_pages.extra_mappings != INVALID_ADDRESS);
    shared_pages.pages = pages;
}
#endif

#ifndef physical_from_virtual
physical physical_from_virtual(void *x)
{
    u64 a = u64_from_pointer(x);
    if (is_linear_backed_address(a))
        return phys_from_linear_backed_virt(a);
    if (point_in_range(pagemem.pagevirt, a))
        return pagemem.physbase + a - pagemem.pagevirt.start;
    u64 p;
    pagetable_lock();
    p = __physical_from_virtual_locked(x);
    pagetable_unlock();
    return p;
}
#endif

u64 *pointer_from_pteaddr(u64 pa)
{
#ifdef BOOT
    return pointer_from_u64(pa);
#else
    u64 offset = pa - pagemem.physbase;
    return pointer_from_u64(pagemem.pagevirt.start + offset);
#endif
}

void *allocate_table_page(u64 *phys)
{
#ifdef BOOT
    /* Bootloader use: single, identity-mapped pages */
    void *p = allocate_zero(pagemem.pageheap, PAGESIZE);
    *phys = u64_from_pointer(p);
    return p;
#else
    page_init_debug("allocate_table_page:");
    if (pagemem.free_tables) {
        *phys = pagemem.free_tables;
        u64 *p = pointer_from_pteaddr(*phys);
        pagemem.free_tables = p[0];
        zero(p, PAGESIZE);
        return p;
    }
    if (range_span(pagemem.current_phys) == 0) {
        page_init_debug(" [new alloc, pa: ");
        u64 pa = allocate_u64(pagemem.pageheap, PAGEMEM_ALLOC_SIZE);
        if (pa == INVALID_PHYSICAL) {
            msg_err("page table: failed to allocate memory");
            return INVALID_ADDRESS;
        }
        page_init_debug_u64(pa);
        page_init_debug("] ");
        pagemem.current_phys = irangel(pa, PAGEMEM_ALLOC_SIZE);
    }

    *phys = pagemem.current_phys.start;
    pagemem.current_phys.start += PAGESIZE;
    void *p = pointer_from_pteaddr(*phys);
    page_init_debug(" phys: ");
    page_init_debug_u64(*phys);
    page_init_debug("\n");
    zero(p, PAGESIZE);
    return p;
#endif
}

#define PTE_ENTRIES U64_FROM_BIT(9)
static boolean recurse_ptes(u64 pbase, int level, u64 vstart, u64 len, u64 laddr, entry_handler ph)
{
    int shift = pt_level_shift(level);
    u64 lsize = U64_FROM_BIT(shift);
    u64 vaddr = vstart & MASK(VIRTUAL_ADDRESS_BITS);
    u64 start_idx = vaddr > laddr ? ((vaddr - laddr) >> shift) : 0;
    u64 x = vaddr + len - laddr;
    u64 end_idx = MIN(pad(x, lsize) >> shift, PTE_ENTRIES);
    u64 offset = start_idx << shift;

#ifdef PAGE_TRAVERSE_DEBUG
    rprintf("   pbase 0x%lx, level %d, shift %d, lsize 0x%lx, laddr 0x%lx,\n"
            "      start_idx %ld, end_idx %ld, offset 0x%lx\n",
            pbase, level, shift, lsize, laddr, start_idx, end_idx, offset);
#endif

    assert(start_idx <= PTE_ENTRIES);
    assert(end_idx <= PTE_ENTRIES);

    for (u64 i = start_idx; i < end_idx; i++, offset += lsize) {
        u64 addr = (vstart & ~MASK(VIRTUAL_ADDRESS_BITS)) + laddr + (i << shift);
        u64 pteaddr = pbase + (i * sizeof(u64));
        u64 *pte = pointer_from_pteaddr(pteaddr);
#ifdef PAGE_TRAVERSE_DEBUG
        rprintf("   idx %d, offset 0x%lx, addr 0x%lx, pteaddr 0x%lx, *pte %p\n",
                i, offset, addr, pteaddr, *pte);
#endif
        if (!apply(ph, level, addr, pte))
            return false;
        if (pte_is_present(*pte) && level < PT_PTE_LEVEL &&
            (level == PT_FIRST_LEVEL || !pte_is_mapping(level, *pte)) &&
            !recurse_ptes(page_from_pte(*pte), level + 1, vstart, len,
                          laddr + offset, ph))
            return false;
    }
    return true;
}

static boolean traverse_ptes_root(u64 root, u64 vaddr, u64 length, entry_handler ph)
{
#ifdef PAGE_TRAVERSE_DEBUG
    rprintf("traverse_ptes vaddr 0x%lx, length 0x%lx\n", vaddr, length);
#endif
    pagetable_lock();
    boolean result = recurse_ptes(root, PT_FIRST_LEVEL, vaddr, length, 0, ph);
    pagetable_unlock();
    return result;
}

boolean traverse_ptes(u64 vaddr, u64 length, entry_handler ph)
{
    return traverse_ptes_root(get_pagetable_base(vaddr), vaddr, length, ph);
}

closure_func_basic(entry_handler, boolean, dump_entry,
                   int level, u64 vaddr, pteptr entry)
{
    for (int i = 0; i < (level - PT_FIRST_LEVEL); i++)
        early_debug("   ");
    early_debug("v 0x");
    early_debug_u64(vaddr);
    early_debug(" (pte @ 0x");
    early_debug_u64(u64_from_pointer(entry));
    early_debug(") = 0x");
    early_debug_u64(*entry);
    early_debug("\n");
    return true;
}

void dump_page_tables(u64 addr, u64 length)
{
    early_debug("page table dump for address range [");
    early_debug_u64(addr);
    early_debug(", ");
    early_debug_u64(addr + length);
    early_debug(") (length ");
    early_debug_u64(length);
    early_debug(")\n");
    traverse_ptes(addr, length, stack_closure_func(entry_handler, dump_entry));
    early_debug("\n");
}

static u64 loaded_root(u64 vaddr)
{
#ifdef KERNEL
    if (vaddr < USER_LIMIT && kernel_address_space) {
        address_space as = current_cpu()->current_as;
        if (as)
            return as->root;
    }
#endif
    return get_pagetable_base(vaddr);
}

/* called with lock held */
closure_func_basic(entry_handler, boolean, validate_entry,
                   int level, u64 vaddr, pteptr entry)
{
    return pte_is_present(pte_from_pteptr(entry));
}

/* validate that all pages in vaddr range [base, base + length) are present */
boolean validate_virtual(void * base, u64 length)
{
    page_debug("base %p, length 0x%lx\n", base, length);
    u64 p = u64_from_pointer(base) >> VIRTUAL_ADDRESS_BITS;
    if (p != 0 && p != MASK(64-VIRTUAL_ADDRESS_BITS))
        return false;
    return traverse_ptes_root(loaded_root(u64_from_pointer(base)), u64_from_pointer(base), length,
                              stack_closure_func(entry_handler, validate_entry));
}

closure_func_basic(entry_handler, boolean, validate_entry_writable,
                   int level, u64 vaddr, pteptr entry)
{
    pte pte = pte_from_pteptr(entry);
    if (!pte_is_present(pte))
        return false;
    return !pte_is_mapping(level, pte) || pageflags_is_writable(pageflags_from_pte(pte));
}

boolean validate_virtual_writable(void * base, u64 length)
{
    page_debug("base %p, length 0x%lx\n", base, length);
    return traverse_ptes_root(loaded_root(u64_from_pointer(base)), u64_from_pointer(base), length,
                              stack_closure_func(entry_handler, validate_entry_writable));
}

static pte pte_split(pteptr entry, range gap)
{
    u64 new_page_phys;
    u64 *new_page = allocate_table_page(&new_page_phys);
    pte new_pte;
    if (new_page != INVALID_ADDRESS) {
        u64 oldentry = pte_from_pteptr(entry);
        u64 phys = page_from_pte(oldentry);
        u64 flags = flags_from_pte(oldentry);
        for (u64 i = 0; i < gap.start; i++)
            new_page[i] = page_pte(phys + (i << PAGELOG), flags);
        for (u64 i = gap.end; i < PAGESIZE / sizeof(*new_page); i++)
            new_page[i] = page_pte(phys + (i << PAGELOG), flags);
        new_pte = new_level_pte(new_page_phys);
        pte_set(entry, new_pte);
    } else {
        new_pte = INVALID_PHYSICAL;
    }
    return new_pte;
}

/* called with lock held */
closure_function(4, 3, boolean, update_pte_flags,
                 u64, vstart, u64, len, pageflags, flags, flush_entry, fe,
                 int level, u64 addr, pteptr entry)
{
    /* we only care about present ptes */
    pte orig_pte = pte_from_pteptr(entry);
    if (!pte_is_present(orig_pte) || !pte_is_mapping(level, orig_pte))
        return true;

    u64 map_len = pte_map_size(level, orig_pte);
    u64 vstart = bound(vstart);
    u64 map_offset, offset;
    if (addr >= vstart) {
        map_offset = 0;
        offset = addr - vstart;
    } else {
        map_offset = vstart - addr;
        offset = 0;
    }
    u64 phys = page_from_pte(orig_pte);
    u64 flags = bound(flags).w;
#ifdef KERNEL
    if ((level == PT_PTE_LEVEL) && pageflags_is_writable((pageflags){.w = flags}) &&
        page_is_shared(phys))
        flags = pageflags_readonly((pageflags){.w = flags}).w;
#endif
    u64 update_len = MIN(map_len - map_offset, bound(len) - offset);
    if (update_len == map_len) {
        pte_set(entry, (level == PT_PTE_LEVEL) ? page_pte(phys, flags) : block_pte(phys, flags));
    } else {
        if (pte_split(entry, irange(0, 0)) == INVALID_PHYSICAL)
            return false;
    }

#ifdef PAGE_UPDATE_DEBUG
    page_debug("update 0x%lx: pte @ 0x%lx, 0x%lx -> 0x%lx\n", addr, entry, orig_pte,
               pte_from_pteptr(entry));
#endif
    page_invalidate(bound(fe), addr);
    return true;
}

static void update_map_flags_root(u64 root, u64 vaddr, u64 length, pageflags flags)
{
    page_debug("vaddr 0x%lx, length 0x%lx, flags 0x%lx\n", vaddr, length, flags.w);

    flush_entry fe = get_page_flush_entry();
    traverse_ptes_root(root, vaddr, length,
                       stack_closure(update_pte_flags, vaddr, length, flags, fe));
    page_invalidate_sync(fe, 0, false);
#ifdef PAGE_DUMP_ALL
    early_debug("update_map_flags ");
    dump_page_tables(vaddr, length);
#endif
}

/* Update access protection flags for any pages mapped within a given area */
void update_map_flags(u64 vaddr, u64 length, pageflags flags)
{
    assert_kernel_range(vaddr, length);
    update_map_flags_root(get_pagetable_base(vaddr), vaddr, length, flags);
}

/* Replaces a block mapping with a table of PTE-level mappings of the same memory; called with
   lock held, e.g. from an entry handler, in which case traverse_ptes() walks the new table */
boolean split_mapping(pteptr entry)
{
    return pte_split(entry, irange(0, 0)) != INVALID_PHYSICAL;
}

static boolean map_level(u64 *table_ptr, int level, range v, u64 *p, u64 flags, flush_entry fe);

/* called with lock held */
closure_function(5, 3, boolean, remap_entry,
                 u64, root, u64, new, u64, old, u64, len, flush_entry, fe,
                 int level, u64 curr, pteptr entry)
{
    u64 oldentry = pte_from_pteptr(entry);
    if (!pte_is_present(oldentry) || !pte_is_mapping(level, oldentry))
        return true;
    u64 old = bound(old);
    u64 map_offset, offset;
    if (curr >= old) {
        map_offset = 0;
        offset = curr - old;
    } else {
        map_offset = old - curr;
        offset = 0;
    }
    u64 new_curr = bound(new) + offset;
    u64 phys = page_from_pte(oldentry) + map_offset;
    u64 flags = flags_from_pte(oldentry);
    u64 map_len = pte_map_size(level, oldentry);
    u64 remap_len = MIN(map_len - map_offset, bound(len) - offset);

#ifdef PAGE_UPDATE_DEBUG
    page_debug("level %d, old curr 0x%lx, phys 0x%lx, new curr 0x%lx, entry 0x%lx, *entry 0x%lx, flags 0x%lx\n",
               level, curr, phys, new_curr, entry, *entry, flags);
#endif

    /* transpose mapped page */
    assert(map_level(pointer_from_pteaddr(bound(root)), PT_FIRST_LEVEL,
                     irangel(new_curr, remap_len),
                     &phys, flags, 0));

    if (remap_len == map_len) {
        /* reset old entry */
        *entry = 0;
    } else {
        range unmapped = range_rshift(irangel(map_offset, remap_len), PAGELOG);
        if (pte_split(entry, unmapped) == INVALID_PHYSICAL)
            return false;
    }

    /* invalidate old mapping */
    page_invalidate(bound(fe), curr);

    return true;
}

/* We're just going to do forward traversal, for we don't yet need to
   support overlapping moves. Should the latter become necessary
   (e.g. to support MREMAP_FIXED in mremap(2) without depending on
   MREMAP_MAYMOVE), write a "traverse_ptes_reverse" to walk pages
   from high address to low (like memcpy).
*/
static void remap_pages_root(u64 root, u64 vaddr_new, u64 vaddr_old, u64 length)
{
    page_debug("vaddr_new 0x%lx, vaddr_old 0x%lx, length 0x%lx\n", vaddr_new, vaddr_old, length);
    if (vaddr_new == vaddr_old)
        return;
    assert(range_empty(range_intersection(irange(vaddr_new, vaddr_new + length),
                                          irange(vaddr_old, vaddr_old + length))));
    flush_entry fe = get_page_flush_entry();
    traverse_ptes_root(root, vaddr_old, length,
                       stack_closure(remap_entry, root, vaddr_new, vaddr_old, length, fe));
    page_invalidate_sync(fe, 0, false);
#ifdef PAGE_DUMP_ALL
    early_debug("remap ");
    dump_page_tables(vaddr_new, length);
#endif
}

void remap_pages(u64 vaddr_new, u64 vaddr_old, u64 length)
{
    assert_kernel_range(vaddr_new, length);
    assert_kernel_range(vaddr_old, length);
    remap_pages_root(get_pagetable_base(vaddr_old), vaddr_new, vaddr_old, length);
}

/* called with lock held */
closure_func_basic(entry_handler, boolean, zero_page,
                   int level, u64 addr, pteptr entry)
{
    u64 e = pte_from_pteptr(entry);
    if (pte_is_present(e) && pte_is_mapping(level, e)) {
        u64 size = pte_map_size(level, e);
#ifdef PAGE_UPDATE_DEBUG
        page_debug("addr 0x%lx, size 0x%lx\n", addr, size);
#endif
        zero(pointer_from_u64(addr), size);
    }
    return true;
}

void zero_mapped_pages(u64 vaddr, u64 length)
{
    assert_kernel_range(vaddr, length);
    traverse_ptes(vaddr, length, stack_closure_func(entry_handler, zero_page));
}

/* called with lock held */
closure_function(4, 3, boolean, unmap_page,
                 u64, vstart, u64, len, buffer, phys_ranges, flush_entry, fe,
                 int level, u64 vaddr, pteptr entry)
{
    u64 old_entry = pte_from_pteptr(entry);
    if (pte_is_present(old_entry) && pte_is_mapping(level, old_entry)) {
#ifdef PAGE_UPDATE_DEBUG
        page_debug("rh %p, level %d, vaddr 0x%lx, entry %p, *entry 0x%lx\n",
                   rh, level, vaddr, entry, *entry);
#endif
        u64 map_len = pte_map_size(level, old_entry);
        u64 vstart = bound(vstart);
        u64 map_offset, offset;
        if (vaddr >= vstart) {
            map_offset = 0;
            offset = vaddr - vstart;
        } else {
            map_offset = vstart - vaddr;
            offset = 0;
        }
        u64 unmap_len = MIN(map_len - map_offset, bound(len) - offset);
        if (unmap_len == map_len) {
            *entry = 0;
        } else {
            range unmapped = range_rshift(irangel(map_offset, unmap_len), PAGELOG);
            if (pte_split(entry, unmapped) == INVALID_PHYSICAL)
                return false;
        }
        page_invalidate(bound(fe), vaddr);
        buffer phys_ranges = bound(phys_ranges);
        if (phys_ranges) {
            range r = irangel(page_from_pte(old_entry) + map_offset, unmap_len);
            return buffer_write(phys_ranges, &r, sizeof(r));
        }
    }
    return true;
}

static void unmap_root(u64 root, u64 virtual, u64 length)
{
    assert(!((virtual & PAGEMASK) || (length & PAGEMASK)));
    flush_entry fe = get_page_flush_entry();
    traverse_ptes_root(root, virtual, length, stack_closure(unmap_page, virtual, length, 0, fe));
    page_invalidate_sync(fe, 0, false);
#ifdef PAGE_DUMP_ALL
    early_debug("unmap ");
    dump_page_tables(virtual, length);
#endif
}

void unmap(u64 virtual, u64 length)
{
    assert_kernel_range(virtual, length);
    unmap_root(get_pagetable_base(virtual), virtual, length);
}

#define next_addr(a, mask) (a = (a + (mask) + 1) & ~(mask))
#define INDEX_MASK (PAGEMASK >> 3)
/* The virtual address range is remapped, i.e. any existing mapping is overwritten (and the
 * overwritten pages are added to the flush entry to be subsequently invalidated. */
static boolean map_level(u64 *table_ptr, int level, range v, u64 *p, u64 flags, flush_entry fe)
{
    int shift = pt_level_shift(level);
    u64 mask = MASK(shift);
    u64 vmask = v.start & ~MASK(VIRTUAL_ADDRESS_BITS);
    v.start &= MASK(VIRTUAL_ADDRESS_BITS);
    v.end &= MASK(VIRTUAL_ADDRESS_BITS);
    // XXX this was level > 2, but that didn't seem right - validate me
    u64 vlbase = level > PT_FIRST_LEVEL ? v.start & ~MASK(pt_level_shift(level - 1)) : 0;
    int first_index = (v.start >> shift) & INDEX_MASK;
    int last_index = ((v.end - 1) >> shift) & INDEX_MASK;

    page_init_debug("\nmap_level: table_ptr ");
    page_init_debug_u64(u64_from_pointer(table_ptr));
    page_init_debug(", level ");
    page_init_debug_u64(level);
    page_init_debug("\n   v ");
    page_init_debug_u64(v.start);
    page_init_debug(" - ");
    page_init_debug_u64(v.end);
    page_init_debug(", p ");
    page_init_debug_u64(*p);
    page_init_debug(" first ");
    page_init_debug_u64(first_index);
    page_init_debug(" last ");
    page_init_debug_u64(last_index);
    page_init_debug("\n");
    assert(first_index <= last_index);
    assert(table_ptr && table_ptr != INVALID_ADDRESS);

    for (int i = first_index; i <= last_index; i++, next_addr(v.start, mask)) {
        page_init_debug("   index ");
        page_init_debug_u64(i);
        page_init_debug(", v.start ");
        page_init_debug_u64(v.start);
        page_init_debug(", p ");
        page_init_debug_u64(*p);
        u64 pte = table_ptr[i];
        page_init_debug(", pte ");
        page_init_debug_u64(pte);
        page_init_debug("\n");
        if (!pte_is_present(pte)) {
            if ((level == PT_FIRST_LEVEL) && (i >= PT_ROOT_KERNEL_INDEX) &&
                pagemem.kernel_root_sealed)
                halt("%s: kernel half root entry %d missing after address spaces exist\n",
                     func_ss, i);
            if (level == PT_PTE_LEVEL) {
                pte = page_pte(*p, flags);
                next_addr(*p, mask);
            } else if (!flags_has_minpage(flags) && level > PT_FIRST_LEVEL &&
                       (pagemem.levelmask & U64_FROM_BIT(level)) && (v.start & mask) == 0 &&
                       (*p & mask) == 0 && range_span(v) >= U64_FROM_BIT(shift)) {
                pte = block_pte(*p, flags);
                next_addr(*p, mask);
            } else {
                page_init_debug("      new level: ");
                void *tp;
                u64 tp_phys;
                if ((tp = allocate_table_page(&tp_phys)) == INVALID_ADDRESS) {
                    return false;
                }
                /* user and writable are AND of flags from all levels */
                pte = new_level_pte(tp_phys);
                u64 end = vlbase | (((u64)(i + 1)) << shift);
                /* length instead of end to avoid overflow at end of space */
                u64 len = MIN(range_span(v), end - v.start);
                page_init_debug("  end ");
                page_init_debug_u64(end);
                page_init_debug(", len ");
                page_init_debug_u64(len);
                page_init_debug("\n");
                if (!map_level(tp, level + 1, irangel(v.start | vmask, len), p, flags, fe))
                    return false;
            }
            page_init_debug("      pte @ ");
            page_init_debug_u64(u64_from_pointer(&table_ptr[i]));
            page_init_debug(" = ");
            page_init_debug_u64(pte);
            page_init_debug("\n");
            table_ptr[i] = pte;
        } else {
            /* Check if the page or block is already installed. */
            u64 end = vlbase | (((u64)(i + 1)) << shift);
            u64 len = MIN(range_span(v), end - v.start);
            if (pte_is_mapping(level, pte)) {
                page_debug("would overwrite entry: level %d, v %R, pa 0x%lx, "
                           "flags 0x%lx, index %d, entry 0x%lx\n", level, v, *p, flags, i, pte);
                if (pte_map_size(level, pte) <= len) {
                    /* overwrite existing mapping */
                    table_ptr[i] = (level == PT_PTE_LEVEL) ?
                                   page_pte(*p, flags) : block_pte(*p, flags);
                    page_invalidate(fe, v.start | vmask);
                    next_addr(*p, mask);
                    continue;
                }
                int next_level_shift = pt_level_shift(level + 1);
                pte = pte_split(table_ptr + i, irangel((v.start >> next_level_shift) & INDEX_MASK,
                                                       len >> next_level_shift));
                if (pte == INVALID_PHYSICAL)
                    return false;
            }
            u64 nexttable = page_from_pte(pte);
            u64 *nexttable_ptr = pointer_from_pteaddr(nexttable);
            if (!map_level(nexttable_ptr, level + 1, irangel(v.start | vmask, len), p, flags, fe))
                return false;
        }
    }
    return true;
}

static void map_root(u64 root, u64 v, physical p, u64 length, pageflags flags)
{
    page_init_debug("map: v ");
    page_init_debug_u64(v);
    page_init_debug(", p ");
    page_init_debug_u64(p);
    page_init_debug(", length ");
    page_init_debug_u64(length);
    page_init_debug(", flags ");
    page_init_debug_u64(flags.w);
    page_init_debug("\n   called from ");
    page_init_debug_u64(u64_from_pointer(__builtin_return_address(0)));
    page_init_debug("\n");

    assert((v & PAGEMASK) == 0);
    assert((p & PAGEMASK) == 0);
    range r = irangel(v, pad(length, PAGESIZE));
    flush_entry fe = get_page_flush_entry();
    pagetable_lock();
    u64 *table_ptr = pointer_from_pteaddr(root);
    if (!map_level(table_ptr, PT_FIRST_LEVEL, r, &p, flags.w, fe)) {
        pagetable_unlock();
        print_frame_trace_from_here();
        halt("map failed for v 0x%lx, p 0x%lx, len 0x%lx, flags 0x%lx\n",
             v, p, length, flags.w);
    }
    page_init_debug("map_level done\n");
    pagetable_unlock();
    page_invalidate_sync(fe, 0, false);
#ifdef PAGE_DUMP_ALL
    early_debug("map ");
    dump_page_tables(v, length);
#endif
}

void map(u64 v, physical p, u64 length, pageflags flags)
{
    assert_kernel_range(v, length);
    map_root(get_pagetable_base(v), v, p, length, flags);
}

/* Set up a mapping, like the map() function but without acquiring the page table lock; this
 * function is meant to be called by init code, when there is only one CPU running. */
static void map_root_nolock(u64 root, u64 v, physical p, u64 length, pageflags flags)
{
    range r = irangel(v, pad(length, PAGESIZE));
    u64 *table_ptr = pointer_from_pteaddr(root);
    map_level(table_ptr, PT_FIRST_LEVEL, r, &p, flags.w, 0);
}

void map_nolock(u64 v, physical p, u64 length, pageflags flags)
{
    map_root_nolock(get_pagetable_base(v), v, p, length, flags);
}

#ifdef KERNEL
closure_function(2, 0, void, unmap_and_free_phys_complete,
                 buffer, phys_ranges, boolean, on_stack)
{
    heap h = heap_physical(get_kernel_heaps());
    buffer phys_ranges = bound(phys_ranges);
    range *r;
    while ((r = buffer_pop(phys_ranges, sizeof(*r))))
        free_unshared_phys(h, *r);
    if (bound(on_stack)) {
        /* clear the buffer so it can be reused if there are other iterations */
        buffer_clear(phys_ranges);
    } else {
        deallocate_buffer(phys_ranges);
        closure_finish();
    }
}

static void unmap_and_free_phys_sync(u64 root, u64 virtual, u64 length)
{
    buffer phys_ranges = little_stack_buffer(context_stack_space() / 2);
    thunk completion = stack_closure(unmap_and_free_phys_complete, phys_ranges, true);
    boolean done, progress;
    do {
        flush_entry fe = get_page_flush_entry();
        done = traverse_ptes_root(root, virtual, length,
                                  stack_closure(unmap_page, virtual, length, phys_ranges, fe));
        progress = buffer_length(phys_ranges) != 0;
        page_invalidate_sync(fe, completion, true);
    } while (!done && progress);
}

static void unmap_and_free_phys_root(u64 root, u64 virtual, u64 length)
{
    heap h = heap_locked(get_kernel_heaps());
    buffer phys_ranges = allocate_buffer(h, 64 * sizeof(range));
    if (phys_ranges == INVALID_ADDRESS)
        return unmap_and_free_phys_sync(root, virtual, length);
    thunk completion = closure(h, unmap_and_free_phys_complete, phys_ranges, false);
    if (completion == INVALID_ADDRESS) {
        deallocate_buffer(phys_ranges);
        return unmap_and_free_phys_sync(root, virtual, length);
    }
    flush_entry fe = get_page_flush_entry();
    boolean success = traverse_ptes_root(root, virtual, length,
                                         stack_closure(unmap_page, virtual, length, phys_ranges, fe));
    page_invalidate_sync(fe, completion, !success);
    if (!success)
        unmap_and_free_phys_sync(root, virtual, length);
}

void unmap_and_free_phys(u64 virtual, u64 length)
{
    assert_kernel_range(virtual, length);
    unmap_and_free_phys_root(get_pagetable_base(virtual), virtual, length);
}

void page_free_phys(u64 phys)
{
    if (!page_unshare(phys))
        return;
    u64 virt = pagemem.pagevirt.start + phys;
    deallocate_u64((heap)get_kernel_heaps()->pages, virt, PAGESIZE);
}
#endif

static boolean init_page_map(range phys, range *curr_virt, id_heap virt_heap, pageflags flags)
{
    if (phys.end > range_span(*curr_virt)) {
        u64 alloc = pad(phys.end - range_span(*curr_virt), virt_heap->h.pagesize);
        if (!id_heap_set_area(virt_heap, curr_virt->end, alloc, true, true))
            return false;
        curr_virt->end += alloc;
    }
    map(curr_virt->start + phys.start, phys.start, range_span(phys), flags);
    return true;
}

closure_function(6, 1, boolean, init_page_map_all_rh,
                 range *, curr_virt, id_heap, virt_heap, u64, margin, pageflags, flags, u64, last_end, range *, init_pages,
                 range r)
{
    u64 margin = bound(margin);
    range phys = irange(r.start & ~(margin - 1), pad(r.end, margin));
    if (phys.start < bound(last_end)) {
        phys.start = bound(last_end);
        if (phys.start == phys.end)
            return true;
    }
    range *curr_virt = bound(curr_virt);
    id_heap virt_heap = bound(virt_heap);
    pageflags flags = bound(flags);
    if (!init_page_map(phys, curr_virt, virt_heap, flags))
        return false;
    bound(last_end) = phys.end;
    range *init_pages = bound(init_pages);
    range i = range_intersection(phys, *init_pages);
    if (range_span(i)) {
        if ((i.start > init_pages->start) &&
            !init_page_map(irange(init_pages->start, i.start), curr_virt, virt_heap, flags))
            return false;
        init_pages->start = i.end;
    }
    return true;
}

range init_page_map_all(heap phys, id_heap virt_heap)
{
    u64 initial_alloc = virt_heap->h.pagesize;
    range pagevirt = irangel(allocate_u64((heap)virt_heap, initial_alloc), initial_alloc);
    u64 margin = GB;
    pageflags flags = pageflags_kernel_data();

    /* Map all memory in the physical heap, plus the initial pages (which may not be accounted for
     * in the physical heap). */
    range init_pages = irange(pagemem.physbase & ~(margin - 1),
                              pad(pagemem.current_phys.end, margin));
    pageheap_range_foreach(stack_closure(init_page_map_all_rh, &pagevirt, virt_heap, margin,
                                              flags, 0, &init_pages));
    if (range_span(init_pages))
        assert(init_page_map(init_pages, &pagevirt, virt_heap, flags));

    pagemem.current_phys = irange(0, 0);
    pagemem.pageheap = (heap)phys;
    pagemem.pagevirt = pagevirt;
    pagemem.physbase = 0;
    return pagevirt;
}

/* pageheap must be a physical memory heap. */
void init_page_tables(heap pageheap, range pagevirt)
{
    page_init_debug("init_page_tables: pageheap ");
    page_init_debug_u64(u64_from_pointer(pageheap));
    page_init_debug("\n");
    pagemem.current_phys = irange(0, 0);
    pagemem.pageheap = pageheap;
    pagemem.pagevirt = pagevirt;
    pagemem.physbase = 0;
}

void page_set_allowed_levels(u64 levelmask)
{
    pagemem.levelmask = levelmask;
}

#ifdef KERNEL
/* Use a fixed area for page table allocation, either before MMU init or with
   only initial mappings set up. */
void init_page_initial_map(void *initial_map, range phys)
{
    page_init_debug("init_page_initial_map: initial_map ");
    page_init_debug_u64(u64_from_pointer(initial_map));
    page_init_debug(", phys ");
    page_init_debug_u64(phys.start);
    page_init_debug(", length ");
    page_init_debug_u64(range_span(phys));
    page_init_debug("\n");
    spin_lock_init(&pt_lock);
    pagemem.current_phys = phys;
    pagemem.pageheap = 0;
    pagemem.pagevirt = irangel(u64_from_pointer(initial_map), range_span(phys));
    pagemem.physbase = phys.start;
}
#endif

#ifdef KERNEL
static void free_table_page_locked(u64 phys)
{
    u64 *p = pointer_from_pteaddr(phys);
    p[0] = pagemem.free_tables;
    pagemem.free_tables = phys;
}

static void free_table_level_locked(u64 table, int level)
{
    if (level < PT_PTE_LEVEL) {
        u64 *t = pointer_from_pteaddr(table);
        for (int i = 0; i < PTE_ENTRIES; i++) {
            pte e = t[i];
            if (pte_is_present(e) && !pte_is_mapping(level, e))
                free_table_level_locked(page_from_pte(e), level + 1);
        }
    }
    free_table_page_locked(table);
}

BSS_RO_AFTER_INIT address_space kernel_address_space;

closure_func_basic(thunk, void, address_space_free)
{
    address_space as = struct_from_closure(address_space, free);
    assert(bitmap_range_get_first(as->cpus, 0, total_processors) == INVALID_PHYSICAL);
    arch_asid_release(as->asid);
    pagetable_lock();
    u64 *root = pointer_from_pteaddr(as->root);
    for (int i = 0; i < PT_ROOT_KERNEL_INDEX; i++) {
        pte e = root[i];
        if (pte_is_present(e) && !pte_is_mapping(PT_FIRST_LEVEL, e))
            free_table_level_locked(page_from_pte(e), PT_FIRST_LEVEL + 1);
    }
    free_table_page_locked(as->root);
    pagetable_unlock();
    deallocate_bitmap(as->cpus);
    deallocate(as->h, as, sizeof(*as));
}

void init_address_spaces(heap h)
{
    static struct address_space kas;
    kas.root = get_pagetable_base(0);
    kas.h = 0;
    kas.asid = arch_asid_allocate();
    pagetable_lock();
    u64 *root = pointer_from_pteaddr(kas.root);
    for (int i = PT_ROOT_KERNEL_INDEX; i < PTE_ENTRIES; i++) {
        if (pte_is_present(root[i]))
            continue;
        u64 phys;
        assert(allocate_table_page(&phys) != INVALID_ADDRESS);
        root[i] = new_level_pte(phys);
    }
    pagemem.kernel_root_sealed = true;
    pagetable_unlock();
    kernel_address_space = &kas;
    cpuinfo ci;
    vector_foreach(cpuinfos, ci)
        ci->current_as = kernel_address_space;
}

address_space allocate_address_space(heap h)
{
    address_space as = allocate(h, sizeof(*as));
    if (as == INVALID_ADDRESS)
        return as;
    as->cpus = allocate_bitmap(h, h, total_processors);
    if (as->cpus == INVALID_ADDRESS) {
        deallocate(h, as, sizeof(*as));
        return INVALID_ADDRESS;
    }
    pagetable_lock();
    u64 *root = allocate_table_page(&as->root);
    if (root != INVALID_ADDRESS) {
        u64 *kroot = pointer_from_pteaddr(kernel_address_space->root);
        for (int i = PT_ROOT_KERNEL_INDEX; i < PTE_ENTRIES; i++)
            root[i] = kroot[i];
    }
    pagetable_unlock();
    if (root == INVALID_ADDRESS) {
        deallocate_bitmap(as->cpus);
        deallocate(h, as, sizeof(*as));
        return INVALID_ADDRESS;
    }
    as->h = h;
    as->asid = arch_asid_allocate();
    init_refcount(&as->refcount, 1, init_closure_func(&as->free, thunk, address_space_free));
    return as;
}

void address_space_switch(address_space as)
{
    if (!kernel_address_space)
        return;
    u64 flags = irq_disable_save();
    cpuinfo ci = current_cpu();
    address_space prev = ci->current_as;
    if (prev != as) {
        if (as != kernel_address_space) {
            address_space_reserve(as);
            bitmap_set_atomic(as->cpus, ci->id, 1);
        }
        ci->current_as = as;
        if (as == kernel_address_space)
            flush_profile_add(kernel_loads, 1);
        else
            flush_profile_add(user_loads, 1);
        arch_load_address_space(as->root, as->asid);
        if (prev != kernel_address_space) {
            bitmap_set_atomic(prev->cpus, ci->id, 0);
            address_space_release(prev);
        }
    }
    irq_restore(flags);
}

address_space use_address_space(address_space as)
{
    context ctx = get_current_context(current_cpu());
    address_space prev = ctx->as;
    ctx->as = as;
    if (as)
        address_space_switch(as);
    return prev;
}

void restore_address_space(address_space prev)
{
    context ctx = get_current_context(current_cpu());
    ctx->as = prev;
    if (prev)
        address_space_switch(prev);
}

#define assert_user_range(v, len) do {                                              \
        if (!((v) < USER_LIMIT && (len) <= USER_LIMIT - (v)))                       \
            halt("%s: address 0x%lx (length 0x%lx) is not a user address\n",       \
                 func_ss, (u64)(v), (u64)(len));                                    \
    } while (0)

void address_space_map(address_space as, u64 v, physical p, u64 length, pageflags flags)
{
    assert_user_range(v, length);
    map_root(as->root, v, p, length, flags);
}

void address_space_map_locked(address_space as, u64 v, physical p, u64 length, pageflags flags)
{
    assert_user_range(v, length);
    map_root_nolock(as->root, v, p, length, flags);
}

void address_space_unmap(address_space as, u64 v, u64 length)
{
    assert_user_range(v, length);
    unmap_root(as->root, v, length);
}

void address_space_unmap_and_free_phys(address_space as, u64 v, u64 length)
{
    assert_user_range(v, length);
    unmap_and_free_phys_root(as->root, v, length);
}

void address_space_update_map_flags(address_space as, u64 v, u64 length, pageflags flags)
{
    assert_user_range(v, length);
    update_map_flags_root(as->root, v, length, flags);
}

void address_space_remap_pages(address_space as, u64 vnew, u64 vold, u64 length)
{
    assert_user_range(vnew, length);
    assert_user_range(vold, length);
    remap_pages_root(as->root, vnew, vold, length);
}

boolean address_space_traverse_ptes(address_space as, u64 v, u64 length, entry_handler eh)
{
    assert_user_range(v, length);
    return traverse_ptes_root(as->root, v, length, eh);
}

closure_func_basic(thunk, void, address_space_sync_done)
{
}

closure_function(4, 3, boolean, protect_pte,
                 u64, vstart, u64, len, flush_entry, fe, boolean *, oom,
                 int level, u64 addr, pteptr entry)
{
    pte e = pte_from_pteptr(entry);
    if (!pte_is_present(e) || !pte_is_mapping(level, e))
        return true;
    if (level != PT_PTE_LEVEL) {
        if (!split_mapping(entry)) {
            *bound(oom) = true;
            return false;
        }
        return true;
    }
    pageflags flags = {.w = flags_from_pte(e)};
    if (!pageflags_is_writable(flags))
        return true;
    pte_set(entry, page_pte(page_from_pte(e), pageflags_readonly(flags).w));
    page_invalidate(bound(fe), addr);
    return true;
}

boolean address_space_protect(address_space as, u64 v, u64 length)
{
    assert_user_range(v, length);
    boolean oom = false;
    flush_entry fe = get_page_flush_entry();
    traverse_ptes_root(as->root, v, length, stack_closure(protect_pte, v, length, fe, &oom));
    page_invalidate_sync(fe, stack_closure_func(thunk, address_space_sync_done), true);
    return !oom;
}

void address_space_replace_page(address_space as, u64 v, physical p, pageflags flags)
{
    assert_user_range(v, PAGESIZE);
    flush_entry fe = get_page_flush_entry();
    pagetable_lock();
    u64 phys = p;
    boolean ok = map_level(pointer_from_pteaddr(as->root), PT_FIRST_LEVEL, irangel(v, PAGESIZE),
                           &phys, flags.w, fe);
    pagetable_unlock();
    if (!ok)
        halt("%s: map failed for v 0x%lx, p 0x%lx\n", func_ss, v, p);
    page_invalidate_sync(fe, stack_closure_func(thunk, address_space_sync_done), true);
}

physical address_space_physical_locked(address_space as, u64 v)
{
    assert_user_range(v, 0);
    u64 table = as->root;
    for (int level = PT_FIRST_LEVEL; level <= PT_PTE_LEVEL; level++) {
        int shift = pt_level_shift(level);
        pte e = *pointer_from_pteaddr(table + (((v >> shift) & MASK(9)) * sizeof(u64)));
        if (!pte_is_present(e))
            return INVALID_PHYSICAL;
        if (pte_is_mapping(level, e))
            return page_from_pte(e) | (v & MASK(shift));
        table = page_from_pte(e);
    }
    return INVALID_PHYSICAL;
}

physical address_space_physical(address_space as, u64 v)
{
    pagetable_lock();
    physical p = address_space_physical_locked(as, v);
    pagetable_unlock();
    return p;
}
#endif
