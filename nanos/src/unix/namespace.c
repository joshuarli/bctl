#include <unix_internal.h>

static struct user_namespace init_ns;
static struct net_namespace initial_net_ns;
static u32 init_inums[NAMESPACE_KIND_COUNT];
static struct spinlock inum_lock;
static u32 next_inum;

static u32 namespace_alloc_inum(void)
{
    spin_lock(&inum_lock);
    u32 inum = next_inum++;
    spin_unlock(&inum_lock);
    return inum;
}

closure_func_basic(thunk, void, user_ns_free)
{
    user_namespace ns = struct_from_closure(user_namespace, free);
    if (ns->parent)
        user_ns_release(ns->parent);
    deallocate(heap_locked(get_kernel_heaps()), ns, sizeof(struct user_namespace));
}

void user_ns_reserve(user_namespace ns)
{
    refcount_reserve(&ns->refcount);
}

void user_ns_release(user_namespace ns)
{
    refcount_release(&ns->refcount);
}

user_namespace user_ns_init(void)
{
    return &init_ns;
}

static user_namespace user_ns_alloc(user_namespace parent, u32 owner, u32 group)
{
    heap h = heap_locked(get_kernel_heaps());
    user_namespace ns = allocate(h, sizeof(struct user_namespace));
    if (ns == INVALID_ADDRESS)
        return ns;
    zero(ns, sizeof(*ns));
    spin_lock_init(&ns->lock);
    init_refcount(&ns->refcount, 1, init_closure_func(&ns->free, thunk, user_ns_free));
    if (parent) {
        user_ns_reserve(parent);
        ns->parent = parent;
        ns->level = parent->level + 1;
    }
    ns->owner = owner;
    ns->group = group;
    ns->inum = namespace_alloc_inum();
    ns->setgroups_allowed = parent ? parent->setgroups_allowed : true;
    return ns;
}

user_namespace user_ns_create(user_namespace parent, u32 owner, u32 group)
{
    if (parent && parent->level >= USER_NS_LEVEL_MAX)
        return INVALID_ADDRESS;
    return user_ns_alloc(parent, owner, group);
}

void namespace_init(void)
{
    spin_lock_init(&inum_lock);
    next_inum = 0xf0000000;

    zero(&init_ns, sizeof(init_ns));
    spin_lock_init(&init_ns.lock);
    init_refcount(&init_ns.refcount, 1, 0);
    init_ns.level = 0;
    init_ns.owner = 0;
    init_ns.group = 0;
    init_ns.inum = namespace_alloc_inum();
    init_ns.uid_map.nr = 1;
    init_ns.uid_map.written = true;
    init_ns.uid_map.e[0] = (struct id_extent){ 0, 0, 0xffffffff };
    init_ns.gid_map.nr = 1;
    init_ns.gid_map.written = true;
    init_ns.gid_map.e[0] = (struct id_extent){ 0, 0, 0xffffffff };
    init_ns.setgroups_allowed = true;

    init_inums[NAMESPACE_USER] = init_ns.inum;
    zero(&initial_net_ns, sizeof(initial_net_ns));
    init_refcount(&initial_net_ns.refcount, 1, 0);
    initial_net_ns.user_ns = &init_ns;
    initial_net_ns.inum = namespace_alloc_inum();
    init_inums[NAMESPACE_NET] = initial_net_ns.inum;
    for (int kind = NAMESPACE_PID; kind < NAMESPACE_KIND_COUNT; kind++) {
        if (kind != NAMESPACE_NET)
            init_inums[kind] = namespace_alloc_inum();
    }
}

static struct id_map *user_ns_id_map(user_namespace ns, boolean group)
{
    return group ? &ns->gid_map : &ns->uid_map;
}

static boolean kernel_to_ns(user_namespace ns, boolean group, u32 kid, u32 *local)
{
    if (!ns->parent) {
        *local = kid;
        return true;
    }
    u32 parent_local;
    if (!kernel_to_ns(ns->parent, group, kid, &parent_local))
        return false;
    struct id_map *m = user_ns_id_map(ns, group);
    boolean found = false;
    spin_lock(&ns->lock);
    for (u32 i = 0; i < m->nr; i++) {
        struct id_extent *e = &m->e[i];
        if ((u64)parent_local >= e->lower_first &&
            (u64)parent_local < (u64)e->lower_first + e->count) {
            *local = e->first + (parent_local - e->lower_first);
            found = true;
            break;
        }
    }
    spin_unlock(&ns->lock);
    return found;
}

static boolean ns_to_kernel(user_namespace ns, boolean group, u32 local, u32 *kid)
{
    if (!ns->parent) {
        *kid = local;
        return true;
    }
    struct id_map *m = user_ns_id_map(ns, group);
    u32 parent_local;
    boolean found = false;
    spin_lock(&ns->lock);
    for (u32 i = 0; i < m->nr; i++) {
        struct id_extent *e = &m->e[i];
        if ((u64)local >= e->first && (u64)local < (u64)e->first + e->count) {
            parent_local = e->lower_first + (local - e->first);
            found = true;
            break;
        }
    }
    spin_unlock(&ns->lock);
    if (!found)
        return false;
    return ns_to_kernel(ns->parent, group, parent_local, kid);
}

u32 user_ns_from_kuid(user_namespace ns, u32 kid)
{
    u32 local;
    return kernel_to_ns(ns, false, kid, &local) ? local : USER_NS_OVERFLOW_ID;
}

boolean user_ns_to_kuid(user_namespace ns, u32 local, u32 *kid)
{
    return ns_to_kernel(ns, false, local, kid);
}

u32 user_ns_from_kgid(user_namespace ns, u32 kgid)
{
    u32 local;
    return kernel_to_ns(ns, true, kgid, &local) ? local : USER_NS_OVERFLOW_ID;
}

boolean user_ns_to_kgid(user_namespace ns, u32 local, u32 *kgid)
{
    return ns_to_kernel(ns, true, local, kgid);
}

boolean user_ns_setgroups_ok(user_namespace ns)
{
    if (!ns->parent)
        return true;
    spin_lock(&ns->lock);
    boolean ok = ns->setgroups_allowed && ns->gid_map.written;
    spin_unlock(&ns->lock);
    return ok;
}

u32 namespace_inum(process p, enum namespace_kind kind)
{
    u32 inum;
    switch (kind) {
    case NAMESPACE_USER:
        process_lock(p);
        inum = p->creds.user_ns->inum;
        process_unlock(p);
        return inum;
    case NAMESPACE_NET:
        process_lock(p);
        inum = p->net_ns->inum;
        process_unlock(p);
        return inum;
    default:
        return init_inums[kind];
    }
}

closure_func_basic(thunk, void, net_ns_free)
{
    net_namespace ns = struct_from_closure(net_namespace, free);
    user_ns_release(ns->user_ns);
    deallocate(heap_locked(get_kernel_heaps()), ns, sizeof(struct net_namespace));
}

net_namespace net_ns_init(void)
{
    return &initial_net_ns;
}

net_namespace net_ns_create(user_namespace owner)
{
    net_namespace ns = allocate(heap_locked(get_kernel_heaps()), sizeof(struct net_namespace));
    if (ns == INVALID_ADDRESS)
        return ns;
    zero(ns, sizeof(*ns));
    init_refcount(&ns->refcount, 1, init_closure_func(&ns->free, thunk, net_ns_free));
    user_ns_reserve(owner);
    ns->user_ns = owner;
    ns->inum = namespace_alloc_inum();
    return ns;
}

void net_ns_reserve(net_namespace ns)
{
    refcount_reserve(&ns->refcount);
}

void net_ns_release(net_namespace ns)
{
    refcount_release(&ns->refcount);
}

boolean process_in_initial_net_ns(process p)
{
    process_lock(p);
    boolean initial = p->net_ns == &initial_net_ns;
    process_unlock(p);
    return initial;
}

user_namespace process_user_ns(process p)
{
    process_lock(p);
    user_namespace ns = p->creds.user_ns;
    user_ns_reserve(ns);
    process_unlock(p);
    return ns;
}

const char *namespace_kind_name(enum namespace_kind kind)
{
    switch (kind) {
    case NAMESPACE_USER: return "user";
    case NAMESPACE_PID: return "pid";
    case NAMESPACE_NET: return "net";
    case NAMESPACE_IPC: return "ipc";
    case NAMESPACE_MNT: return "mnt";
    case NAMESPACE_UTS: return "uts";
    default: return "unknown";
    }
}

boolean ns_capable_creds(struct creds *c, user_namespace target, int cap)
{
    if (!target)
        return false;
    user_namespace ns = target;
    for (;;) {
        if (ns == c->user_ns)
            return !!(c->cap_effective & (1ull << cap));
        if (!ns->parent || ns->level <= c->user_ns->level)
            return false;
        if (ns->owner == c->euid)
            return true;
        ns = ns->parent;
    }
}

boolean ns_capable(process p, user_namespace target, int cap)
{
    process_lock(p);
    boolean r = ns_capable_creds(&p->creds, target, cap);
    process_unlock(p);
    return r;
}

boolean process_is_chrooted(process p)
{
    fs_struct fs = p->fs;
    filesystem root_fs = get_root_fs();
    boolean chrooted;
    spin_lock(&fs->lock);
    chrooted = (fs->root_fs != root_fs) ||
               (fs->root_tuple != filesystem_getroot(root_fs));
    spin_unlock(&fs->lock);
    return chrooted;
}

closure_func_basic(thunk, void, fs_struct_free)
{
    fs_struct fs = struct_from_closure(fs_struct, free);
    buffer b = fs->root_path;
    if (b)
        deallocate_buffer(b);
    filesystem_release(fs->root_fs);
    filesystem_release(fs->cwd_fs);
    deallocate(heap_locked(get_kernel_heaps()), fs, sizeof(struct fs_struct));
}

static fs_struct fs_struct_alloc(void)
{
    heap h = heap_locked(get_kernel_heaps());
    fs_struct fs = allocate(h, sizeof(struct fs_struct));
    if (fs == INVALID_ADDRESS)
        return fs;
    spin_lock_init(&fs->lock);
    init_refcount(&fs->refcount, 1, init_closure_func(&fs->free, thunk, fs_struct_free));
    fs->root_fs = 0;
    fs->cwd_fs = 0;
    fs->root_path = 0;
    return fs;
}

fs_struct fs_struct_create(filesystem root_fs, filesystem cwd_fs, tuple root, inode cwd)
{
    fs_struct fs = fs_struct_alloc();
    if (fs == INVALID_ADDRESS)
        return fs;
    buffer b = allocate_string(2);
    if (b == INVALID_ADDRESS) {
        deallocate(heap_locked(get_kernel_heaps()), fs, sizeof(struct fs_struct));
        return INVALID_ADDRESS;
    }
    push_u8(b, '/');
    filesystem_reserve(root_fs);
    filesystem_reserve(cwd_fs);
    fs->root_fs = root_fs;
    fs->cwd_fs = cwd_fs;
    fs->root = root_fs->get_inode(root_fs, root);
    fs->cwd = cwd;
    fs->root_tuple = root;
    fs->root_path = b;
    return fs;
}

fs_struct fs_struct_copy(fs_struct src)
{
    fs_struct fs = fs_struct_alloc();
    if (fs == INVALID_ADDRESS)
        return fs;
    heap h = heap_locked(get_kernel_heaps());
    spin_lock(&src->lock);
    filesystem_reserve(src->root_fs);
    filesystem_reserve(src->cwd_fs);
    fs->root_fs = src->root_fs;
    fs->cwd_fs = src->cwd_fs;
    fs->root = src->root;
    fs->cwd = src->cwd;
    fs->root_tuple = src->root_tuple;
    buffer b = clone_buffer(h, src->root_path);
    spin_unlock(&src->lock);
    if (b == INVALID_ADDRESS) {
        filesystem_release(fs->root_fs);
        filesystem_release(fs->cwd_fs);
        deallocate(h, fs, sizeof(struct fs_struct));
        return INVALID_ADDRESS;
    }
    fs->root_path = b;
    return fs;
}

void fs_struct_reserve(fs_struct fs)
{
    refcount_reserve(&fs->refcount);
}

void fs_struct_release(fs_struct fs)
{
    refcount_release(&fs->refcount);
}
