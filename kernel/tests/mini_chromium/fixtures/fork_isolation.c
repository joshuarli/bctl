#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile int memory_value = 17;
static _Thread_local int tls_value = 23;

#if defined(__aarch64__)
static unsigned long fp_control(void)
{
    unsigned long value;
    __asm__ volatile("mrs %0, fpcr" : "=r"(value));
    return value;
}

static void set_fp_control(unsigned long value)
{
    __asm__ volatile("msr fpcr, %0" :: "r"(value));
}
#endif

int main(void)
{
    int ready[2];
    if (pipe(ready))
        return 1;
#if defined(__aarch64__)
    unsigned long original_fp = fp_control();
    unsigned long inherited_fp = (original_fp & ~(3UL << 22)) | (1UL << 22);
    set_fp_control(inherited_fp);
#endif
    pid_t child = fork();
    if (child == 0) {
        close(ready[1]);
        char token;
        if (read(ready[0], &token, 1) != 1)
            _exit(1);
        int ok = memory_value == 17 && tls_value == 23;
#if defined(__aarch64__)
        ok = ok && fp_control() == inherited_fp;
#endif
        memory_value = 31;
        tls_value = 47;
        _exit(ok && memory_value == 31 && tls_value == 47 ? 42 : 1);
    }
    close(ready[0]);
#if defined(__aarch64__)
    set_fp_control(original_fp);
#endif
    int sent = child > 0 && write(ready[1], "x", 1) == 1;
    close(ready[1]);
    int status = 0;
    int ok = sent && child > 0 && waitpid(child, &status, 0) == child &&
        WIFEXITED(status) && WEXITSTATUS(status) == 42 &&
        memory_value == 17 && tls_value == 23;
#if defined(__aarch64__)
    ok = ok && fp_control() == original_fp;
#endif
    printf("MINI_CHROMIUM_TEST name=fork_isolation status=%s\n", ok ? "PASS" : "FAIL");
    printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
    return !ok;
}
