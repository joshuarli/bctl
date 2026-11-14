#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

struct sock_filter {
    uint16_t code;
    uint8_t jt, jf;
    uint32_t k;
};
struct sock_fprog {
    unsigned short len;
    struct sock_filter *filter;
};
enum {
    BPF_LD_W_ABS = 0x20,
    BPF_JMP_JEQ_K = 0x15,
    BPF_RET_K = 0x06,
    SECCOMP_SET_MODE_STRICT = 0,
    SECCOMP_SET_MODE_FILTER = 1,
    SECCOMP_FILTER_FLAG_TSYNC = 1,
    SECCOMP_RET_ERRNO = 0x00050000,
    SECCOMP_RET_ALLOW = 0x7fff0000,
};

static const long denied[] = {SYS_getppid, SYS_getuid, SYS_geteuid, SYS_getgid, SYS_getegid, SYS_getsid};
#define NDENIED ((int)(sizeof denied / sizeof denied[0]))
#define ERRNO_BASE 100

static long install(long nr, int err, unsigned flags)
{
    struct sock_filter filter[] = {
        {BPF_LD_W_ABS, 0, 0, 0},
        {BPF_JMP_JEQ_K, 0, 1, (uint32_t)nr},
        {BPF_RET_K, 0, 0, SECCOMP_RET_ERRNO | (uint32_t)err},
        {BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW},
    };
    struct sock_fprog program = {.len = sizeof filter / sizeof filter[0], .filter = filter};
    return syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, flags, &program);
}

static int is_denied(long nr, int err)
{
    errno = 0;
    return syscall(nr, 0) == -1 && errno == err;
}

static int is_allowed(long nr)
{
    errno = 0;
    return syscall(nr, 0) != -1;
}

static int thread_seccomp_mode(long tid)
{
    char path[64], line[256];
    snprintf(path, sizeof path, "/proc/self/task/%ld/status", tid);
    FILE *f = fopen(path, "r");
    if (!f)
        return -1;
    int mode = -1;
    while (fgets(line, sizeof line, f))
        if (sscanf(line, "Seccomp:\t%d", &mode) == 1)
            break;
    fclose(f);
    return mode;
}

static void wait_byte(int fd)
{
    char c;
    while (read(fd, &c, 1) < 0 && errno == EINTR)
        ;
}

static void send_byte(int fd)
{
    while (write(fd, "x", 1) != 1)
        ;
}

static struct {
    int go[2], ready[2], done[2];
    atomic_long tid;
    atomic_int own_filter;
    atomic_int result;
} sib;

static void sib_init(int own_filter)
{
    if (pipe(sib.go) || pipe(sib.ready) || pipe(sib.done))
        _exit(90);
    atomic_store(&sib.tid, 0);
    atomic_store(&sib.own_filter, own_filter);
    atomic_store(&sib.result, 0);
}

static void *parked_sibling(void *arg)
{
    (void)arg;
    atomic_store(&sib.tid, syscall(SYS_gettid));
    if (atomic_load(&sib.own_filter) && install(SYS_getgid, ERRNO_BASE + 3, 0))
        return (void *)1;
    send_byte(sib.ready[1]);
    wait_byte(sib.go[0]);
    int result = 0;
    result |= is_denied(SYS_getppid, ERRNO_BASE) ? 1 : 0;
    result |= is_denied(SYS_getuid, ERRNO_BASE + 1) ? 2 : 0;
    result |= is_allowed(SYS_getppid) ? 4 : 0;
    result |= is_denied(SYS_getgid, ERRNO_BASE + 3) ? 8 : 0;
    atomic_store(&sib.result, result);
    send_byte(sib.done[1]);
    return 0;
}

static void *strict_sibling(void *arg)
{
    (void)arg;
    atomic_store(&sib.tid, syscall(SYS_gettid));
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_STRICT, 0, 0))
        return (void *)1;
    syscall(SYS_write, sib.ready[1], "x", 1);
    char c;
    while (syscall(SYS_read, sib.go[0], &c, 1) != 1)
        ;
    syscall(SYS_write, sib.done[1], "x", 1);
    syscall(SYS_exit, 0);
    return 0;
}

static int start_sibling(void *(*fn)(void *), pthread_t *th)
{
    if (pthread_create(th, 0, fn, 0))
        return 1;
    wait_byte(sib.ready[0]);
    return 0;
}

static int check_strict_sibling(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return 1;
    sib_init(0);
    pthread_t th;
    if (start_sibling(strict_sibling, &th))
        return 2;
    long tid = atomic_load(&sib.tid);
    errno = 0;
    long rv = install(SYS_getppid, ERRNO_BASE, SECCOMP_FILTER_FLAG_TSYNC);
    if (rv != tid)
        return 3;
    if (thread_seccomp_mode(tid) != 1)
        return 4;
    if (thread_seccomp_mode(syscall(SYS_gettid)) != 0)
        return 5;
    if (!is_allowed(SYS_getppid))
        return 6;
    send_byte(sib.go[1]);
    wait_byte(sib.done[0]);
    return 0;
}

static int check_unfiltered_sibling(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return 1;
    sib_init(0);
    pthread_t th;
    if (start_sibling(parked_sibling, &th))
        return 2;
    if (install(SYS_getppid, ERRNO_BASE, 0))
        return 3;
    errno = 0;
    if (install(SYS_getuid, ERRNO_BASE + 1, SECCOMP_FILTER_FLAG_TSYNC) != 0)
        return 4;
    if (thread_seccomp_mode(atomic_load(&sib.tid)) != 2)
        return 5;
    send_byte(sib.go[1]);
    wait_byte(sib.done[0]);
    pthread_join(th, 0);
    int result = atomic_load(&sib.result);
    if (!(result & 1))
        return 6;
    if (!(result & 2))
        return 7;
    if (result & 4)
        return 8;
    if (!is_denied(SYS_getppid, ERRNO_BASE) || !is_denied(SYS_getuid, ERRNO_BASE + 1))
        return 9;
    return 0;
}

static int check_diverged_sibling(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return 1;
    sib_init(1);
    pthread_t th;
    if (start_sibling(parked_sibling, &th))
        return 2;
    long tid = atomic_load(&sib.tid);
    errno = 0;
    long rv = install(SYS_getppid, ERRNO_BASE, SECCOMP_FILTER_FLAG_TSYNC);
    if (rv != tid)
        return 3;
    if (!is_allowed(SYS_getppid))
        return 4;
    send_byte(sib.go[1]);
    wait_byte(sib.done[0]);
    pthread_join(th, 0);
    int result = atomic_load(&sib.result);
    if (result & 1)
        return 5;
    if (!(result & 4))
        return 6;
    if (!(result & 8))
        return 7;
    return 0;
}

#define STRESS_FILTERS 600
#define STRESS_THREADS 96

static struct {
    int release[2];
    atomic_int started;
    atomic_int finished;
    atomic_int spawned;
    atomic_int bad;
    pthread_t workers[STRESS_THREADS];
} stress;

static int all_denied(void)
{
    for (int i = 0; i < NDENIED; i++)
        if (!is_denied(denied[i], ERRNO_BASE + i))
            return 0;
    return 1;
}

static void *stress_worker(void *arg)
{
    (void)arg;
    wait_byte(stress.release[0]);
    if (!all_denied())
        atomic_fetch_add(&stress.bad, 1);
    return 0;
}

static void *stress_spawner(void *arg)
{
    (void)arg;
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 65536);
    atomic_store(&stress.started, 1);
    for (int i = 0; i < STRESS_THREADS; i++) {
        if (pthread_create(&stress.workers[i], &attr, stress_worker, 0)) {
            atomic_store(&stress.finished, 1);
            return (void *)1;
        }
        atomic_fetch_add(&stress.spawned, 1);
    }
    atomic_store(&stress.finished, 1);
    return 0;
}

static int check_create_during_tsync(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) || pipe(stress.release))
        return 1;
    pthread_t spawner;
    if (pthread_create(&spawner, 0, stress_spawner, 0))
        return 2;
    while (!atomic_load(&stress.started))
        ;
    int failed = 0;
    for (int i = 0; i < STRESS_FILTERS && (i < NDENIED || !atomic_load(&stress.finished)); i++) {
        long rv = install(denied[i % NDENIED], ERRNO_BASE + i % NDENIED, SECCOMP_FILTER_FLAG_TSYNC);
        if (rv != 0)
            failed++;
    }
    void *spawn_result;
    if (pthread_join(spawner, &spawn_result) || spawn_result)
        return 3;
    if (failed)
        return 4;
    close(stress.release[1]);
    int rc = 0;
    for (int i = 0; i < atomic_load(&stress.spawned); i++)
        pthread_join(stress.workers[i], 0);
    if (atomic_load(&stress.bad))
        rc = 5;
    if (!all_denied())
        rc = 6;
    return rc;
}

static struct {
    atomic_int arrived;
    atomic_int finish;
    atomic_long tid;
    atomic_long install_rv;
    atomic_int own_deny_ok;
    atomic_int sync_deny_ok;
    atomic_int sync_allowed;
} race;

static void *racing_installer(void *arg)
{
    (void)arg;
    atomic_store(&race.tid, syscall(SYS_gettid));
    atomic_fetch_add(&race.arrived, 1);
    while (atomic_load(&race.arrived) < 2)
        ;
    atomic_store(&race.install_rv, install(SYS_getuid, ERRNO_BASE + 1, 0));
    while (!atomic_load(&race.finish))
        ;
    atomic_store(&race.own_deny_ok, is_denied(SYS_getuid, ERRNO_BASE + 1));
    atomic_store(&race.sync_deny_ok, is_denied(SYS_getgid, ERRNO_BASE + 3));
    atomic_store(&race.sync_allowed, is_allowed(SYS_getgid));
    return 0;
}

static int race_round(unsigned round)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return 1;
    pthread_t th;
    if (pthread_create(&th, 0, racing_installer, 0))
        return 2;
    atomic_fetch_add(&race.arrived, 1);
    while (atomic_load(&race.arrived) < 2)
        ;
    for (volatile unsigned j = 0; j < (round * 37) % 3000; j++)
        ;
    long rv = install(SYS_getgid, ERRNO_BASE + 3, SECCOMP_FILTER_FLAG_TSYNC);
    atomic_store(&race.finish, 1);
    pthread_join(th, 0);
    if (atomic_load(&race.install_rv) != 0)
        return 3;
    if (!atomic_load(&race.own_deny_ok))
        return 4;
    if (rv == 0)
        return atomic_load(&race.sync_deny_ok) ? 0 : 5;
    if (rv != atomic_load(&race.tid))
        return 6;
    return atomic_load(&race.sync_allowed) && is_allowed(SYS_getgid) ? 0 : 7;
}

#define RACE_ROUNDS 150

static int check_install_during_tsync(void)
{
    for (unsigned i = 0; i < RACE_ROUNDS; i++) {
        pid_t child = fork();
        if (child < 0)
            return 1;
        if (child == 0)
            _exit(race_round(i));
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status))
            return 2;
        if (WEXITSTATUS(status))
            return 100 + WEXITSTATUS(status);
    }
    return 0;
}

int main(void)
{
    alarm(100);
    static const struct {
        const char *name;
        int (*run)(void);
    } checks[] = {
        {"tsync_strict_sibling", check_strict_sibling},
        {"tsync_unfiltered_sibling", check_unfiltered_sibling},
        {"tsync_diverged_sibling", check_diverged_sibling},
        {"tsync_create_during_install", check_create_during_tsync},
        {"tsync_install_during_install", check_install_during_tsync},
    };
    unsigned total = sizeof checks / sizeof checks[0], failed = 0;
    for (unsigned i = 0; i < total; i++) {
        pid_t child = fork();
        if (child == 0)
            _exit(checks[i].run());
        int status = 0;
        int ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
        failed += !ok;
        printf("MINI_CHROMIUM_TEST name=%s status=%s detail=wait_status=%d step=%d\n", checks[i].name,
            ok ? "PASS" : "FAIL", status, WIFEXITED(status) ? WEXITSTATUS(status) : -1);
        fflush(stdout);
    }
    printf("MINI_CHROMIUM_RESULT status=%s passed=%u failed=%u\n", failed ? "FAIL" : "PASS", total - failed, failed);
    return failed != 0;
}
