#include <unix_internal.h>
#include <filesystem.h>
#include <tmpfs.h>

#define MEMFD_KNOWN_FLAGS   \
    (MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_NOEXEC_SEAL | MFD_EXEC)

#define MEMFD_NAME_MAX  249

static struct {
    filesystem fs;
} shmem;

sysreturn memfd_create(const char *name, unsigned int flags)
{
    if ((flags & ~MEMFD_KNOWN_FLAGS) || ((flags & MFD_NOEXEC_SEAL) && (flags & MFD_EXEC)))
        return -EINVAL;
    sstring n;
    if (!fault_in_user_string(name, &n))
        return -EFAULT;
    if (n.len > MEMFD_NAME_MAX)
        return -EINVAL;
    u64 seals = (flags & MFD_NOEXEC_SEAL) ? F_SEAL_EXEC :
                (flags & MFD_ALLOW_SEALING) ? 0 : F_SEAL_SEAL;
    filesystem fs = shmem.fs;
    fsfile fsf;
    filesystem_lock(fs);
    int fss = filesystem_creat_unnamed(fs, &fsf);
    if (fss == 0) {
        if (seals)
            fss = fs->set_seals(fs, fsf, seals);
    } else {
        fsf = 0;
    }
    sysreturn rv;
    if (fss == 0)
        rv = unix_file_new(fs, fs->root, FDESC_TYPE_REGULAR,
                           O_RDWR | O_TMPFILE | ((flags & MFD_CLOEXEC) ? O_CLOEXEC : 0), fsf);
    else
        rv = fss;
    filesystem_unlock(fs);
    if ((rv < 0) && fsf)
        fsfile_release(fsf);
    return rv;
}

int init(status_handler complete)
{
    shmem.fs = tmpfs_new();
    if (shmem.fs == INVALID_ADDRESS) {
        msg_err("shmem: failed to create tmpfs");
        return KLIB_INIT_FAILED;
    }
    swap_syscall_handler(linux_syscalls, SYS_memfd_create, memfd_create);
    return KLIB_INIT_OK;
}
