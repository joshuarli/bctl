#ifdef KERNEL
extern struct spinlock pt_lock;
#define pagetable_lock() u64 _savedflags = spin_lock_irq(&pt_lock)
#define pagetable_unlock() spin_unlock_irq(&pt_lock, _savedflags)
#else
#define pagetable_lock()
#define pagetable_unlock()
#endif

/* Though page flags are just a u64, we hide it behind this type to
   emphasize that page flags should be composed using helpers with
   clear semantics, not architecture bits. This is to avoid mistakes
   due to a union of PAGE_* constants on one architecture meaning
   something entirely different on another. */

typedef struct pageflags {
    u64 w;                      /* _PAGE_* flags, keep private to page.[hc] */
} pageflags;

void init_page_initial_map(void *initial_map, range phys);
range init_page_map_all(heap phys, id_heap virt_heap);
void init_page_tables(heap pageheap, range pagevirt);

/* tlb shootdown */
void init_flush(heap);
flush_entry get_page_flush_entry();
void page_invalidate(flush_entry f, u64 address);
void page_invalidate_sync(flush_entry f, thunk completion, boolean rendezvous);
void page_invalidate_flush();

void invalidate(u64 page);
void flush_tlb(boolean full_flush);

/* mapping and flag update */
/* overwrite any existing mappings in the virtual address range */
void map(u64 v, physical p, u64 length, pageflags flags);
void map_nolock(u64 v, physical p, u64 length, pageflags flags);

void update_map_flags(u64 vaddr, u64 length, pageflags flags);

#define remap(v, p, length, flags)  map(v, p, length, flags)

void zero_mapped_pages(u64 vaddr, u64 length);
void remap_pages(u64 vaddr_new, u64 vaddr_old, u64 length);
void unmap(u64 virtual, u64 length);

#define unmap_pages(virtual, length)    unmap(virtual, length)

#include <page_machine.h>

static inline pageflags pageflags_kernel_data(void)
{
    return pageflags_writable(pageflags_memory());
}

/* table traversal */
closure_type(entry_handler, boolean /* success */, int level, u64 vaddr, pteptr entry);
boolean traverse_ptes(u64 vaddr, u64 length, entry_handler eh);
boolean split_mapping(pteptr entry);
void dump_page_tables(u64 vaddr, u64 length);

/* internal use */
void *allocate_table_page(u64 *phys);
void page_set_allowed_levels(u64 levelmask);

typedef struct address_space *address_space;

#ifdef KERNEL
struct address_space {
    u64 root;
    struct refcount refcount;
    bitmap cpus;
    heap h;
    closure_struct(thunk, free);
};

extern address_space kernel_address_space;

void init_address_spaces(heap h);
address_space allocate_address_space(heap h);
void address_space_switch(address_space as);
address_space use_address_space(address_space as);
void restore_address_space(address_space prev);

static inline void address_space_reserve(address_space as)
{
    refcount_reserve(&as->refcount);
}

static inline void address_space_release(address_space as)
{
    refcount_release(&as->refcount);
}

void address_space_map(address_space as, u64 v, physical p, u64 length, pageflags flags);
void address_space_unmap(address_space as, u64 v, u64 length);
void address_space_unmap_and_free_phys(address_space as, u64 v, u64 length);
void address_space_update_map_flags(address_space as, u64 v, u64 length, pageflags flags);
void address_space_remap_pages(address_space as, u64 vnew, u64 vold, u64 length);
boolean address_space_traverse_ptes(address_space as, u64 v, u64 length, entry_handler eh);
physical address_space_physical(address_space as, u64 v);
physical address_space_physical_locked(address_space as, u64 v);
void address_space_map_locked(address_space as, u64 v, physical p, u64 length, pageflags flags);
void arch_load_address_space(u64 root);

boolean address_space_protect(address_space as, u64 v, u64 length);
void address_space_replace_page(address_space as, u64 v, physical p, pageflags flags);

void init_shared_pages(heap h);
boolean page_share(physical phys);
boolean page_is_shared(physical phys);
boolean page_unshare(physical phys);
#endif
