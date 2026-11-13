#include <kernel.h>
#include <pagecache.h>
#include <fs.h>
#include <virtiofs.h>
#include <errno.h>
#include <storage.h>

#include "virtio_internal.h"
#include "virtio_mmio.h"
#include "virtio_pci.h"

//#define VIRTIO_FS_DEBUG
#ifdef VIRTIO_FS_DEBUG
#define vfs_debug(x, ...) do {tprintf(sym(virtiofs), 0, ss(x), ##__VA_ARGS__);} while(0)
#else
#define vfs_debug(x, ...)
#endif

/* virtio-fs device configuration (virtio spec 5.11.4) */
#define VIRTIO_FS_TAG_LEN       36
#define VIRTIO_FS_CFG_TAG       0
#define VIRTIO_FS_CFG_NUM_QUEUES VIRTIO_FS_TAG_LEN

#define VIRTIO_FS_HIPRIO_QUEUE  0
#define VIRTIO_FS_REQUEST_QUEUE 1

typedef struct virtio_fs {
    heap general;
    backed_heap backed;
    closure_struct(fs_init_handler, fs_init);
    virtqueue hiprio;
    virtqueue requests;
    u64 next_unique;
    struct spinlock lock;
} *virtio_fs;

static u64 vfs_unique(virtio_fs v)
{
    spin_lock(&v->lock);
    u64 unique = ++v->next_unique;
    spin_unlock(&v->lock);
    return unique;
}

static void vfs_fill_in_header(virtio_fs v, struct fuse_in_header *hdr, u32 opcode, u64 nodeid,
                               u32 len)
{
    zero(hdr, sizeof(*hdr));
    hdr->len = len;
    hdr->opcode = opcode;
    hdr->unique = vfs_unique(v);
    hdr->nodeid = nodeid;
}

closure_function(2, 1, void, vfs_call_complete,
                 context, ctx, u32 *, ret_len,
                 u64 len)
{
    *bound(ret_len) = len;
    context_schedule_return(bound(ctx));
    closure_finish();
}

int vfs_call(void *transport, u32 opcode, u64 nodeid, const void *in, u32 in_len, void *out, u32 out_len,
             u32 *ret_len)
{
    vfs_debug("call opcode %d nodeid %ld in %d out %d\n", opcode, nodeid, in_len, out_len);
    virtio_fs v = transport;
    u32 req_len = sizeof(struct fuse_in_header) + in_len;
    u32 resp_len = sizeof(struct fuse_out_header) + out_len;
    u64 phys;
    void *buf = alloc_map(v->backed, req_len + resp_len, &phys);
    if (buf == INVALID_ADDRESS)
        return -ENOMEM;
    vfs_fill_in_header(v, buf, opcode, nodeid, req_len);
    if (in_len)
        runtime_memcpy(buf + sizeof(struct fuse_in_header), in, in_len);
    struct fuse_out_header *out_hdr = buf + req_len;
    zero(out_hdr, sizeof(*out_hdr));
    int s;
    context ctx = get_current_context(current_cpu());
    u32 len = 0;
    vqfinish finish = closure(v->general, vfs_call_complete, ctx, &len);
    if (finish == INVALID_ADDRESS) {
        s = -ENOMEM;
        goto out;
    }
    vqmsg m = allocate_vqmsg(v->requests, 2);
    if (m == INVALID_ADDRESS) {
        deallocate_closure(finish);
        s = -ENOMEM;
        goto out;
    }
    vqmsg_push(v->requests, m, phys, req_len, false);
    vqmsg_push(v->requests, m, phys + req_len, resp_len, true);
    context_pre_suspend(ctx);
    vqmsg_commit(v->requests, m, finish);
    context_suspend();
    if (len < sizeof(*out_hdr) || out_hdr->len < sizeof(*out_hdr)) {
        s = -EIO;
    } else if (out_hdr->error) {
        s = out_hdr->error < 0 ? out_hdr->error : -EIO;
    } else {
        u32 got = MIN(out_hdr->len, len) - sizeof(*out_hdr);
        got = MIN(got, out_len);
        if (got)
            runtime_memcpy(out, out_hdr + 1, got);
        if (ret_len)
            *ret_len = got;
        s = 0;
    }
  out:
    dealloc_unmap(v->backed, buf, phys, req_len + resp_len);
    vfs_debug("  opcode %d result %d\n", opcode, s);
    return s;
}

closure_function(3, 1, void, vfs_forget_complete,
                 virtio_fs, v, void *, buf, u64, phys,
                 u64 len)
{
    dealloc_unmap(bound(v)->backed, bound(buf), bound(phys),
                  sizeof(struct fuse_in_header) + sizeof(struct fuse_forget_in));
    closure_finish();
}

/* FORGET has no reply; it travels on the high-priority queue so it cannot wait behind requests. */
void vfs_forget(void *transport, u64 nodeid, u64 nlookup)
{
    vfs_debug("forget nodeid %ld nlookup %ld\n", nodeid, nlookup);
    virtio_fs v = transport;
    u32 len = sizeof(struct fuse_in_header) + sizeof(struct fuse_forget_in);
    u64 phys;
    struct fuse_in_header *hdr = alloc_map(v->backed, len, &phys);
    if (hdr == INVALID_ADDRESS)
        return;
    vfs_fill_in_header(v, hdr, FUSE_FORGET, nodeid, len);
    ((struct fuse_forget_in *)(hdr + 1))->nlookup = nlookup;
    vqfinish finish = closure(v->general, vfs_forget_complete, v, hdr, phys);
    if (finish == INVALID_ADDRESS)
        goto fail;
    vqmsg m = allocate_vqmsg(v->hiprio, 1);
    if (m == INVALID_ADDRESS) {
        deallocate_closure(finish);
        goto fail;
    }
    vqmsg_push(v->hiprio, m, phys, len, false);
    vqmsg_commit(v->hiprio, m, finish);
    return;
  fail:
    dealloc_unmap(v->backed, hdr, phys, len);
}

typedef struct vfs_io_req {
    struct fuse_in_header in_hdr;
    union {
        struct fuse_read_in read;
        struct fuse_write_in write;
    };
    struct fuse_out_header out_hdr;
    struct fuse_write_out write_out;
} *vfs_io_req;

closure_function(6, 1, void, vfs_io_complete,
                 virtio_fs, v, vfs_io_req, io, u64, phys, boolean, write, void *, dest, status_handler, complete,
                 u64 len)
{
    vfs_io_req io = bound(io);
    boolean write = bound(write);
    u32 count = write ? io->write.size : io->read.size;
    vfs_debug("%s complete, len %ld\n", write ? ss("write") : ss("read"), len);
    status s = STATUS_OK;
    if (len < sizeof(io->out_hdr) || io->out_hdr.len < sizeof(io->out_hdr)) {
        s = timm("result", "virtiofs %s of %d bytes: short reply", write ? ss("write") : ss("read"), count);
    } else if (io->out_hdr.error) {
        s = timm("result", "virtiofs %s of %d bytes failed", write ? ss("write") : ss("read"), count);
        s = timm_append(s, "fsstatus", "%d", io->out_hdr.error);
    } else if (write) {
        if (io->write_out.size != count)
            s = timm("result", "virtiofs wrote %d of %d bytes", io->write_out.size, count);
    } else {
        /* The host file can shrink underneath the guest: what lies past its end reads as zeros. */
        u32 got = io->out_hdr.len - sizeof(io->out_hdr);
        if (got < count)
            zero(bound(dest) + got, count - got);
    }
    dealloc_unmap(bound(v)->backed, io, bound(phys), sizeof(*io));
    apply(bound(complete), s);
    closure_finish();
}

static void vfs_io(virtio_fs v, boolean write, u64 nodeid, u64 fh, u64 offset, u32 count, void *data,
                   status_handler complete)
{
    vfs_debug("%s nodeid %ld fh %ld offset %ld count %d\n", write ? ss("write") : ss("read"), nodeid, fh,
              offset, count);
    status s;
    u64 phys;
    vfs_io_req io = alloc_map(v->backed, sizeof(*io), &phys);
    if (io == INVALID_ADDRESS) {
        s = timm("result", "failed to allocate request");
        goto error;
    }
    u32 arg_len = write ? sizeof(io->write) : sizeof(io->read);
    u32 req_len = sizeof(io->in_hdr) + arg_len;
    vfs_fill_in_header(v, &io->in_hdr, write ? FUSE_WRITE : FUSE_READ, nodeid, req_len + (write ? count : 0));
    zero(&io->write, sizeof(io->write));
    if (write) {
        io->write.fh = fh;
        io->write.offset = offset;
        io->write.size = count;
    } else {
        io->read.fh = fh;
        io->read.offset = offset;
        io->read.size = count;
    }
    zero(&io->out_hdr, sizeof(io->out_hdr) + sizeof(io->write_out));
    vqfinish finish = closure(v->general, vfs_io_complete, v, io, phys, write, data, complete);
    if (finish == INVALID_ADDRESS) {
        s = timm("result", "failed to allocate vqfinish");
        goto dealloc;
    }
    vqmsg m = allocate_vqmsg(v->requests, 3);
    if (m == INVALID_ADDRESS) {
        s = timm("result", "failed to allocate vqmsg");
        deallocate_closure(finish);
        goto dealloc;
    }
    u64 out_phys = phys + offsetof(vfs_io_req, out_hdr);
    vqmsg_push(v->requests, m, phys, req_len, false);
    if (write) {
        vqmsg_push(v->requests, m, physical_from_virtual(data), count, false);
        vqmsg_push(v->requests, m, out_phys, sizeof(io->out_hdr) + sizeof(io->write_out), true);
    } else {
        vqmsg_push(v->requests, m, out_phys, sizeof(io->out_hdr), true);
        vqmsg_push(v->requests, m, physical_from_virtual(data), count, true);
    }
    vqmsg_commit(v->requests, m, finish);
    return;
  dealloc:
    dealloc_unmap(v->backed, io, phys, sizeof(*io));
  error:
    s = timm_append(s, "fsstatus", "%d", -ENOMEM);
    apply(complete, s);
}

void vfs_read(void *transport, u64 nodeid, u64 fh, u64 offset, u32 count, void *dest,
              status_handler complete)
{
    vfs_io(transport, false, nodeid, fh, offset, count, dest, complete);
}

void vfs_write(void *transport, u64 nodeid, u64 fh, u64 offset, u32 count, void *src,
               status_handler complete)
{
    vfs_io(transport, true, nodeid, fh, offset, count, src, complete);
}

closure_func_basic(fs_init_handler, void, vfs_fs_init,
                   boolean readonly, filesystem_complete complete)
{
    virtio_fs v = struct_from_field(closure_self(), virtio_fs, fs_init);
    virtiofs_create_fs(v->general, v, readonly, complete);
}

static boolean virtio_fs_dev_attach(heap general, backed_heap backed, vtdev dev)
{
    char tag[VIRTIO_FS_TAG_LEN + 1];
    vtdev_cfg_read_mem(dev, VIRTIO_FS_CFG_TAG, tag, VIRTIO_FS_TAG_LEN);
    tag[VIRTIO_FS_TAG_LEN] = '\0';
    sstring tag_str = sstring_from_cstring(tag, VIRTIO_FS_TAG_LEN);
    if (tag_str.len == 0 || tag_str.len >= VOLUME_LABEL_MAX_LEN) {
        msg_err("virtiofs: unusable tag '%s'", tag_str);
        return false;
    }
    u32 num_queues = vtdev_cfg_read_4(dev, VIRTIO_FS_CFG_NUM_QUEUES);
    vfs_debug("tag '%s', %d request queues\n", tag_str, num_queues);
    if (num_queues < 1) {
        msg_err("virtiofs %s: device offers no request queue", tag_str);
        return false;
    }
    virtio_fs v = allocate(general, sizeof(*v));
    if (v == INVALID_ADDRESS)
        return false;
    status s = virtio_alloc_virtqueue(dev, ss("virtiofs hiprio"), VIRTIO_FS_HIPRIO_QUEUE, &v->hiprio);
    if (is_ok(s))
        s = virtio_alloc_virtqueue(dev, ss("virtiofs requests"), VIRTIO_FS_REQUEST_QUEUE, &v->requests);
    if (!is_ok(s)) {
        msg_err("virtiofs %s: failed to allocate virtqueues: %v", tag_str, s);
        timm_dealloc(s);
        goto err;
    }
    spin_lock_init(&v->lock);
    v->general = general;
    v->backed = backed;
    v->next_unique = 0;
    vtdev_set_status(dev, VIRTIO_CONFIG_STATUS_DRIVER_OK);
    u8 uuid[UUID_LEN];
    zero(uuid, sizeof(uuid));
    char label[VOLUME_LABEL_MAX_LEN];
    zero(label, sizeof(label));
    runtime_memcpy(label, tag_str.ptr, tag_str.len);
    if (!volume_add(uuid, label, v, init_closure_func(&v->fs_init, fs_init_handler, vfs_fs_init), -1)) {
        msg_err("virtiofs %s: failed to add volume", tag_str);
        goto err;
    }
    return true;
  err:
    deallocate(general, v, sizeof(*v));
    return false;
}

closure_function(2, 1, boolean, vtpci_fs_probe,
                 heap, general, backed_heap, backed,
                 pci_dev d)
{
    if (!vtpci_probe(d, VIRTIO_ID_FS))
        return false;
    vtdev v = (vtdev)attach_vtpci(bound(general), bound(backed), d, 0);
    return virtio_fs_dev_attach(bound(general), bound(backed), v);
}

closure_function(2, 1, void, vtmmio_fs_probe,
                 heap, general, backed_heap, backed,
                 vtmmio d)
{
    if ((vtmmio_get_u32(d, VTMMIO_OFFSET_DEVID) != VIRTIO_ID_FS) ||
        (d->memsize < VTMMIO_OFFSET_CONFIG + VIRTIO_FS_TAG_LEN + sizeof(u32)))
        return;
    if (attach_vtmmio(bound(general), bound(backed), d, 0))
        virtio_fs_dev_attach(bound(general), bound(backed), &d->virtio_dev);
}

void init_virtio_fs(kernel_heaps kh)
{
    heap h = heap_locked(kh);
    backed_heap backed = heap_linear_backed(kh);
    pci_probe probe = closure(h, vtpci_fs_probe, h, backed);
    assert(probe != INVALID_ADDRESS);
    register_pci_driver(probe, 0);
    vtmmio_probe_devs(stack_closure(vtmmio_fs_probe, h, backed));
}
