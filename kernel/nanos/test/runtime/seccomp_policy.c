#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
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
    SECCOMP_GET_ACTION_AVAIL = 2,
    SECCOMP_MODE_STRICT = 1,
    SECCOMP_RET_ERRNO = 0x00050000,
    SECCOMP_RET_ALLOW = 0x7fff0000,
};

static int mode_transition(int use_prctl)
{
    struct sock_filter filter[] = {
        {BPF_LD_W_ABS, 0, 0, 0},
        {BPF_JMP_JEQ_K, 0, 1, SYS_write},
        {BPF_RET_K, 0, 0, SECCOMP_RET_ERRNO | EPERM},
        {BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW},
    };
    struct sock_fprog program = {.len = sizeof filter / sizeof filter[0], .filter = filter};
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) ||
        syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &program))
        return 1;
    errno = 0;
    long result = use_prctl ? prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT, 0, 0, 0) :
        syscall(SYS_seccomp, SECCOMP_SET_MODE_STRICT, 0, NULL);
    if (result != -1 || errno != EINVAL)
        syscall(SYS_exit, 2);
    errno = 0;
    if (syscall(SYS_write, -1, NULL, 0) != -1 || errno != EPERM)
        return 3;
    return 0;
}

static int action_query(unsigned flags, uint32_t action, int expected)
{
    errno = 0;
    long result = syscall(SYS_seccomp, SECCOMP_GET_ACTION_AVAIL, flags, &action);
    return result == -1 && errno == expected ? 0 : 1;
}

int main(void)
{
    alarm(20);
    const char *names[] = {"filter_to_strict", "prctl_filter_to_strict", "action_query_flags",
        "action_query_unknown", "action_query_data_bits"};
    unsigned failed = 0;
    for (unsigned i = 0; i < sizeof names / sizeof names[0]; i++) {
        pid_t child = fork();
        if (child == 0) {
            int result;
            if (i < 2)
                result = mode_transition(i == 1);
            else if (i == 2)
                result = action_query(1, SECCOMP_RET_ALLOW, EINVAL);
            else
                result = action_query(0, i == 3 ? 0x12340000 : SECCOMP_RET_ALLOW | 1, EOPNOTSUPP);
            _exit(result);
        }
        int status = 0;
        int ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
        failed += !ok;
        printf("MINI_CHROMIUM_TEST name=%s status=%s detail=wait_status=%d\n", names[i], ok ? "PASS" : "FAIL", status);
    }
    printf("MINI_CHROMIUM_RESULT status=%s passed=%u failed=%u\n", failed ? "FAIL" : "PASS", 5 - failed, failed);
    return failed != 0;
}
