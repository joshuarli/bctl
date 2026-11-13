/* FUSE wire format (Linux include/uapi/linux/fuse.h, protocol 7.31) as carried by virtio-fs. */

#define FUSE_KERNEL_VERSION         7
#define FUSE_KERNEL_MINOR_VERSION   31
#define FUSE_ROOT_ID                1

#define FUSE_LOOKUP     1
#define FUSE_FORGET     2
#define FUSE_GETATTR    3
#define FUSE_SETATTR    4
#define FUSE_READLINK   5
#define FUSE_SYMLINK    6
#define FUSE_MKNOD      8
#define FUSE_MKDIR      9
#define FUSE_UNLINK     10
#define FUSE_RMDIR      11
#define FUSE_RENAME     12
#define FUSE_OPEN       14
#define FUSE_READ       15
#define FUSE_WRITE      16
#define FUSE_STATFS     17
#define FUSE_RELEASE    18
#define FUSE_FSYNC      20
#define FUSE_INIT       26
#define FUSE_OPENDIR    27
#define FUSE_READDIR    28
#define FUSE_RELEASEDIR 29
#define FUSE_CREATE     35

#define FATTR_SIZE      (1 << 3)
#define FATTR_FH        (1 << 6)

#define FUSE_FSYNC_FDATASYNC    (1 << 0)

struct fuse_in_header {
    u32 len;
    u32 opcode;
    u64 unique;
    u64 nodeid;
    u32 uid;
    u32 gid;
    u32 pid;
    u16 total_extlen;
    u16 padding;
};

struct fuse_out_header {
    u32 len;
    s32 error;
    u64 unique;
};

struct fuse_attr {
    u64 ino;
    u64 size;
    u64 blocks;
    u64 atime;
    u64 mtime;
    u64 ctime;
    u32 atimensec;
    u32 mtimensec;
    u32 ctimensec;
    u32 mode;
    u32 nlink;
    u32 uid;
    u32 gid;
    u32 rdev;
    u32 blksize;
    u32 flags;
};

struct fuse_entry_out {
    u64 nodeid;
    u64 generation;
    u64 entry_valid;
    u64 attr_valid;
    u32 entry_valid_nsec;
    u32 attr_valid_nsec;
    struct fuse_attr attr;
};

struct fuse_attr_out {
    u64 attr_valid;
    u32 attr_valid_nsec;
    u32 dummy;
    struct fuse_attr attr;
};

struct fuse_getattr_in {
    u32 getattr_flags;
    u32 dummy;
    u64 fh;
};

struct fuse_setattr_in {
    u32 valid;
    u32 padding;
    u64 fh;
    u64 size;
    u64 lock_owner;
    u64 atime;
    u64 mtime;
    u64 ctime;
    u32 atimensec;
    u32 mtimensec;
    u32 ctimensec;
    u32 mode;
    u32 unused4;
    u32 uid;
    u32 gid;
    u32 unused5;
};

struct fuse_open_in {
    u32 flags;
    u32 open_flags;
};

struct fuse_open_out {
    u64 fh;
    u32 open_flags;
    u32 padding;
};

struct fuse_create_in {
    u32 flags;
    u32 mode;
    u32 umask;
    u32 open_flags;
};

struct fuse_create_out {
    struct fuse_entry_out entry;
    struct fuse_open_out open;
};

struct fuse_release_in {
    u64 fh;
    u32 flags;
    u32 release_flags;
    u64 lock_owner;
};

struct fuse_read_in {
    u64 fh;
    u64 offset;
    u32 size;
    u32 read_flags;
    u64 lock_owner;
    u32 flags;
    u32 padding;
};

struct fuse_write_in {
    u64 fh;
    u64 offset;
    u32 size;
    u32 write_flags;
    u64 lock_owner;
    u32 flags;
    u32 padding;
};

struct fuse_write_out {
    u32 size;
    u32 padding;
};

struct fuse_fsync_in {
    u64 fh;
    u32 fsync_flags;
    u32 padding;
};

struct fuse_forget_in {
    u64 nlookup;
};

struct fuse_mkdir_in {
    u32 mode;
    u32 umask;
};

struct fuse_mknod_in {
    u32 mode;
    u32 rdev;
    u32 umask;
    u32 padding;
};

struct fuse_rename_in {
    u64 newdir;
};

struct fuse_kstatfs {
    u64 blocks;
    u64 bfree;
    u64 bavail;
    u64 files;
    u64 ffree;
    u32 bsize;
    u32 namelen;
    u32 frsize;
    u32 padding;
    u32 spare[6];
};

struct fuse_init_in {
    u32 major;
    u32 minor;
    u32 max_readahead;
    u32 flags;
    u32 flags2;
    u32 unused[11];
};

struct fuse_init_out {
    u32 major;
    u32 minor;
    u32 max_readahead;
    u32 flags;
    u16 max_background;
    u16 congestion_threshold;
    u32 max_write;
    u32 time_gran;
    u16 max_pages;
    u16 map_alignment;
    u32 flags2;
    u32 unused[7];
};

struct fuse_dirent {
    u64 ino;
    u64 off;
    u32 namelen;
    u32 type;
    char name[];
};

#define FUSE_DIRENT_SIZE(namelen)   pad(sizeof(struct fuse_dirent) + (namelen), 8)

/* Transport, implemented by the virtio-fs driver. Requests block the calling context; a return value
 * is 0 or a negative errno. `in` is the request's argument; `out` receives the reply after its header,
 * and *out_len its length. */
int vfs_call(void *transport, u32 opcode, u64 nodeid, const void *in, u32 in_len, void *out, u32 out_len,
             u32 *ret_len);
void vfs_forget(void *transport, u64 nodeid, u64 nlookup);
void vfs_read(void *transport, u64 nodeid, u64 fh, u64 offset, u32 count, void *dest,
              status_handler complete);
void vfs_write(void *transport, u64 nodeid, u64 fh, u64 offset, u32 count, void *src,
               status_handler complete);

void virtiofs_create_fs(heap h, void *transport, boolean readonly, filesystem_complete complete);
