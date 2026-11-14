#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int target_limits(void)
{
    struct rlimit original, observed, old;
    if (getrlimit(RLIMIT_NOFILE, &original) || original.rlim_cur < 64)
        return 1;
    int ready[2], release[2];
    if (pipe(ready) || pipe(release))
        return 2;
    pid_t child = fork();
    if (child < 0)
        return 3;
    if (!child) {
        close(ready[0]);
        close(release[1]);
        struct rlimit limit = {64, original.rlim_max};
        char byte = !setrlimit(RLIMIT_NOFILE, &limit);
        if (write(ready[1], &byte, 1) != 1 || !byte || read(release[0], &byte, 1) != 1)
            _exit(4);
        _exit(getrlimit(RLIMIT_NOFILE, &observed) || observed.rlim_cur != 32 ? 5 : 0);
    }
    close(ready[1]);
    close(release[0]);
    char byte;
    int ok = read(ready[0], &byte, 1) == 1 && byte;
    ok &= !prlimit(child, RLIMIT_NOFILE, NULL, &observed) && observed.rlim_cur == 64;
    struct rlimit limit = {32, original.rlim_max};
    ok &= !prlimit(child, RLIMIT_NOFILE, &limit, &old) && old.rlim_cur == 64;
    ok &= !getrlimit(RLIMIT_NOFILE, &observed) && observed.rlim_cur == original.rlim_cur &&
        observed.rlim_max == original.rlim_max;
    ok &= write(release[1], "x", 1) == 1;
    close(release[1]);
    close(ready[0]);
    int status;
    ok &= waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    errno = 0;
    ok &= prlimit(child, RLIMIT_NOFILE, NULL, &observed) == -1 && errno == ESRCH;
    return ok ? 0 : 6;
}

static int data_limits(void)
{
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    void *writable = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    void *readonly = mmap(NULL, page, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (writable == MAP_FAILED || readonly == MAP_FAILED)
        return 1;
    struct rlimit original;
    if (getrlimit(RLIMIT_DATA, &original))
        return 2;
    *(volatile char *)writable = 42;
    struct rlimit zero = {0, original.rlim_max};
    if (setrlimit(RLIMIT_DATA, &zero))
        return 3;
    void *soft_zero = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (soft_zero == MAP_FAILED || munmap(soft_zero, page))
        return 11;
    zero.rlim_max = 0;
    if (setrlimit(RLIMIT_DATA, &zero))
        return 12;
    errno = 0;
    if (mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) != MAP_FAILED || errno != ENOMEM)
        return 4;
    errno = 0;
    if (mprotect(readonly, page, PROT_READ | PROT_WRITE) != -1 || errno != ENOMEM)
        return 5;
    errno = 0;
    if (mremap(writable, page, page * 2, MREMAP_MAYMOVE) != MAP_FAILED || errno != ENOMEM)
        return 6;
    void *read_map = mmap(NULL, page, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    void *shared = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (read_map == MAP_FAILED || shared == MAP_FAILED)
        return 7;
    *(volatile char *)shared = 42;
    if (mprotect(writable, page, PROT_READ) || munmap(read_map, page) || munmap(shared, page))
        return 8;
    return munmap(writable, page) || munmap(readonly, page) ? 10 : 0;
}

static int stack_limits(void)
{
    struct rlimit original, observed;
    if (getrlimit(RLIMIT_STACK, &original))
        return 1;
    struct rlimit limit = {1024 * 1024, 2 * 1024 * 1024};
    if (setrlimit(RLIMIT_STACK, &limit) || getrlimit(RLIMIT_STACK, &observed) ||
        observed.rlim_cur != limit.rlim_cur || observed.rlim_max != limit.rlim_max)
        return 2;
    struct rlimit inverted = {limit.rlim_max + 1, limit.rlim_max};
    errno = 0;
    if (setrlimit(RLIMIT_STACK, &inverted) != -1 || errno != EINVAL)
        return 3;
    errno = 0;
    if (syscall(SYS_prlimit64, 0, -1, NULL, NULL) != -1 || errno != EINVAL)
        return 4;
    errno = 0;
    if (syscall(SYS_prlimit64, 0, -1, &limit, NULL) != -1 || errno != EINVAL)
        return 5;
    errno = 0;
    return syscall(SYS_prlimit64, 0, RLIMIT_STACK, (void *)1, NULL) == -1 && errno == EFAULT ? 0 : 5;
}

static int mapping_capacity(void)
{
    const size_t size = 20 * 1024 * 1024;
    struct rlimit limit;
    if (getrlimit(RLIMIT_DATA, &limit))
        return 1;
    limit.rlim_cur = 32 * 1024 * 1024;
    if (setrlimit(RLIMIT_DATA, &limit))
        return 2;
    void *first = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (first == MAP_FAILED)
        return 3;
    errno = 0;
    if (mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) != MAP_FAILED || errno != ENOMEM)
        return 4;
    if (mmap(first, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) != first)
        return 5;
    if (mprotect(first, size, PROT_READ))
        return 6;
    void *second = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (second == MAP_FAILED)
        return 7;
    errno = 0;
    if (mprotect(first, size, PROT_READ | PROT_WRITE) != -1 || errno != ENOMEM)
        return 8;
    if (munmap(second, size) || mprotect(first, size, PROT_READ | PROT_WRITE))
        return 9;
    return munmap(first, size) ? 10 : 0;
}

static int unmodeled_limits(void)
{
    const int resources[] = {RLIMIT_AS, RLIMIT_CPU, RLIMIT_FSIZE};
    for (unsigned i = 0; i < sizeof(resources) / sizeof(resources[0]); i++) {
        struct rlimit original, observed, limit = {1, 1};
        errno = 0;
        if (getrlimit(resources[i], &original)) {
            if (errno != EINVAL)
                return 1;
            errno = 0;
            if (setrlimit(resources[i], &limit) != -1 || errno != EINVAL)
                return 2;
        } else {
            limit = original;
            limit.rlim_cur = resources[i] == RLIMIT_AS ? (1ULL << 40) : 1;
            if (limit.rlim_cur > original.rlim_max)
                return 3;
            if (setrlimit(resources[i], &limit) || getrlimit(resources[i], &observed) ||
                observed.rlim_cur != limit.rlim_cur || observed.rlim_max != limit.rlim_max ||
                setrlimit(resources[i], &original))
                return 4;
        }
    }
    return 0;
}

static int run(const char *name, int (*check)(void))
{
    pid_t child = fork();
    if (!child)
        _exit(check());
    int status = -1;
    int ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    printf("MINI_CHROMIUM_TEST name=%s status=%s detail=status=0x%x\n", name, ok ? "PASS" : "FAIL", status);
    return !ok;
}

int main(void)
{
    alarm(30);
    int failed = run("rlimit_target", target_limits);
    failed += run("rlimit_data_mappings", data_limits);
    failed += run("rlimit_stack_and_errors", stack_limits);
    failed += run("rlimit_mapping_capacity", mapping_capacity);
    failed += run("rlimit_unmodeled_rejected", unmodeled_limits);
    printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", 5 - failed, failed);
    return !!failed;
}
