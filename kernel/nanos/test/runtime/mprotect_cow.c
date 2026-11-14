#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static const unsigned char blob[8192] __attribute__((aligned(4096))) = {0x41};
static const char *self;

static size_t page_len(void)
{
    long n = sysconf(_SC_PAGESIZE);
    return n > 0 ? (size_t)n : 4096;
}

static int fill_path(const char *path, unsigned char byte, size_t len)
{
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (fd < 0)
        return 1;
    size_t off = 0;
    while (off < len) {
        unsigned char buf[512];
        size_t chunk = len - off > sizeof buf ? sizeof buf : len - off;
        memset(buf, byte, chunk);
        ssize_t w = write(fd, buf, chunk);
        if (w != (ssize_t)chunk) {
            close(fd);
            return 2;
        }
        off += (size_t)w;
    }
    if (fsync(fd))
        return 3;
    close(fd);
    return 0;
}

static int private_ro_pread(void)
{
    size_t len = page_len();
    const char *path = "/tmp/mprotect-cow-ro";
    if (fill_path(path, 0x41, len))
        return 1;
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return 2;
    unsigned char *p = mmap(0, len, PROT_READ, MAP_PRIVATE, fd, 0);
    if (p == MAP_FAILED)
        return 3;
    volatile unsigned char first = p[0];
    if (first != 0x41)
        return 4;
    if (mprotect(p, len, PROT_READ | PROT_WRITE))
        return 5;
    p[0] = 0x58;
    unsigned char back = 0;
    if (pread(fd, &back, 1, 0) != 1)
        return 6;
    int rv = back != 0x41 ? 7 : 0;
    munmap(p, len);
    close(fd);
    unlink(path);
    return rv;
}

static int private_ro_remap(void)
{
    size_t len = page_len();
    const char *path = "/tmp/mprotect-cow-remap";
    if (fill_path(path, 0x41, len))
        return 1;
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return 2;
    unsigned char *p = mmap(0, len, PROT_READ, MAP_PRIVATE, fd, 0);
    if (p == MAP_FAILED)
        return 3;
    volatile unsigned char first = p[0];
    if (first != 0x41)
        return 4;
    if (mprotect(p, len, PROT_READ | PROT_WRITE))
        return 5;
    p[0] = 0x58;
    unsigned char *q = mmap(0, len, PROT_READ, MAP_PRIVATE, fd, 0);
    if (q == MAP_FAILED)
        return 6;
    int rv = q[0] != 0x41 ? 7 : 0;
    munmap(q, len);
    munmap(p, len);
    close(fd);
    unlink(path);
    return rv;
}

static int private_ro_child(void)
{
    size_t len = page_len();
    const char *path = "/tmp/mprotect-cow-child";
    if (fill_path(path, 0x41, len))
        return 1;
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return 2;
    unsigned char *p = mmap(0, len, PROT_READ, MAP_PRIVATE, fd, 0);
    if (p == MAP_FAILED)
        return 3;
    volatile unsigned char first = p[0];
    if (first != 0x41)
        return 4;
    if (mprotect(p, len, PROT_READ | PROT_WRITE))
        return 5;
    p[0] = 0x58;
    pid_t child = fork();
    if (child < 0)
        return 6;
    if (child == 0) {
        unsigned char *q = mmap(0, len, PROT_READ, MAP_PRIVATE, fd, 0);
        if (q == MAP_FAILED)
            _exit(7);
        _exit(q[0] != 0x41 ? 8 : 0);
    }
    int status = 0;
    int ok = waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int code = !ok ? 9 : 0;
    if (!ok && WIFEXITED(status))
        code = 10 + WEXITSTATUS(status);
    munmap(p, len);
    close(fd);
    unlink(path);
    return code;
}

static int private_rw_pread(void)
{
    size_t len = page_len();
    const char *path = "/tmp/mprotect-cow-rw";
    if (fill_path(path, 0x42, len))
        return 1;
    int fd = open(path, O_RDWR);
    if (fd < 0)
        return 2;
    unsigned char *p = mmap(0, len, PROT_READ, MAP_PRIVATE, fd, 0);
    if (p == MAP_FAILED)
        return 3;
    volatile unsigned char first = p[0];
    if (first != 0x42)
        return 4;
    if (mprotect(p, len, PROT_READ | PROT_WRITE))
        return 5;
    p[0] = 0x58;
    unsigned char back = 0;
    if (pread(fd, &back, 1, 0) != 1)
        return 6;
    int rv = back != 0x42 ? 7 : 0;
    munmap(p, len);
    close(fd);
    unlink(path);
    return rv;
}

static int shared_ro_denied(void)
{
    size_t len = page_len();
    const char *path = "/tmp/mprotect-cow-shared";
    if (fill_path(path, 0x43, len))
        return 1;
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return 2;
    unsigned char *p = mmap(0, len, PROT_READ, MAP_SHARED, fd, 0);
    if (p == MAP_FAILED)
        return 3;
    volatile unsigned char first = p[0];
    if (first != 0x43)
        return 4;
    errno = 0;
    if (mprotect(p, len, PROT_READ | PROT_WRITE) != -1 || errno != EACCES)
        return 5;
    unsigned char back = 0;
    if (pread(fd, &back, 1, 0) != 1)
        return 6;
    int rv = back != 0x43 ? 7 : 0;
    munmap(p, len);
    close(fd);
    unlink(path);
    return rv;
}

static int fork_isolation(void)
{
    size_t len = page_len();
    const char *path = "/tmp/mprotect-cow-fork";
    if (fill_path(path, 0x44, len))
        return 1;
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return 2;
    unsigned char *p = mmap(0, len, PROT_READ, MAP_PRIVATE, fd, 0);
    if (p == MAP_FAILED)
        return 3;
    volatile unsigned char first = p[0];
    if (first != 0x44)
        return 4;
    pid_t child = fork();
    if (child < 0)
        return 5;
    if (child == 0) {
        volatile unsigned char cfirst = p[0];
        if (cfirst != 0x44)
            _exit(6);
        if (mprotect(p, len, PROT_READ | PROT_WRITE))
            _exit(7);
        p[0] = 0x59;
        if (p[0] != 0x59)
            _exit(8);
        _exit(0);
    }
    int status = 0;
    int ok = waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    if (!ok) {
        int code = 10;
        if (WIFEXITED(status))
            code = 20 + WEXITSTATUS(status);
        munmap(p, len);
        close(fd);
        unlink(path);
        return code;
    }
    int rv = p[0] != 0x44 ? 11 : 0;
    unsigned char back = 0;
    if (pread(fd, &back, 1, 0) != 1)
        rv = 12;
    else if (back != 0x44)
        rv = 13;
    munmap(p, len);
    close(fd);
    unlink(path);
    return rv;
}

static int exec_wait(const char *path, const char *mode)
{
    pid_t child = fork();
    if (child < 0)
        return -1;
    if (child == 0) {
        execl(path, path, mode, (char *)0);
        _exit(127);
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status))
        return -1;
    return WEXITSTATUS(status);
}

static int copy_self(const char *dst)
{
    int in = open(self, O_RDONLY);
    if (in < 0)
        return 1;
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    if (out < 0) {
        close(in);
        return 2;
    }
    unsigned char buf[4096];
    ssize_t n;
    int rv = 0;
    while ((n = read(in, buf, sizeof buf)) > 0) {
        if (write(out, buf, (size_t)n) != n) {
            rv = 3;
            break;
        }
    }
    if (n < 0)
        rv = 4;
    if (!rv && fsync(out))
        rv = 5;
    close(in);
    close(out);
    return rv;
}

static int text_write(void)
{
    size_t len = page_len();
    volatile unsigned char *p = (volatile unsigned char *)blob;
    if (p[0] != 0x41)
        return 1;
    if (mprotect((void *)p, len, PROT_READ | PROT_WRITE))
        return 2;
    p[0] = 0x58;
    if (p[0] != 0x58)
        return 3;
    int fd = open(self, O_RDONLY);
    if (fd < 0)
        return 4;
    off_t size = lseek(fd, 0, SEEK_END);
    int rv = 0;
    for (off_t off = 0; off + (off_t)sizeof(blob) <= size && !rv; off += (off_t)len) {
        unsigned char page[4096];
        if (pread(fd, page, sizeof page, off) != (ssize_t)sizeof page)
            continue;
        size_t i = 1;
        while (i < sizeof page && !page[i])
            i++;
        if (page[0] == 0x58 && i == sizeof page)
            rv = 5;
    }
    close(fd);
    if (rv)
        return rv;
    int other = exec_wait(self, "--blob");
    return other ? 10 + other : 0;
}

static int exec_text(void)
{
    const char *copy = "/tmp/mprotect-cow-exe";
    int rv = copy_self(copy);
    if (rv)
        return rv;
    int code = exec_wait(copy, "--text");
    unlink(copy);
    return code ? 20 + code : 0;
}

static int run(const char *name, int (*check)(void))
{
    pid_t child = fork();
    if (!child)
        _exit(check());
    int status = -1;
    int ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) &&
        WEXITSTATUS(status) == 0;
    printf("MINI_CHROMIUM_TEST name=%s status=%s detail=status=0x%x\n", name, ok ? "PASS" : "FAIL",
        status);
    return !ok;
}

int main(int argc, char **argv)
{
    if (argc > 1 && !strcmp(argv[1], "--blob"))
        return ((volatile const unsigned char *)blob)[0] == 0x41 ? 0 : 1;
    self = argv[0];
    if (argc > 1 && !strcmp(argv[1], "--text"))
        return text_write();
    alarm(30);
    int failed = run("private_ro_pread", private_ro_pread);
    failed += run("private_ro_remap", private_ro_remap);
    failed += run("private_ro_child", private_ro_child);
    failed += run("private_rw_pread", private_rw_pread);
    failed += run("shared_ro_denied", shared_ro_denied);
    failed += run("fork_isolation", fork_isolation);
    failed += run("exec_text", exec_text);
    printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
        7 - failed, failed);
    return !!failed;
}
