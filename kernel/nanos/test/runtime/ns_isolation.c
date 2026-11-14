#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define OWNER_UID 1000
#define OTHER_UID 2000

#define EXPECT(code, cond) do { if (!(cond)) return (code); } while (0)

static int write_text(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY);
    if (fd < 0)
        return -1;
    size_t len = strlen(text);
    int ok = write(fd, text, len) == (ssize_t)len;
    close(fd);
    return ok ? 0 : -1;
}

static int drop_to(uid_t id)
{
    if (setgroups(0, NULL) || setresgid(id, id, id) || setresuid(id, id, id))
        return -1;
    return prctl(PR_SET_DUMPABLE, 1, 0, 0, 0);
}

static int map_self(uid_t id)
{
    char line[32];
    snprintf(line, sizeof(line), "0 %u 1", id);
    if (write_text("/proc/self/setgroups", "deny") || write_text("/proc/self/uid_map", line) ||
        write_text("/proc/self/gid_map", line))
        return -1;
    return prctl(PR_SET_DUMPABLE, 1, 0, 0, 0);
}

struct holder {
    pid_t pid;
    int release;
    int value;
};

static int release_holder(struct holder *h)
{
    int status;
    char byte = 'x';
    int ok = write(h->release, &byte, 1) == 1;
    close(h->release);
    ok &= waitpid(h->pid, &status, 0) == h->pid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    return ok ? 0 : -1;
}

enum holder_mode { HOLD_PLAIN, HOLD_FD, HOLD_USERNS, HOLD_NESTED_USERNS };

static int start_holder(struct holder *h, enum holder_mode mode, uid_t id)
{
    int ready[2], release[2];
    if (pipe(ready) || pipe(release))
        return -1;
    pid_t child = fork();
    if (child < 0)
        return -1;
    if (!child) {
        close(ready[0]);
        close(release[1]);
        int fd = -1;
        if (id != (uid_t)-1 && drop_to(id))
            _exit(1);
        if (mode == HOLD_USERNS || mode == HOLD_NESTED_USERNS) {
            if (unshare(CLONE_NEWUSER) || prctl(PR_SET_DUMPABLE, 1, 0, 0, 0))
                _exit(2);
        }
        if (mode == HOLD_NESTED_USERNS) {
            if (map_self(id) || unshare(CLONE_NEWUSER) || prctl(PR_SET_DUMPABLE, 1, 0, 0, 0))
                _exit(3);
        }
        if (mode == HOLD_FD) {
            fd = open("/", O_RDONLY | O_DIRECTORY);
            if (fd < 0)
                _exit(4);
        }
        char byte = 'r';
        if (write(ready[1], &fd, sizeof(fd)) != sizeof(fd) || read(release[0], &byte, 1) != 1)
            _exit(5);
        _exit(0);
    }
    close(ready[1]);
    close(release[0]);
    h->pid = child;
    h->release = release[1];
    int rc = read(ready[0], &h->value, sizeof(h->value)) == sizeof(h->value) ? 0 : -1;
    close(ready[0]);
    return rc;
}

static int step(int base, int inner)
{
    return inner ? base + (inner < 0 ? 15 : inner) : 0;
}

static int run_as(int (*check)(const struct holder *), const struct holder *h)
{
    pid_t child = fork();
    if (child < 0)
        return -1;
    if (!child)
        _exit(check(h));
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status))
        return -1;
    return WEXITSTATUS(status);
}

static int kill_as_root(const struct holder *h)
{
    return kill(h->pid, 0) ? 1 : 0;
}

static int kill_from_user_ns(const struct holder *h)
{
    EXPECT(1, !drop_to(OWNER_UID));
    EXPECT(2, !unshare(CLONE_NEWUSER));
    errno = 0;
    EXPECT(3, kill(h->pid, 0) == -1 && errno == EPERM);
    return 0;
}

static int kill_from_host_user(const struct holder *h)
{
    EXPECT(1, !drop_to(OWNER_UID));
    errno = 0;
    EXPECT(2, kill(h->pid, 0) == -1 && errno == EPERM);
    return 0;
}

static int kill_user_ns_scope(void)
{
    struct holder plain, nested;
    EXPECT(1, !start_holder(&plain, HOLD_PLAIN, OTHER_UID));
    int rc = 0;
    rc = rc ? rc : step(0x20, run_as(kill_as_root, &plain));
    rc = rc ? rc : step(0x30, run_as(kill_from_host_user, &plain));
    rc = rc ? rc : step(0x40, run_as(kill_from_user_ns, &plain));
    EXPECT(5, !release_holder(&plain));
    EXPECT(6, !start_holder(&nested, HOLD_USERNS, OTHER_UID));
    rc = rc ? rc : step(0x70, run_as(kill_as_root, &nested));
    EXPECT(8, !release_holder(&nested));
    return rc;
}

static int open_setgroups_from_sibling(const struct holder *h)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/setgroups", h->pid);
    EXPECT(1, !drop_to(OWNER_UID));
    EXPECT(2, !unshare(CLONE_NEWUSER));
    errno = 0;
    EXPECT(3, open(path, O_WRONLY) == -1 && errno == EACCES);
    return 0;
}

static int open_setgroups_from_ancestor(const struct holder *h)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/setgroups", h->pid);
    EXPECT(1, !drop_to(OWNER_UID));
    int fd = open(path, O_WRONLY);
    EXPECT(2, fd >= 0);
    close(fd);
    return 0;
}

static int owner_needs_ancestry(void)
{
    struct holder nested;
    EXPECT(1, !start_holder(&nested, HOLD_NESTED_USERNS, OWNER_UID));
    int rc = 0;
    rc = rc ? rc : step(0x20, run_as(open_setgroups_from_ancestor, &nested));
    rc = rc ? rc : step(0x30, run_as(open_setgroups_from_sibling, &nested));
    EXPECT(4, !release_holder(&nested));
    return rc;
}

static int open_fd_of(const struct holder *h)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/fd/%d", h->pid, h->value);
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return 1;
    close(fd);
    return 0;
}

static int open_fd_as_root(const struct holder *h)
{
    return open_fd_of(h);
}

static int open_fd_from_user_ns(const struct holder *h)
{
    EXPECT(1, !drop_to(OWNER_UID));
    EXPECT(2, !unshare(CLONE_NEWUSER));
    errno = 0;
    EXPECT(3, open_fd_of(h) && errno == EACCES);
    return 0;
}

static int open_fd_in_own_user_ns(const struct holder *unused)
{
    (void)unused;
    EXPECT(1, !drop_to(OWNER_UID));
    EXPECT(2, !unshare(CLONE_NEWUSER));
    struct holder peer;
    EXPECT(3, !start_holder(&peer, HOLD_FD, (uid_t)-1));
    int rc = open_fd_of(&peer);
    EXPECT(4, !release_holder(&peer));
    return rc ? 5 : 0;
}

static int proc_fd_user_ns(void)
{
    struct holder target;
    EXPECT(1, !start_holder(&target, HOLD_FD, OTHER_UID));
    int rc = 0;
    rc = rc ? rc : step(0x20, run_as(open_fd_as_root, &target));
    rc = rc ? rc : step(0x30, run_as(open_fd_from_user_ns, &target));
    rc = rc ? rc : step(0x40, run_as(open_fd_in_own_user_ns, &target));
    EXPECT(5, !release_holder(&target));
    return rc;
}

static int capget_sets(uint32_t *effective, uint32_t *permitted)
{
    struct { uint32_t version; int pid; } header = {0x20080522, 0};
    struct { uint32_t effective, permitted, inheritable; } data[2];
    if (syscall(SYS_capget, &header, data))
        return -1;
    effective[0] = data[0].effective;
    effective[1] = data[1].effective;
    permitted[0] = data[0].permitted;
    permitted[1] = data[1].permitted;
    return 0;
}

static int raise_effective_as_mapper(void)
{
    struct { uint32_t version; int pid; } header = {0x20080522, 0};
    struct { uint32_t effective, permitted, inheritable; } data[2];
    if (setresuid(0, OWNER_UID, 0) || syscall(SYS_capget, &header, data))
        return -1;
    data[0].effective = data[0].permitted;
    data[1].effective = data[1].permitted;
    return syscall(SYS_capset, &header, data);
}

static int mapped_root_drop(int drop_all)
{
    int ready[2], mapped[2];
    if (pipe(ready) || pipe(mapped))
        return 1;
    pid_t child = fork();
    if (child < 0)
        return 2;
    if (!child) {
        char byte = 'r';
        if (drop_to(OWNER_UID) || unshare(CLONE_NEWUSER) || write(ready[1], &byte, 1) != 1 ||
            read(mapped[0], &byte, 1) != 1)
            _exit(1);
        uint32_t eff[2], perm[2];
        if (getuid() != 0 || capget_sets(eff, perm) || !eff[0] || !perm[0])
            _exit(2);
        if (setresuid(drop_all ? 1 : (uid_t)-1, 1, drop_all ? 1 : (uid_t)-1))
            _exit(3);
        if (capget_sets(eff, perm))
            _exit(4);
        if (eff[0] || eff[1])
            _exit(5);
        if (drop_all ? (perm[0] || perm[1]) : !perm[0])
            _exit(6);
        _exit(0);
    }
    char byte;
    char path[48], map[64];
    int ok = read(ready[0], &byte, 1) == 1;
    if (!ok || raise_effective_as_mapper())
        return 3;
    snprintf(path, sizeof(path), "/proc/%d/uid_map", child);
    snprintf(map, sizeof(map), "0 %u 1\n1 %u 1\n", OWNER_UID, OTHER_UID);
    if (write_text(path, map))
        return 4;
    ok = write(mapped[1], &byte, 1) == 1;
    int status;
    ok &= waitpid(child, &status, 0) == child && WIFEXITED(status);
    if (!ok)
        return 5;
    return WEXITSTATUS(status) ? 10 + WEXITSTATUS(status) : 0;
}

static int mapped_root_euid_drop(void)
{
    return mapped_root_drop(0);
}

static int mapped_root_full_drop(void)
{
    return mapped_root_drop(1);
}

static volatile sig_atomic_t usr1_seen;

static void on_usr1(int sig)
{
    (void)sig;
    usr1_seen = 1;
}

static int pid_ns_group_view(void)
{
    if (setpgid(0, 0))
        return 1;
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_usr1;
    sa.sa_flags = SA_RESTART;
    if (sigaction(SIGUSR1, &sa, NULL))
        return 2;
    int report[2];
    if (pipe(report))
        return 3;
    pid_t outer = fork();
    if (outer < 0)
        return 4;
    if (!outer) {
        if (unshare(CLONE_NEWUSER | CLONE_NEWPID))
            _exit(1);
        pid_t inner = fork();
        if (!inner) {
            char out = 0;
            if (getpgid(0) != 0)
                out = 'g';
            else if (kill(0, SIGUSR1))
                out = 'k';
            else
                out = 'o';
            _exit(write(report[1], &out, 1) == 1 ? 0 : 2);
        }
        int status;
        _exit(waitpid(inner, &status, 0) == inner && WIFEXITED(status) && !WEXITSTATUS(status) ? 0 : 3);
    }
    int status;
    char out = 0;
    if (waitpid(outer, &status, 0) != outer || !WIFEXITED(status) || WEXITSTATUS(status))
        return 5;
    if (read(report[0], &out, 1) != 1 || out != 'o')
        return 6;
    return usr1_seen ? 0 : 7;
}

static int run(const char *name, int (*check)(void), int *total)
{
    (*total)++;
    pid_t child = fork();
    if (!child)
        _exit(check());
    int status = -1;
    int ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    printf("MINI_CHROMIUM_TEST name=%s status=%s detail=status=0x%x\n", name, ok ? "PASS" : "FAIL", status);
    fflush(stdout);
    return !ok;
}

int main(void)
{
    alarm(60);
    if (geteuid() != 0) {
        printf("MINI_CHROMIUM_TEST name=ns_isolation_root status=FAIL detail=requires_root_to_write_uid_maps\n");
        printf("MINI_CHROMIUM_RESULT status=FAIL passed=0 failed=1\n");
        return 1;
    }
    int total = 0;
    int failed = run("ns_kill_user_ns_scope", kill_user_ns_scope, &total);
    failed += run("ns_owner_requires_ancestry", owner_needs_ancestry, &total);
    failed += run("ns_proc_fd_user_ns", proc_fd_user_ns, &total);
    failed += run("ns_mapped_root_euid_drop", mapped_root_euid_drop, &total);
    failed += run("ns_mapped_root_full_drop", mapped_root_full_drop, &total);
    failed += run("ns_pid_ns_group_view", pid_ns_group_view, &total);
    printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", total - failed, failed);
    return !!failed;
}
