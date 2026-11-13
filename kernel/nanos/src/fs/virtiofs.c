#include <kernel.h>
#include <pagecache.h>
#include <fs.h>
#include <virtiofs.h>
#include <errno.h>
#include <unix/system_structs.h>

//#define VIRTIOFS_DEBUG
#ifdef VIRTIOFS_DEBUG
#define virtiofs_debug(x, ...) do {tprintf(sym(virtiofs), 0, ss(x), ##__VA_ARGS__);} while(0)
#else
#define virtiofs_debug(x, ...)
#endif

/* The largest READ or WRITE payload the driver sends, below the host's negotiated max_write. */
#define VIRTIOFS_IO_MAX     (256 * KB)

#define VIRTIOFS_NO_FH      ((u64)-1)

/* A node the host knows by `nodeid` once the guest has looked it up `nlookup` times; nodes listed by a
 * directory read have no nodeid until they are looked up. */
typedef struct virtiofs_dentry {
    struct list l;
    u64 nodeid;
    u64 nlookup;
    u64 ino;
    tuple md;
    boolean pinned;
} *virtiofs_dentry;

typedef struct virtiofs {
    struct filesystem fs;
    u32 io_max;
    struct virtiofs_dentry root;
    struct list dentries;
    struct list fsfiles;
    void *transport;
} *virtiofs;

typedef struct virtiofs_fsfile {
    struct fsfile f;
    struct list l;
    u64 blocks;
    u64 fh;
    virtiofs_dentry dentry;
    closure_struct(pagecache_node_reserve, reserve);
    closure_struct(thunk, free);
} *virtiofs_fsfile;

static void virtiofs_dentry_delete(virtiofs fs, virtiofs_dentry dentry);

static void virtiofs_dentry_forget(virtiofs fs, virtiofs_dentry dentry)
{
    if (dentry->nlookup && dentry->nodeid != FUSE_ROOT_ID)
        vfs_forget(fs->transport, dentry->nodeid, dentry->nlookup);
    dentry->nodeid = 0;
    dentry->nlookup = 0;
}

static virtiofs_dentry virtiofs_dentry_new(virtiofs fs, u64 nodeid, u64 ino, tuple md)
{
    virtiofs_dentry dentry = allocate(fs->fs.h, sizeof(*dentry));
    if (dentry == INVALID_ADDRESS)
        return 0;
    dentry->nodeid = nodeid;
    dentry->nlookup = nodeid ? 1 : 0;
    dentry->ino = ino;
    dentry->md = md;
    dentry->pinned = false;
    list_insert_after(list_end(&fs->dentries), &dentry->l);
    return dentry;
}

static void virtiofs_dentry_delete(virtiofs fs, virtiofs_dentry dentry)
{
    if (list_inserted(&dentry->l))
        list_delete(&dentry->l);
    virtiofs_dentry_forget(fs, dentry);
    if (dentry->md)
        fs_cleanup_dir_entry(dentry->md);
    deallocate(fs->fs.h, dentry, sizeof(*dentry));
}

static virtiofs_dentry virtiofs_dentry_from_md(virtiofs fs, tuple md);

/* The caller destroys `md`, whose node the host just removed: an open file keeps its dentry until it
 * closes. */
static void virtiofs_dentry_detach(virtiofs fs, tuple md)
{
    virtiofs_dentry dentry = virtiofs_dentry_from_md(fs, md);
    if (!dentry)
        return;
    dentry->md = 0;
    if (!dentry->pinned)
        virtiofs_dentry_delete(fs, dentry);
}

static void virtiofs_cache_hit(struct list *l, struct list *elem)
{
    if (elem != list_begin(l)) {
        list_delete(elem);
        list_insert_after(list_end(l), elem);
    }
}

static virtiofs_dentry virtiofs_dentry_from_md(virtiofs fs, tuple md)
{
    if (md == fs->root.md)
        return &fs->root;
    list_foreach(&fs->dentries, e) {
        virtiofs_dentry dentry = struct_from_list(e, virtiofs_dentry, l);
        if (dentry->md == md) {
            virtiofs_cache_hit(&fs->dentries, &dentry->l);
            return dentry;
        }
    }
    return 0;
}

static virtiofs_dentry virtiofs_dentry_from_ino(virtiofs fs, u64 ino)
{
    if (ino == fs->root.ino)
        return &fs->root;
    list_foreach(&fs->dentries, e) {
        virtiofs_dentry dentry = struct_from_list(e, virtiofs_dentry, l);
        if (dentry->ino == ino) {
            virtiofs_cache_hit(&fs->dentries, &dentry->l);
            return dentry;
        }
    }
    return 0;
}

/* Builds a request argument of `arg` followed by NUL-terminated `names`, in `buf` of `buf_len` bytes. */
static u32 virtiofs_named_arg(void *buf, u32 buf_len, const void *arg, u32 arg_len, string name1, string name2)
{
    u32 len = arg_len + buffer_length(name1) + 1 + (name2 ? buffer_length(name2) + 1 : 0);
    if (len > buf_len)
        return 0;
    if (arg_len)
        runtime_memcpy(buf, arg, arg_len);
    u32 off = arg_len;
    buffer_read_at(name1, 0, buf + off, buffer_length(name1));
    off += buffer_length(name1);
    ((char *)buf)[off++] = '\0';
    if (name2) {
        buffer_read_at(name2, 0, buf + off, buffer_length(name2));
        off += buffer_length(name2);
        ((char *)buf)[off++] = '\0';
    }
    return off;
}

#define VIRTIOFS_ARG_MAX    (sizeof(struct fuse_create_in) + 2 * (NAME_MAX + 1) + PATH_MAX)

static int virtiofs_named_call(virtiofs fs, u32 opcode, u64 nodeid, const void *arg, u32 arg_len,
                               string name1, string name2, void *out, u32 out_len)
{
    void *buf = allocate(fs->fs.h, VIRTIOFS_ARG_MAX);
    if (buf == INVALID_ADDRESS)
        return -ENOMEM;
    u32 len = virtiofs_named_arg(buf, VIRTIOFS_ARG_MAX, arg, arg_len, name1, name2);
    int s = len ? vfs_call(fs->transport, opcode, nodeid, buf, len, out, out_len, 0) : -ENAMETOOLONG;
    deallocate(fs->fs.h, buf, VIRTIOFS_ARG_MAX);
    return s;
}

static int virtiofs_release(virtiofs fs, u32 opcode, u64 nodeid, u64 fh)
{
    struct fuse_release_in in;
    zero(&in, sizeof(in));
    in.fh = fh;
    return vfs_call(fs->transport, opcode, nodeid, &in, sizeof(in), 0, 0, 0);
}

static void virtiofs_set_times(filesystem fs, tuple md, struct fuse_attr *attr)
{
    filesystem_set_atime(fs, md, seconds(attr->atime) + nanoseconds(attr->atimensec));
    filesystem_set_mtime(fs, md, seconds(attr->mtime) + nanoseconds(attr->mtimensec));
}

static void virtiofs_fsf_io(virtiofs_fsfile fsf, boolean write, sg_list sg, range q,
                            status_handler complete)
{
    virtiofs_debug("%s file %p, sg %p, r %R, sh %F\n", write ? ss("write") : ss("read"), fsf, sg, q,
                   complete);
    virtiofs fs = (virtiofs)fsf->f.fs;
    merge m = allocate_merge(fs->fs.h, complete);
    complete = apply_merge(m);
    u64 len = range_span(q);
    u64 offset = q.start;
    while (len > 0) {
        sg_buf sgb = sg_list_head_peek(sg);
        u32 count = MIN(MIN(fs->io_max, len), sg_buf_len(sgb));
        if (write)
            vfs_write(fs->transport, fsf->dentry->nodeid, fsf->fh, offset, count, sgb->buf + sgb->offset,
                      apply_merge(m));
        else
            vfs_read(fs->transport, fsf->dentry->nodeid, fsf->fh, offset, count, sgb->buf + sgb->offset,
                     apply_merge(m));
        sg_consume(sg, count);
        offset += count;
        len -= count;
    }
    apply(complete, STATUS_OK);
}

static void virtiofs_fsf_read(fsfile f, sg_list sg, range q, status_handler completion)
{
    virtiofs_fsf_io((virtiofs_fsfile)f, false, sg, q, completion);
}

closure_function(3, 1, void, virtiofs_fsf_write_complete,
                 fsfile, f, u64, write_end, status_handler, completion,
                 status s)
{
    if (is_ok(s)) {
        fsfile f = bound(f);
        tuple md = f->md;
        if (md)
            filesystem_update_mtime(f->fs, md);
        u64 write_end = bound(write_end);
        if (write_end > f->length)
            fsfile_set_length(f, write_end);
    }
    apply(bound(completion), s);
    closure_finish();
}

static void virtiofs_fsf_write(fsfile f, sg_list sg, range q, status_handler completion)
{
    context ctx = context_from_closure(completion);
    status_handler write_complete;
    if (ctx)    /* direct I/O */
        write_complete = closure_from_context(ctx, virtiofs_fsf_write_complete, f, q.end, completion);
    else    /* I/O via page cache */
        write_complete = closure(f->fs->h, virtiofs_fsf_write_complete, f, q.end, completion);
    if (write_complete != INVALID_ADDRESS) {
        virtiofs_fsf_io((virtiofs_fsfile)f, true, sg, q, write_complete);
    } else {
        status s = timm("result", "out of memory");
        apply(completion, timm_append(s, "fsstatus", "%d", -ENOMEM));
    }
}

closure_func_basic(pagecache_node_reserve, status, virtiofs_fsf_reserve,
                   range q)
{
    virtiofs_fsfile fsf = struct_from_field(closure_self(), virtiofs_fsfile, reserve);
    fsfile f = &fsf->f;
    if (f->length < q.end)
        f->length = q.end;
    return STATUS_OK;
}

static s64 virtiofs_get_blocks(fsfile f)
{
    return ((virtiofs_fsfile)f)->blocks;
}

static void virtiofs_fsfile_delete(virtiofs fs, virtiofs_fsfile fsf)
{
    if (list_inserted(&fsf->l))
        list_delete(&fsf->l);
    if (fsf->fh != VIRTIOFS_NO_FH)
        virtiofs_release(fs, FUSE_RELEASE, fsf->dentry->nodeid, fsf->fh);
    fsf->dentry->pinned = false;
    pagecache_deallocate_node(fsf->f.cache_node);
    deallocate(fs->fs.h, fsf, sizeof(*fsf));
}

closure_func_basic(status_handler, void, virtiofs_fsf_sync_complete,
                   status s)
{
    if (!is_ok(s)) {
        msg_err("virtiofs: failed to sync page cache node: %v", s);
        timm_dealloc(s);
    }
    fsfile f = struct_from_closure(fsfile, sync_complete);
    virtiofs_fsfile fsf = (virtiofs_fsfile)f;
    filesystem fs = f->fs;
    virtiofs vfs = (virtiofs)fs;
    filesystem_lock(fs);
    virtiofs_dentry dentry = fsf->dentry;
    virtiofs_fsfile_delete(vfs, fsf);
    if (!dentry->md)
        virtiofs_dentry_delete(vfs, dentry);
    filesystem_unlock(fs);
}

closure_func_basic(thunk, void, virtiofs_fsf_free)
{
    virtiofs_fsfile fsf = struct_from_field(closure_self(), virtiofs_fsfile, free);
    fsfile f = &fsf->f;
    filesystem fs = f->fs;
    filesystem_lock(fs);
    if (f->refcount.c != 0) {
        /* Someone obtained a reference to this fsfile before we could remove it from the open file
         * list. */
        filesystem_unlock(fs);
        return;
    }
    /* The node and its metadata stay: only directory reads remove entries, so nothing can still
     * reach a tuple freed here. */
    list_delete(&fsf->l);
    filesystem_unlock(fs);
    pagecache_sync_node(f->cache_node,
                        init_closure_func(&f->sync_complete, status_handler, virtiofs_fsf_sync_complete));
}

static virtiofs_fsfile virtiofs_fsfile_new(virtiofs fs, virtiofs_dentry dentry)
{
    heap h = fs->fs.h;
    virtiofs_fsfile fsf = allocate(h, sizeof(*fsf));
    if (fsf == INVALID_ADDRESS)
        return 0;
    fsf->dentry = dentry;
    fsf->fh = VIRTIOFS_NO_FH;
    fsf->blocks = 0;
    int s = fsfile_init(&fs->fs, &fsf->f, dentry->md,
                        init_closure_func(&fsf->reserve, pagecache_node_reserve, virtiofs_fsf_reserve),
                        init_closure_func(&fsf->free, thunk, virtiofs_fsf_free));
    if (s != 0) {
        deallocate(h, fsf, sizeof(*fsf));
        return 0;
    }
    fsf->f.get_blocks = virtiofs_get_blocks;
    list_insert_after(list_end(&fs->fsfiles), &fsf->l);
    dentry->pinned = true;
    return fsf;
}

closure_function(2, 2, boolean, virtiofs_dir_cleanup,
                 virtiofs, fs, tuple, other_c,
                 value k, value v)
{
    virtiofs fs = bound(fs);
    tuple other_c = bound(other_c);
    if (!other_c || !get_tuple(other_c, k)) {
        virtiofs_dentry dentry = virtiofs_dentry_from_md(fs, v);
        if (dentry) {
            fs_notify_release(dentry->md, false);
            virtiofs_dentry_delete(fs, dentry);
        } else {
            deallocate_value(v);
        }
    }
    return true;
}

static void virtiofs_set_type(tuple t, tuple parent, u32 dt)
{
    symbol attr;
    switch (dt) {
    case DT_DIR:
        attr = sym(children);
        if (!get(t, attr))
            set(t, attr, allocate_tuple());
        set(t, sym(..), parent);
        break;
    case DT_LNK:
        attr = sym(linktarget);
        if (!get(t, attr))
            set(t, attr, null_value);
        break;
    case DT_SOCK:
        attr = sym(socket);
        if (!get(t, attr))
            set(t, attr, null_value);
        break;
    }
}

static int virtiofs_readdir(virtiofs fs, virtiofs_dentry dir)
{
    virtiofs_debug("readdir: nodeid %ld, md %p\n", dir->nodeid, dir->md);
    struct fuse_open_in open_in;
    zero(&open_in, sizeof(open_in));
    open_in.flags = O_RDONLY;
    struct fuse_open_out open_out;
    int s = vfs_call(fs->transport, FUSE_OPENDIR, dir->nodeid, &open_in, sizeof(open_in), &open_out,
                     sizeof(open_out), 0);
    if (s != 0)
        return s;
    const u32 buf_size = PAGESIZE;
    u8 *buf = allocate(fs->fs.h, buf_size);
    if (buf == INVALID_ADDRESS) {
        virtiofs_release(fs, FUSE_RELEASEDIR, dir->nodeid, open_out.fh);
        return -ENOMEM;
    }
    tuple md = dir->md;
    tuple old_c = children(md);
    tuple new_c = allocate_tuple();
    struct fuse_read_in read_in;
    zero(&read_in, sizeof(read_in));
    read_in.fh = open_out.fh;
    read_in.size = buf_size;
    u32 count;
    do {
        s = vfs_call(fs->transport, FUSE_READDIR, dir->nodeid, &read_in, sizeof(read_in), buf, buf_size,
                     &count);
        if (s != 0)
            break;
        u32 off = 0;
        while (off + sizeof(struct fuse_dirent) <= count) {
            struct fuse_dirent *entry = (void *)buf + off;
            if (entry->namelen > NAME_MAX || off + FUSE_DIRENT_SIZE(entry->namelen) > count) {
                s = -EIO;
                break;
            }
            sstring name = isstring(entry->name, entry->namelen);
            if (runtime_strcmp(name, ss(".")) && runtime_strcmp(name, ss(".."))) {
                symbol name_sym = intern(alloca_wrap_sstring(name));
                tuple t = old_c ? get_tuple(old_c, name_sym) : 0;
                virtiofs_dentry dentry;
                if (t) {
                    dentry = virtiofs_dentry_from_md(fs, t);
                } else {
                    t = allocate_tuple();
                    dentry = 0;
                }
                if (!dentry) {
                    dentry = virtiofs_dentry_new(fs, 0, entry->ino, t);
                    if (!dentry) {
                        deallocate_value(t);
                        s = -ENOMEM;
                        break;
                    }
                }
                virtiofs_set_type(t, md, entry->type);
                set(new_c, name_sym, t);
            }
            read_in.offset = entry->off;
            off += FUSE_DIRENT_SIZE(entry->namelen);
        }
        if (s != 0)
            break;
    } while (count > 0);
    deallocate(fs->fs.h, buf, buf_size);
    virtiofs_release(fs, FUSE_RELEASEDIR, dir->nodeid, open_out.fh);
    if (s == 0) {
        if (old_c) {
            iterate(old_c, stack_closure(virtiofs_dir_cleanup, fs, new_c));
            deallocate_value(old_c);
        }
        set(md, sym(children), new_c);
    } else {
        iterate(new_c, stack_closure(virtiofs_dir_cleanup, fs, old_c));
        deallocate_value(new_c);
    }
    return s;
}

static int virtiofs_readlink(virtiofs fs, u64 nodeid, tuple md)
{
    buffer target = allocate_buffer(fs->fs.h, PATH_MAX);
    if (target == INVALID_ADDRESS)
        return -ENOMEM;
    u32 len;
    int s = vfs_call(fs->transport, FUSE_READLINK, nodeid, 0, 0, buffer_ref(target, 0), PATH_MAX, &len);
    if (s != 0) {
        deallocate_buffer(target);
        return s;
    }
    buffer_produce(target, len);
    symbol target_sym = sym(linktarget);
    buffer old_target = get_string(md, target_sym);
    if (old_target && (old_target != null_value))
        deallocate_buffer(old_target);
    set(md, target_sym, target);
    return 0;
}

static u32 virtiofs_dt_from_mode(u32 mode)
{
    switch (mode & S_IFMT) {
    case S_IFDIR:
        return DT_DIR;
    case S_IFLNK:
        return DT_LNK;
    case S_IFSOCK:
        return DT_SOCK;
    default:
        return DT_REG;
    }
}

/* Records a node the host just returned in `entry`, which counts as one lookup of it. */
static virtiofs_dentry virtiofs_entry_dentry(virtiofs fs, struct fuse_entry_out *entry, tuple md)
{
    virtiofs_dentry dentry = virtiofs_dentry_from_ino(fs, entry->attr.ino);
    if (dentry && dentry != &fs->root) {
        if (dentry->nodeid == entry->nodeid) {
            dentry->nlookup++;
        } else {
            virtiofs_dentry_forget(fs, dentry);
            dentry->nodeid = entry->nodeid;
            dentry->nlookup = 1;
        }
        return dentry;
    }
    return virtiofs_dentry_new(fs, entry->nodeid, entry->attr.ino, md);
}

static int virtiofs_rename(filesystem fs, tuple old_parent, string old_name, tuple old_md,
                           tuple new_parent, string new_name, tuple new_md, boolean exchange,
                           boolean *destruct_md)
{
    if (exchange)
        return -EINVAL; /* not supported */
    virtiofs vfs = (virtiofs)fs;
    virtiofs_dentry oldp = virtiofs_dentry_from_md(vfs, old_parent);
    virtiofs_dentry newp = virtiofs_dentry_from_md(vfs, new_parent);
    if (!oldp || !newp || !oldp->nodeid || !newp->nodeid)
        return -ENOENT;
    struct fuse_rename_in in = { .newdir = newp->nodeid };
    int s = virtiofs_named_call(vfs, FUSE_RENAME, oldp->nodeid, &in, sizeof(in), old_name, new_name, 0, 0);
    if ((s == 0) && new_md) {
        virtiofs_dentry_detach(vfs, new_md);
        *destruct_md = true;
    }
    return s;
}

static int virtiofs_unlink(filesystem fs, tuple parent, string name, tuple md, boolean *destruct_md)
{
    virtiofs vfs = (virtiofs)fs;
    virtiofs_dentry parent_dentry = virtiofs_dentry_from_md(vfs, parent);
    if (!parent_dentry || !parent_dentry->nodeid)
        return -ENOENT;
    int s = virtiofs_named_call(vfs, is_dir(md) ? FUSE_RMDIR : FUSE_UNLINK, parent_dentry->nodeid, 0, 0,
                                name, 0, 0, 0);
    if (s == 0) {
        virtiofs_dentry_detach(vfs, md);
        *destruct_md = true;
    }
    return s;
}

static int virtiofs_open(virtiofs vfs, virtiofs_fsfile fsf)
{
    u64 nodeid = fsf->dentry->nodeid;
    struct fuse_open_in in;
    zero(&in, sizeof(in));
    struct fuse_open_out out;
    in.flags = O_RDWR;
    int s = vfs_call(vfs->transport, FUSE_OPEN, nodeid, &in, sizeof(in), &out, sizeof(out), 0);
    if (s == -EACCES || s == -EROFS || s == -EPERM) {
        in.flags = O_RDONLY;
        s = vfs_call(vfs->transport, FUSE_OPEN, nodeid, &in, sizeof(in), &out, sizeof(out), 0);
    }
    if (s == 0)
        fsf->fh = out.fh;
    return s;
}

static int virtiofs_get_fsfile(filesystem fs, tuple md, fsfile *f)
{
    virtiofs_debug("get fsfile, md %p\n", md);
    virtiofs vfs = (virtiofs)fs;
    list_foreach(&vfs->fsfiles, e) {
        virtiofs_fsfile fsf = struct_from_list(e, virtiofs_fsfile, l);
        if (fsf->f.md == md) {
            virtiofs_cache_hit(&vfs->fsfiles, &fsf->l);
            fsfile_reserve(&fsf->f);
            *f = &fsf->f;
            return 0;
        }
    }
    virtiofs_dentry dentry = virtiofs_dentry_from_md(vfs, md);
    if (!dentry || !dentry->nodeid)
        return -ENOENT;
    virtiofs_fsfile fsf = virtiofs_fsfile_new(vfs, dentry);
    if (!fsf)
        return -ENOMEM;
    struct fuse_getattr_in in;
    zero(&in, sizeof(in));
    struct fuse_attr_out out;
    int s = vfs_call(vfs->transport, FUSE_GETATTR, dentry->nodeid, &in, sizeof(in), &out, sizeof(out), 0);
    if (s == 0)
        s = virtiofs_open(vfs, fsf);
    if (s != 0) {
        virtiofs_fsfile_delete(vfs, fsf);
        return s;
    }
    fsf->blocks = out.attr.blocks;
    virtiofs_set_times(fs, md, &out.attr);
    fsfile_set_length(&fsf->f, out.attr.size);
    *f = &fsf->f;
    return 0;
}

static inode virtiofs_get_inode(filesystem fs, tuple md)
{
    virtiofs_dentry dentry = virtiofs_dentry_from_md((virtiofs)fs, md);
    return dentry ? dentry->ino : 0;
}

static tuple virtiofs_get_meta(filesystem fs, inode n)
{
    virtiofs_dentry dentry = virtiofs_dentry_from_ino((virtiofs)fs, n);
    return dentry ? dentry->md : 0;
}

static tuple virtiofs_lookup(filesystem fs, tuple parent, string name)
{
    virtiofs_debug("lookup %p '%b'\n", parent, name);
    virtiofs vfs = (virtiofs)fs;
    virtiofs_dentry parent_dentry = virtiofs_dentry_from_md(vfs, parent);
    if (!parent_dentry || !parent_dentry->nodeid)
        return 0;
    if (!buffer_strcmp(name, "."))
        return (virtiofs_readdir(vfs, parent_dentry) == 0) ? parent : 0;
    struct fuse_entry_out entry;
    if (virtiofs_named_call(vfs, FUSE_LOOKUP, parent_dentry->nodeid, 0, 0, name, 0, &entry,
                            sizeof(entry)) != 0)
        return 0;
    if (entry.nodeid == 0)  /* a negative entry */
        return 0;
    virtiofs_dentry dentry = virtiofs_dentry_from_ino(vfs, entry.attr.ino);
    boolean new_md = !dentry || !dentry->md;
    tuple md = new_md ? allocate_tuple() : dentry->md;
    u32 dt = virtiofs_dt_from_mode(entry.attr.mode);
    if (new_md)
        virtiofs_set_type(md, parent, dt);
    parent_dentry->pinned = true;
    dentry = virtiofs_entry_dentry(vfs, &entry, md);
    parent_dentry->pinned = false;
    if (!dentry) {
        vfs_forget(vfs->transport, entry.nodeid, 1);
        if (new_md)
            fs_cleanup_dir_entry(md);
        return 0;
    }
    if (!dentry->md)
        dentry->md = md;
    int s = 0;
    if (dt == DT_DIR)
        s = virtiofs_readdir(vfs, dentry);
    else if (dt == DT_LNK)
        s = virtiofs_readlink(vfs, dentry->nodeid, md);
    if (s != 0 && new_md) {
        list_delete(&dentry->l);
        virtiofs_dentry_delete(vfs, dentry);
        return 0;
    }
    return md;
}

static int virtiofs_create(filesystem fs, tuple parent, string name, tuple md, fsfile *f)
{
    if (!name)
        return -EOPNOTSUPP;
    virtiofs_debug("create parent %p name '%b' md %p f %p\n", parent, name, md, f);
    virtiofs vfs = (virtiofs)fs;
    virtiofs_dentry parent_dentry = virtiofs_dentry_from_md(vfs, parent);
    if (!parent_dentry || !parent_dentry->nodeid)
        return -ENOENT;
    u64 pnode = parent_dentry->nodeid;
    struct fuse_create_out out;
    int s;
    if (is_dir(md)) {
        struct fuse_mkdir_in in = { .mode = 0777, .umask = 0 };
        s = virtiofs_named_call(vfs, FUSE_MKDIR, pnode, &in, sizeof(in), name, 0, &out.entry, sizeof(out.entry));
    } else if (is_symlink(md)) {
        s = virtiofs_named_call(vfs, FUSE_SYMLINK, pnode, 0, 0, name, linktarget(md), &out.entry,
                                sizeof(out.entry));
    } else if (is_socket(md) || !f) {
        struct fuse_mknod_in in;
        zero(&in, sizeof(in));
        in.mode = (is_socket(md) ? S_IFSOCK : S_IFREG) | 0644;
        s = virtiofs_named_call(vfs, FUSE_MKNOD, pnode, &in, sizeof(in), name, 0, &out.entry, sizeof(out.entry));
    } else {
        struct fuse_create_in in;
        zero(&in, sizeof(in));
        in.flags = O_RDWR;
        in.mode = S_IFREG | 0644;
        s = virtiofs_named_call(vfs, FUSE_CREATE, pnode, &in, sizeof(in), name, 0, &out, sizeof(out));
    }
    if (s != 0)
        return s;
    virtiofs_dentry dentry = virtiofs_dentry_new(vfs, out.entry.nodeid, out.entry.attr.ino, md);
    if (!dentry) {
        vfs_forget(vfs->transport, out.entry.nodeid, 1);
        return -ENOMEM;
    }
    if (!f || is_dir(md) || is_symlink(md) || is_socket(md))
        return 0;
    virtiofs_fsfile fsf = virtiofs_fsfile_new(vfs, dentry);
    if (!fsf) {
        virtiofs_release(vfs, FUSE_RELEASE, out.entry.nodeid, out.open.fh);
        dentry->md = 0;
        virtiofs_dentry_delete(vfs, dentry);
        return -ENOMEM;
    }
    fsf->fh = out.open.fh;
    *f = &fsf->f;
    return 0;
}

static int virtiofs_truncate(filesystem fs, fsfile f, u64 len)
{
    virtiofs_fsfile fsf = (virtiofs_fsfile)f;
    struct fuse_setattr_in in;
    zero(&in, sizeof(in));
    in.valid = FATTR_SIZE | FATTR_FH;
    in.fh = fsf->fh;
    in.size = len;
    struct fuse_attr_out out;
    return vfs_call(((virtiofs)fs)->transport, FUSE_SETATTR, fsf->dentry->nodeid, &in, sizeof(in), &out,
                    sizeof(out), 0);
}

static int virtiofs_fsync(virtiofs fs, virtiofs_fsfile f, boolean datasync)
{
    struct fuse_fsync_in in;
    zero(&in, sizeof(in));
    in.fh = f->fh;
    in.fsync_flags = datasync ? FUSE_FSYNC_FDATASYNC : 0;
    return vfs_call(fs->transport, FUSE_FSYNC, f->dentry->nodeid, &in, sizeof(in), 0, 0, 0);
}

closure_function(4, 1, void, virtiofs_cache_sync_complete,
                 filesystem, fs, fsfile, f, boolean, datasync, status_handler, completion,
                 status s)
{
    if (is_ok(s)) {
        virtiofs fs = (virtiofs)bound(fs);
        virtiofs_fsfile f = (virtiofs_fsfile)bound(f);
        boolean datasync = bound(datasync);
        int fss = 0;
        filesystem_lock(&fs->fs);
        if (f) {
            fss = virtiofs_fsync(fs, f, datasync);
        } else {
            list_foreach(&fs->fsfiles, e) {
                fss = virtiofs_fsync(fs, struct_from_list(e, virtiofs_fsfile, l), datasync);
                if (fss != 0)
                    break;
            }
        }
        filesystem_unlock(&fs->fs);
        if (fss != 0)
            s = timm("result", "fsync failed (%d)", fss);
    }
    async_apply_status_handler(bound(completion), s);
    closure_finish();
}

static status_handler virtiofs_get_sync_handler(filesystem fs, fsfile fsf, boolean datasync,
                                                status_handler completion)
{
    return closure(fs->h, virtiofs_cache_sync_complete, fs, fsf, datasync, completion);
}

void virtiofs_create_fs(heap h, void *transport, boolean readonly, filesystem_complete complete)
{
    virtiofs fs = allocate(h, sizeof(*fs));
    status s;
    if (fs == INVALID_ADDRESS) {
        apply(complete, INVALID_ADDRESS, timm("result", "failed to allocate fs"));
        return;
    }
    struct fuse_init_in init_in;
    zero(&init_in, sizeof(init_in));
    init_in.major = FUSE_KERNEL_VERSION;
    init_in.minor = FUSE_KERNEL_MINOR_VERSION;
    struct fuse_init_out init_out;
    zero(&init_out, sizeof(init_out));
    int fss = vfs_call(transport, FUSE_INIT, 0, &init_in, sizeof(init_in), &init_out, sizeof(init_out), 0);
    if (fss == 0 && init_out.major != FUSE_KERNEL_VERSION)
        fss = -EPROTONOSUPPORT;
    if (fss != 0) {
        s = timm("result", "failed to negotiate the FUSE protocol (%d)", fss);
        goto dealloc_fs;
    }
    fs->io_max = MIN(VIRTIOFS_IO_MAX, init_out.max_write ? init_out.max_write : PAGESIZE);
    struct fuse_getattr_in getattr_in;
    zero(&getattr_in, sizeof(getattr_in));
    struct fuse_attr_out root_attr;
    fss = vfs_call(transport, FUSE_GETATTR, FUSE_ROOT_ID, &getattr_in, sizeof(getattr_in), &root_attr,
                   sizeof(root_attr), 0);
    if (fss != 0) {
        s = timm("result", "failed to read the root directory (%d)", fss);
        goto dealloc_fs;
    }
    struct fuse_kstatfs statfs;
    fss = vfs_call(transport, FUSE_STATFS, FUSE_ROOT_ID, 0, 0, &statfs, sizeof(statfs), 0);
    if (fss != 0) {
        s = timm("result", "failed to get filesystem information (%d)", fss);
        goto dealloc_fs;
    }
    s = filesystem_init(&fs->fs, h, statfs.blocks * statfs.bsize, 1, readonly);
    if (!is_ok(s)) {
        s = timm_up(s, "result", "failed to init fs");
        goto dealloc_fs;
    }
    fs->root.nodeid = FUSE_ROOT_ID;
    fs->root.nlookup = 0;
    fs->root.ino = root_attr.attr.ino;
    fs->root.pinned = true;
    fs->fs.root = fs->root.md = allocate_tuple();
    set(fs->root.md, sym_this(".."), fs->root.md);
    fs->fs.lookup = virtiofs_lookup;
    fs->fs.create = virtiofs_create;
    fs->fs.unlink = virtiofs_unlink;
    fs->fs.rename = virtiofs_rename;
    fs->fs.truncate = virtiofs_truncate;
    fs->fs.get_fsfile = virtiofs_get_fsfile;
    fs->fs.get_inode = virtiofs_get_inode;
    fs->fs.file_read = virtiofs_fsf_read;
    fs->fs.file_write = virtiofs_fsf_write;
    fs->fs.get_meta = virtiofs_get_meta;
    fs->fs.get_sync_handler = virtiofs_get_sync_handler;
    list_init(&fs->dentries);
    list_init(&fs->fsfiles);
    fs->transport = transport;
    apply(complete, &fs->fs, STATUS_OK);
    return;
  dealloc_fs:
    deallocate(h, fs, sizeof(*fs));
    apply(complete, INVALID_ADDRESS, s);
}
