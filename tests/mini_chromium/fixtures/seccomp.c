#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>
enum { CHECK_DEADLINE_SECONDS = 20 };
struct sock_filter {
    uint16_t code;
    uint8_t jt;
    uint8_t jf;
    uint32_t k;
};
struct sock_fprog {
    unsigned short len;
    struct sock_filter *filter;
};
struct seccomp_data {
    int nr;
    uint32_t arch;
    uint64_t instruction_pointer;
    uint64_t args[6];
};
enum {
    BPF_LD_W_ABS = 0x20,
    BPF_LD_W_LEN = 0x80,
    BPF_LDX_W_LEN = 0x81,
    BPF_LD_IMM = 0x00,
    BPF_LDX_IMM = 0x01,
    BPF_LD_MEM = 0x60,
    BPF_LDX_MEM = 0x61,
    BPF_ST = 0x02,
    BPF_STX = 0x03,
    BPF_MISC_TAX = 0x07,
    BPF_MISC_TXA = 0x87,
    BPF_JMP_JA = 0x05,
    BPF_JMP_JEQ_K = 0x15,
    BPF_JMP_JEQ_X = 0x1d,
    BPF_JMP_JGT_K = 0x25,
    BPF_JMP_JGT_X = 0x2d,
    BPF_JMP_JGE_K = 0x35,
    BPF_JMP_JGE_X = 0x3d,
    BPF_JMP_JSET_K = 0x45,
    BPF_JMP_JSET_X = 0x4d,
    BPF_ALU_ADD_K = 0x04,
    BPF_ALU_ADD_X = 0x0c,
    BPF_ALU_SUB_K = 0x14,
    BPF_ALU_SUB_X = 0x1c,
    BPF_ALU_MUL_K = 0x24,
    BPF_ALU_MUL_X = 0x2c,
    BPF_ALU_DIV_K = 0x34,
    BPF_ALU_DIV_X = 0x3c,
    BPF_ALU_OR_K = 0x44,
    BPF_ALU_OR_X = 0x4c,
    BPF_ALU_AND_K = 0x54,
    BPF_ALU_AND_X = 0x5c,
    BPF_ALU_LSH_K = 0x64,
    BPF_ALU_LSH_X = 0x6c,
    BPF_ALU_RSH_K = 0x74,
    BPF_ALU_RSH_X = 0x7c,
    BPF_ALU_NEG = 0x84,
    BPF_ALU_MOD_K = 0x94,
    BPF_ALU_XOR_K = 0xa4,
    BPF_ALU_XOR_X = 0xac,
    BPF_RET_K = 0x06,
    BPF_RET_A = 0x16,
};
#define SECCOMP_SET_MODE_STRICT 0
#define SECCOMP_SET_MODE_FILTER 1
#define SECCOMP_GET_ACTION_AVAIL 2
#define SECCOMP_RET_KILL_PROCESS 0x80000000U
#define SECCOMP_RET_KILL_THREAD 0x00000000U
#define SECCOMP_RET_TRAP 0x00030000U
#define SECCOMP_RET_ERRNO 0x00050000U
#define SECCOMP_RET_TRACE 0x7ff00000U
#define SECCOMP_RET_LOG 0x7ffc0000U
#define SECCOMP_RET_ALLOW 0x7fff0000U
#define SECCOMP_RET_USER_NOTIF 0x7fc00000U
#define SECCOMP_FILTER_FLAG_TSYNC 1
#define SECCOMP_FILTER_FLAG_SPEC_ALLOW 4
#define SECCOMP_FILTER_FLAG_TSYNC_ESRCH 16
#define AUDIT_ARCH_X86_64 0xc000003eU
#define X32_SYSCALL_BIT 0x40000000U
#define SYS_SECCOMP_SI_CODE 1
#define TRAPPED_RESULT 4242
#define do_syscall(n, ...) syscall(n, ##__VA_ARGS__)
#define STMT(c, v) ((struct sock_filter){ (c), 0, 0, (v) })
#define JUMP(c, v, t, f) ((struct sock_filter){ (c), (t), (f), (v) })
static int passed, failed;
static const char *volatile current_check = "";
static char self[4096];
extern char **environ;
static void record(const char *name, int ok, const char *fmt, ...)
{
    char detail[192] = "", line[320];
    if (fmt) {
        va_list ap;
        va_start(ap, fmt);
        vsnprintf(detail, sizeof detail, fmt, ap);
        va_end(ap);
    }
    int len = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s%s%s\n", name, ok ? "PASS" : "FAIL",
        detail[0] ? " detail=" : "", detail);
    write(1, line, (size_t)len);
    ok ? passed++ : failed++;
}
static void deadline(int sig)
{
    (void)sig;
    char line[160];
    int len = snprintf(line, sizeof line,
        "MINI_CHROMIUM_TEST name=%s status=FAIL detail=watchdog\n"
        "MINI_CHROMIUM_RESULT status=FAIL passed=%d failed=%d\n",
        current_check, passed, failed + 1);
    write(1, line, (size_t)len);
    _exit(1);
}
static int wait_status(pid_t pid)
{
    int status;
    pid_t r;
    while ((r = waitpid(pid, &status, 0)) < 0 && errno == EINTR)
        ;
    return r == pid ? status : -1;
}
static int run_child(int (*fn)(void))
{
    pid_t pid = fork();
    if (pid < 0)
        return -1;
    if (pid == 0)
        _exit(fn());
    return wait_status(pid);
}
static int child_exit_code(int status)
{
    return WIFEXITED(status) ? WEXITSTATUS(status) : 1000 + status;
}
static void run_check(const char *name, void (*fn)(void))
{
    current_check = name;
    alarm(CHECK_DEADLINE_SECONDS);
    fn();
    alarm(0);
}
static ssize_t read_file(const char *path, char *buf, size_t len)
{
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return -errno;
    size_t got = 0;
    ssize_t n;
    while (got < len - 1 && (n = read(fd, buf + got, len - 1 - got)) > 0)
        got += (size_t)n;
    close(fd);
    buf[got] = 0;
    return (ssize_t)got;
}
static int status_field(const char *field, char *out, size_t outlen)
{
    char text[4096];
    if (read_file("/proc/self/status", text, sizeof text) < 0)
        return -1;
    size_t flen = strlen(field);
    for (char *line = text; line && *line; line = strchr(line, '\n') ? strchr(line, '\n') + 1 : 0) {
        if (!strncmp(line, field, flen) && line[flen] == ':') {
            char *v = line + flen + 1;
            while (*v == ' ' || *v == '\t')
                v++;
            size_t len = strcspn(v, "\n");
            if (len >= outlen)
                len = outlen - 1;
            memcpy(out, v, len);
            out[len] = 0;
            return 0;
        }
    }
    return -1;
}
static int field_is(const char *field, const char *want)
{
    char v[256];
    return !status_field(field, v, sizeof v) && !strcmp(v, want);
}
static int seccomp_install(unsigned flags, struct sock_filter *f, unsigned len)
{
    struct sock_fprog prog = { (unsigned short)len, f };
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return -errno;
    return syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, flags, &prog) ? -errno : 0;
}
static int child_invalid_corpus(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return 2;
    struct sock_filter out_of_range[] = { JUMP(BPF_JMP_JA, 50, 0, 0), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_filter no_return[] = { STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)) };
    struct sock_filter misaligned[] = { STMT(BPF_LD_W_ABS, 3), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_filter beyond[] = { STMT(BPF_LD_W_ABS, sizeof(struct seccomp_data)), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_filter mod[] = { STMT(BPF_ALU_MOD_K, 0), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_filter unbalanced[] = { STMT(BPF_LD_MEM, 0), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_filter div0[] = { STMT(BPF_ALU_DIV_K, 0), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_filter lsh[] = { STMT(BPF_ALU_LSH_K, 32), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_fprog progs[] = {
        { 2, out_of_range }, { 1, no_return }, { 2, misaligned }, { 2, beyond }, { 0, out_of_range },
        { 2, mod }, { 2, unbalanced }, { 2, div0 }, { 2, lsh },
    };
    int rejected = 0;
    for (size_t i = 0; i < sizeof progs / sizeof progs[0]; i++) {
        errno = 0;
        if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &progs[i]) < 0 && errno == EINVAL)
            rejected++;
        else
            return 10 + (int)i;
    }
    if (rejected != (int)(sizeof progs / sizeof progs[0]))
        return 20;
    unsigned short big_len = 4097;
    struct sock_filter *big = malloc(big_len * sizeof(struct sock_filter));
    if (!big)
        return 21;
    for (unsigned short i = 0; i < big_len - 1; i++)
        big[i] = STMT(BPF_LD_IMM, 0);
    big[big_len - 1] = STMT(BPF_RET_K, SECCOMP_RET_ALLOW);
    struct sock_fprog bigprog = { big_len, big };
    errno = 0;
    int big_ok = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &bigprog) < 0 && errno == EINVAL;
    free(big);
    return big_ok ? 0 : 22;
}
static void check_invalid_corpus(void)
{
    int status = run_child(child_invalid_corpus);
    record("invalid_corpus", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_null_and_flags(void)
{
    errno = 0;
    int p1 = prctl(PR_SET_SECCOMP, 2, 0, 0, 0) < 0 && errno == EFAULT;
    errno = 0;
    int p2 = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, 0) < 0 && errno == EFAULT;
    errno = 0;
    int p3 = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, 0) < 0 && errno == EFAULT;
    struct sock_filter allow[] = { STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_fprog prog = { 1, allow };
    errno = 0;
    int bad_flag = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0x100, &prog) < 0 && errno == EINVAL;
    errno = 0;
    int bad_flag2 = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0x20, &prog) < 0 && errno == EINVAL;
    errno = 0;
    int bad_op = syscall(SYS_seccomp, 99, 0, &prog) < 0 && errno == EINVAL;
    errno = 0;
    struct sock_filter dummy[] = { STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    (void)dummy;
    int strict_bad = syscall(SYS_seccomp, SECCOMP_SET_MODE_STRICT, 1, 0) < 0 && errno == EINVAL;
    return (p1 && p2 && p3 && bad_flag && bad_flag2 && bad_op && strict_bad) ? 0 : 2;
}
static void check_null_flags(void)
{
    int status = run_child(child_null_and_flags);
    record("null_and_flags", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_requires_nnp(void)
{
    if (geteuid() == 0) {
        if (setgroups(0, 0) || setgid(1000) || setuid(1000))
            return 2;
    }
    if (geteuid() == 0)
        return 3;
    if (prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 0)
        return 4;
    struct sock_filter allow[] = { STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_fprog prog = { 1, allow };
    errno = 0;
    int r = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog);
    return (r < 0 && errno == EACCES) ? 0 : 5;
}
static void check_requires_nnp(void)
{
    int status = run_child(child_requires_nnp);
    record("requires_nnp", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_errno_cap(void)
{
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | 5000),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 2;
    errno = 0;
    int r = socket(AF_INET, SOCK_STREAM, 0);
    int capped = (r < 0 && errno == 4095);
    int ok_getpid = getpid() > 0;
    return (capped && ok_getpid) ? 0 : 3;
}
static void check_errno_cap(void)
{
    int status = run_child(child_errno_cap);
    record("errno_cap", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static volatile sig_atomic_t trapped_syscall = -1;
static volatile sig_atomic_t trapped_arch_ok;
static volatile sig_atomic_t trapped_ra;
static volatile sig_atomic_t trapped_rip_ok;
static void on_sigsys(int sig, siginfo_t *info, void *ctx)
{
    (void)sig;
    ucontext_t *uc = ctx;
    trapped_syscall = info->si_syscall;
    trapped_arch_ok = info->si_code == SYS_SECCOMP_SI_CODE && info->si_arch == AUDIT_ARCH_X86_64;
    trapped_ra = uc->uc_mcontext.gregs[REG_RAX];
    trapped_rip_ok = uc->uc_mcontext.gregs[REG_RIP] == (long)(uintptr_t)info->si_call_addr;
    uc->uc_mcontext.gregs[REG_RAX] = TRAPPED_RESULT;
}
static int child_trap(void)
{
    struct sigaction sa = { .sa_sigaction = on_sigsys, .sa_flags = SA_SIGINFO };
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSYS, &sa, 0);
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_uname, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_TRAP),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 2;
    struct utsname u;
    errno = 0;
    long trapped = syscall(SYS_uname, &u);
    int ok = trapped == TRAPPED_RESULT && trapped_syscall == SYS_uname && trapped_arch_ok && trapped_ra == SYS_uname && trapped_rip_ok;
    return ok ? 0 : 3;
}
static void check_trap(void)
{
    int status = run_child(child_trap);
    record("trap", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_trap_blocked(void)
{
    struct sigaction sa = { .sa_sigaction = on_sigsys, .sa_flags = SA_SIGINFO };
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSYS, &sa, 0);
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGSYS);
    if (sigprocmask(SIG_BLOCK, &mask, 0))
        return 2;
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_uname, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_TRAP),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 3;
    struct utsname u;
    trapped_syscall = -1;
    long trapped = syscall(SYS_uname, &u);
    int ok = trapped == TRAPPED_RESULT && trapped_syscall == SYS_uname;
    return ok ? 0 : 4;
}
static void check_trap_blocked(void)
{
    int status = run_child(child_trap_blocked);
    record("trap_blocked", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_kill_process(void)
{
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_getpid, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 2;
    getpid();
    return 0;
}
static void check_kill_process(void)
{
    int status = run_child(child_kill_process);
    int ok = WIFSIGNALED(status) && WTERMSIG(status) == SIGSYS;
    record("kill_process", ok, "stage=%d", child_exit_code(status));
}
static void *kill_thread_worker(void *arg)
{
    (void)arg;
    struct utsname uu;
    syscall(SYS_uname, &uu);
    return 0;
}
static int child_kill_thread(void)
{
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_uname, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_KILL_THREAD),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 2;
    pthread_t th;
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    if (pthread_create(&th, &attr, kill_thread_worker, 0))
        return 3;
    for (int i = 0; i < 200; i++) {
        if (field_is("Threads", "1"))
            break;
        struct timespec ts = { 0, 10000000 };
        nanosleep(&ts, 0);
    }
    if (!field_is("Threads", "1"))
        return 4;
    return (getpid() > 0) ? 0 : 5;
}
static void check_kill_thread(void)
{
    int status = run_child(child_kill_thread);
    record("kill_thread", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_stacking(void)
{
    struct sock_filter deny[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EACCES),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, deny, 4))
        return 2;
    struct sock_filter allow[] = { STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_fprog prog = { 1, allow };
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog))
        return 3;
    errno = 0;
    int r = socket(AF_INET, SOCK_STREAM, 0);
    int denied = (r < 0 && errno == EACCES);
    int filters = field_is("Seccomp_filters", "2");
    return (denied && filters) ? 0 : 4;
}
static void check_stacking(void)
{
    int status = run_child(child_stacking);
    record("stacking", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_ptrace(void)
{
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_ptrace, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EPERM),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 2;
    errno = 0;
    int r = ptrace(PTRACE_ATTACH, 1, 0, 0);
    return (r < 0 && errno == EPERM) ? 0 : 3;
}
static void check_ptrace(void)
{
    int status = run_child(child_ptrace);
    record("ptrace_filtered", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_x32(void)
{
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JSET_K, X32_SYSCALL_BIT, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | 222),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 2;
    errno = 0;
    int r = syscall(SYS_getpid | X32_SYSCALL_BIT);
    return (r < 0 && errno == 222) ? 0 : 3;
}
static void check_x32(void)
{
    int status = run_child(child_x32);
    record("x32", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int check_socket_denied(void)
{
    errno = 0;
    return socket(AF_INET, SOCK_STREAM, 0) < 0 && errno == EACCES;
}
static int child_inherit_fork(void)
{
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EACCES),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 2;
    pid_t pid = fork();
    if (pid < 0)
        return 3;
    if (pid == 0) {
        int ok = check_socket_denied() && field_is("Seccomp", "2") && field_is("Seccomp_filters", "1");
        _exit(ok ? 0 : 1);
    }
    int status = wait_status(pid);
    return child_exit_code(status) == 0 ? 0 : 4;
}
static void check_inherit_fork(void)
{
    int status = run_child(child_inherit_fork);
    record("inherit_fork", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static void *inherit_clone_worker(void *arg)
{
    (void)arg;
    errno = 0;
    int denied = check_socket_denied();
    int sec = field_is("Seccomp", "2");
    return (void *)(intptr_t)(denied && sec ? 0 : 1);
}
static int child_inherit_clone(void)
{
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EACCES),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 2;
    pthread_t th;
    if (pthread_create(&th, 0, inherit_clone_worker, 0))
        return 3;
    void *res = 0;
    pthread_join(th, &res);
    return (int)(intptr_t)res;
}
static void check_inherit_clone(void)
{
    int status = run_child(child_inherit_clone);
    record("inherit_clone", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int role_inherit_check(void)
{
    return (check_socket_denied() && field_is("Seccomp", "2")) ? 0 : 1;
}
static int child_inherit_exec(void)
{
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EACCES),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    if (seccomp_install(0, filter, 4))
        return 2;
    char *argv[] = { self, "--role=inherit-check", 0 };
    execve(self, argv, environ);
    return 3;
}
static void check_inherit_exec(void)
{
    int status = run_child(child_inherit_exec);
    record("inherit_exec", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_action_avail(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return 2;
    struct { uint32_t act; int want; } cases[] = {
        { SECCOMP_RET_KILL_PROCESS, 0 }, { SECCOMP_RET_KILL_THREAD, 0 }, { SECCOMP_RET_TRAP, 0 },
        { SECCOMP_RET_ERRNO, 0 }, { SECCOMP_RET_TRACE, 0 }, { SECCOMP_RET_LOG, 0 }, { SECCOMP_RET_ALLOW, 0 },
    };
    for (size_t i = 0; i < sizeof cases / sizeof cases[0]; i++) {
        uint32_t act = cases[i].act;
        errno = 0;
        long r = syscall(SYS_seccomp, SECCOMP_GET_ACTION_AVAIL, 0, &act);
        if (r != 0)
            return 3 + (int)i;
    }
    uint32_t act = SECCOMP_RET_USER_NOTIF;
    errno = 0;
    long r = syscall(SYS_seccomp, SECCOMP_GET_ACTION_AVAIL, 0, &act);
    if (r < 0 && errno == EOPNOTSUPP)
        return 0;
    return 4;
}
static void check_action_avail(void)
{
    int status = run_child(child_action_avail);
    record("action_avail", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_prctl_get(void)
{
    struct sock_filter filter[] = { STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    if (seccomp_install(0, filter, 1))
        return 2;
    int mode = prctl(PR_GET_SECCOMP, 0, 0, 0, 0);
    return (mode == 2 && field_is("Seccomp", "2") && field_is("Seccomp_filters", "1")) ? 0 : 3;
}
static void check_prctl_get(void)
{
    int status = run_child(child_prctl_get);
    record("prctl_get", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static volatile int tsync_ready;
static volatile int tsync_go;
static volatile int tsync_errno;
static void *tsync_worker(void *arg)
{
    (void)arg;
    tsync_ready = 1;
    while (!tsync_go) {
        struct timespec ts = { 0, 1000000 };
        nanosleep(&ts, 0);
    }
    errno = 0;
    int r = socket(AF_INET, SOCK_STREAM, 0);
    tsync_errno = (r < 0 ? errno : 0);
    return 0;
}
static int child_tsync_success(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return 2;
    tsync_ready = 0;
    tsync_go = 0;
    tsync_errno = 0;
    pthread_t th;
    if (pthread_create(&th, 0, tsync_worker, 0))
        return 3;
    while (!tsync_ready) {
        struct timespec ts = { 0, 1000000 };
        nanosleep(&ts, 0);
    }
    struct sock_filter filter[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EACCES),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = { 4, filter };
    errno = 0;
    long r = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, &prog);
    if (r != 0)
        return 4;
    tsync_go = 1;
    pthread_join(th, 0);
    int main_denied = check_socket_denied();
    return (tsync_errno == EACCES && main_denied) ? 0 : 5;
}
static void check_tsync_success(void)
{
    int status = run_child(child_tsync_success);
    record("tsync_success", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static volatile int divergent_ready;
static volatile int divergent_go;
static pthread_t divergent_tid;
static void *divergent_worker(void *arg)
{
    (void)arg;
    struct sock_filter f[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | E2BIG),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = { 4, f };
    syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog);
    divergent_ready = 1;
    while (!divergent_go) {
        struct timespec ts = { 0, 1000000 };
        nanosleep(&ts, 0);
    }
    return 0;
}
static int child_tsync_divergent(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return 2;
    divergent_ready = 0;
    divergent_go = 0;
    struct sock_filter base[] = { STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_fprog baseprog = { 1, base };
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &baseprog))
        return 3;
    if (pthread_create(&divergent_tid, 0, divergent_worker, 0))
        return 4;
    while (!divergent_ready) {
        struct timespec ts = { 0, 1000000 };
        nanosleep(&ts, 0);
    }
    struct sock_filter nz[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EACCES),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog nzprog = { 4, nz };
    errno = 0;
    long r = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, &nzprog);
    divergent_go = 1;
    pthread_join(divergent_tid, 0);
    return (r > 0) ? 0 : 5;
}
static void check_tsync_divergent(void)
{
    int status = run_child(child_tsync_divergent);
    record("tsync_divergent", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_tsync_esrch(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        return 2;
    divergent_ready = 0;
    divergent_go = 0;
    struct sock_filter base[] = { STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
    struct sock_fprog baseprog = { 1, base };
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &baseprog))
        return 3;
    if (pthread_create(&divergent_tid, 0, divergent_worker, 0))
        return 4;
    while (!divergent_ready) {
        struct timespec ts = { 0, 1000000 };
        nanosleep(&ts, 0);
    }
    struct sock_filter nz[] = {
        STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
        JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
        STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EACCES),
        STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog nzprog = { 4, nz };
    errno = 0;
    long r = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC | SECCOMP_FILTER_FLAG_TSYNC_ESRCH, &nzprog);
    divergent_go = 1;
    pthread_join(divergent_tid, 0);
    return (r < 0 && errno == ESRCH) ? 0 : 5;
}
static void check_tsync_esrch(void)
{
    int status = run_child(child_tsync_esrch);
    record("tsync_esrch", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}
static int child_strict_allowed(void)
{
    int p[2];
    if (pipe(p))
        return 2;
    long r = syscall(SYS_seccomp, SECCOMP_SET_MODE_STRICT, 0, 0);
    if (r != 0)
        return 3;
    if (write(p[1], "ok", 2) != 2)
        return 4;
    syscall(SYS_exit, 0);
    return 5;
}
static void check_strict_allowed(void)
{
    int status = run_child(child_strict_allowed);
    int ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    record("strict_allowed", ok, "stage=%d", child_exit_code(status));
}
static int child_strict_denied(void)
{
    long r = syscall(SYS_seccomp, SECCOMP_SET_MODE_STRICT, 0, 0);
    if (r != 0)
        return 2;
    getpid();
    return 0;
}
static void check_strict_denied(void)
{
    int status = run_child(child_strict_denied);
    int ok = WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL;
    record("strict_denied", ok, "stage=%d", child_exit_code(status));
}
int main(int argc, char **argv)
{
    ssize_t n = readlink("/proc/self/exe", self, sizeof self - 1);
    if (n > 0)
        self[n] = 0;
    else
        snprintf(self, sizeof self, "%s", argv[0]);
    if (argc > 1 && !strcmp(argv[1], "--role=inherit-check"))
        return role_inherit_check();
    signal(SIGALRM, deadline);
    run_check("invalid_corpus", check_invalid_corpus);
    run_check("null_and_flags", check_null_flags);
    run_check("requires_nnp", check_requires_nnp);
    run_check("errno_cap", check_errno_cap);
    run_check("trap", check_trap);
    run_check("trap_blocked", check_trap_blocked);
    run_check("kill_process", check_kill_process);
    run_check("kill_thread", check_kill_thread);
    run_check("stacking", check_stacking);
    run_check("ptrace_filtered", check_ptrace);
    run_check("x32", check_x32);
    run_check("inherit_fork", check_inherit_fork);
    run_check("inherit_clone", check_inherit_clone);
    run_check("inherit_exec", check_inherit_exec);
    run_check("action_avail", check_action_avail);
    run_check("prctl_get", check_prctl_get);
    run_check("tsync_success", check_tsync_success);
    run_check("tsync_divergent", check_tsync_divergent);
    run_check("tsync_esrch", check_tsync_esrch);
    run_check("strict_allowed", check_strict_allowed);
    run_check("strict_denied", check_strict_denied);
    char line[96];
    int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
        passed, failed);
    write(1, line, (size_t)len);
    return failed ? 1 : 0;
}
