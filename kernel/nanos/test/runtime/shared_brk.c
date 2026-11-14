#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static struct {
    _Atomic int phase;
    _Atomic int done;
    _Atomic int stop;
    _Atomic int error;
    uintptr_t request[2];
} shared;
static pid_t owner;

static int worker(void *argument)
{
    unsigned index = (uintptr_t)argument;
    if (prctl(PR_SET_PDEATHSIG, SIGKILL) || getppid() != owner)
        return 1;
    int previous = 0;
    for (;;) {
        int phase;
        while ((phase = atomic_load(&shared.phase)) == previous)
            syscall(SYS_futex, &shared.phase, 0, previous, NULL, NULL, 0);
        if (atomic_load(&shared.stop))
            return 0;
        previous = phase;
        if ((uintptr_t)syscall(SYS_brk, shared.request[index]) != shared.request[index])
            atomic_store(&shared.error, 1);
        atomic_fetch_add(&shared.done, 1);
        syscall(SYS_futex, &shared.done, 1, INT_MAX, NULL, NULL, 0);
    }
}

int main(void)
{
    alarm(45);
    owner = getpid();
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    uintptr_t original = (uintptr_t)syscall(SYS_brk, NULL);
    uintptr_t base = (original + page - 1) & ~(page - 1);
    const size_t stack_size = 64 * 1024;
    void *stacks[2];
    pid_t children[2] = {-1, -1};
    unsigned created = 0, iteration = 0;
    int ok = 1;
    for (unsigned i = 0; i < 2; i++) {
        stacks[i] = mmap(NULL, stack_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (stacks[i] == MAP_FAILED) {
            ok = 0;
            break;
        }
        shared.request[i] = base + (i ? 3 : 1) * page;
        children[i] = clone(worker, (char *)stacks[i] + stack_size, CLONE_VM | SIGCHLD, (void *)(uintptr_t)i);
        if (children[i] < 0) {
            munmap(stacks[i], stack_size);
            ok = 0;
            break;
        }
        created++;
    }
    int mismatch = -1;
    uintptr_t final = original;
    for (; ok && iteration < 2000; iteration++) {
        if ((uintptr_t)syscall(SYS_brk, base + 2 * page) != base + 2 * page) {
            ok = 0;
            break;
        }
        atomic_store(&shared.done, 0);
        atomic_store(&shared.phase, (int)iteration + 1);
        syscall(SYS_futex, &shared.phase, 1, INT_MAX, NULL, NULL, 0);
        int done;
        while ((done = atomic_load(&shared.done)) < 2)
            syscall(SYS_futex, &shared.done, 0, done, NULL, NULL, 0);
        final = (uintptr_t)syscall(SYS_brk, NULL);
        ok &= !atomic_load(&shared.error) && (final == base + page || final == base + 3 * page);
        for (unsigned i = 0; ok && i < 3; i++) {
            unsigned char resident;
            errno = 0;
            int result = mincore((void *)(base + i * page), page, &resident);
            int mapped = result == 0;
            if ((result && errno != ENOMEM) || mapped != (base + i * page < final)) {
                mismatch = (int)i;
                ok = 0;
            }
        }
    }
    atomic_store(&shared.stop, 1);
    atomic_fetch_add(&shared.phase, 1);
    syscall(SYS_futex, &shared.phase, 1, INT_MAX, NULL, NULL, 0);
    for (unsigned i = 0; i < created; i++) {
        int status;
        ok &= waitpid(children[i], &status, 0) == children[i] && WIFEXITED(status) && WEXITSTATUS(status) == 0;
        munmap(stacks[i], stack_size);
    }
    syscall(SYS_brk, original);
    char record[384];
    int length = snprintf(record, sizeof record,
        "MINI_CHROMIUM_TEST name=shared_brk_ranges status=%s detail=iteration=%u,mismatch=%d,break=%lx,base=%lx\n"
        "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n",
        ok ? "PASS" : "FAIL", iteration, mismatch, (unsigned long)final, (unsigned long)base,
        ok ? "PASS" : "FAIL", ok, !ok);
    if (length < 0 || (size_t)length >= sizeof record || write(STDOUT_FILENO, record, (size_t)length) != length)
        return 1;
    return !ok;
}
