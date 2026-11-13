#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

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

#if defined(__aarch64__)
#define AUDIT_ARCH_NATIVE 0xc00000b7U
#elif defined(__x86_64__)
#define AUDIT_ARCH_NATIVE 0xc000003eU
#else
#error unsupported architecture
#endif

int main(void)
{
    struct sock_filter filter[] = {
        {0x20, 0, 0, 0},
        {0x15, 0, 4, SYS_getpid},
        {0x20, 0, 0, 4},
        {0x15, 0, 1, AUDIT_ARCH_NATIVE},
        {0x06, 0, 0, 0x00050000U | EPERM},
        {0x06, 0, 0, 0x00050000U | EACCES},
        {0x06, 0, 0, 0x7fff0000U},
    };
    struct sock_fprog program = {7, filter};
    int ok = prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0 &&
        syscall(SYS_seccomp, 1, 0, &program) == 0;
    errno = 0;
    ok = ok && syscall(SYS_getpid) == -1 && errno == EPERM;
    printf("MINI_CHROMIUM_TEST name=seccomp_arch status=%s\n", ok ? "PASS" : "FAIL");
    printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
    return !ok;
}
