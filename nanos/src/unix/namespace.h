typedef struct user_namespace *user_namespace;
typedef struct fs_struct *fs_struct;
typedef struct net_namespace *net_namespace;
struct creds;

#define USER_NS_LEVEL_MAX       32
#define USER_NS_MAP_EXTENTS     5
#define USER_NS_OVERFLOW_ID     65534

#ifndef EUSERS
#define EUSERS                  87
#endif

struct id_extent {
    u32 first;
    u32 lower_first;
    u32 count;
};

struct id_map {
    u32 nr;
    boolean written;
    struct id_extent e[USER_NS_MAP_EXTENTS];
};

struct user_namespace {
    struct refcount refcount;
    closure_struct(thunk, free);
    struct user_namespace *parent;
    u32 level;
    u32 owner;
    u32 group;
    u32 inum;
    struct id_map uid_map;
    struct id_map gid_map;
    boolean setgroups_allowed;
    struct spinlock lock;
};

struct fs_struct {
    struct refcount refcount;
    closure_struct(thunk, free);
    struct spinlock lock;
    filesystem root_fs;
    filesystem cwd_fs;
    inode root;
    inode cwd;
    tuple root_tuple;
    buffer root_path;
};

struct net_namespace {
    struct refcount refcount;
    closure_struct(thunk, free);
    user_namespace user_ns;
    u32 inum;
};

enum namespace_kind {
    NAMESPACE_USER,
    NAMESPACE_PID,
    NAMESPACE_NET,
    NAMESPACE_IPC,
    NAMESPACE_MNT,
    NAMESPACE_UTS,
    NAMESPACE_KIND_COUNT,
};

void namespace_init(void);
user_namespace user_ns_init(void);
void user_ns_reserve(user_namespace ns);
void user_ns_release(user_namespace ns);
user_namespace user_ns_create(user_namespace parent, u32 owner, u32 group);
u32 user_ns_from_kuid(user_namespace ns, u32 kid);
boolean user_ns_to_kuid(user_namespace ns, u32 local, u32 *kid);
u32 user_ns_from_kgid(user_namespace ns, u32 kgid);
boolean user_ns_to_kgid(user_namespace ns, u32 local, u32 *kgid);
boolean user_ns_setgroups_ok(user_namespace ns);
u32 namespace_inum(process p, enum namespace_kind kind);
const char *namespace_kind_name(enum namespace_kind kind);
user_namespace process_user_ns(process p);

net_namespace net_ns_init(void);
net_namespace net_ns_create(user_namespace owner);
void net_ns_reserve(net_namespace ns);
void net_ns_release(net_namespace ns);
boolean process_in_initial_net_ns(process p);

fs_struct fs_struct_create(filesystem root_fs, filesystem cwd_fs, tuple root, inode cwd);
fs_struct fs_struct_copy(fs_struct src);
void fs_struct_reserve(fs_struct fs);
void fs_struct_release(fs_struct fs);

boolean ns_capable(process p, user_namespace target, int cap);
boolean ns_capable_creds(struct creds *c, user_namespace target, int cap);
boolean process_is_chrooted(process p);
