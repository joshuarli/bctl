#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/file.h>
#include <time.h>
#include <unistd.h>

#include "libkrun.h"

static void startup(const char *stage)
{
    if (!getenv("BCTL_STARTUP_TRACE"))
        return;
    struct timespec now;
    if (clock_gettime(CLOCK_REALTIME, &now)) {
        perror("nanos-krun: startup clock");
        return;
    }
    fprintf(stderr, "bctl-startup runner %s epoch_us=%llu\n", stage,
            (unsigned long long)now.tv_sec * 1000000 + (unsigned long long)now.tv_nsec / 1000);
}

static int number(const char *text, unsigned long max, unsigned long *value)
{
    char *end;
    errno = 0;
    unsigned long parsed = strtoul(text, &end, 10);
    if (errno || end == text || *end || !parsed || parsed > max)
        return -1;
    *value = parsed;
    return 0;
}

static int configured(const char *operation, int32_t result)
{
    if (result >= 0) {
        startup(operation);
        return 1;
    }
    fprintf(stderr, "nanos-krun: %s: error %d\n", operation, result);
    return 0;
}

int main(int argc, char **argv)
{
    startup("begin");
    unsigned long cpus, memory;
    if ((argc != 8 && argc != 10 && argc != 11) || number(argv[6], UINT8_MAX, &cpus) || number(argv[7], UINT32_MAX, &memory)) {
        fprintf(stderr, "usage: nanos-krun DISK FIRMWARE CDP_SOCKET CHECK_SOCKET EGRESS_SOCKET CPUS MEMORY_MIB [PROFILE_DISK STOP_SOCKET [FILES_DIR]]\n");
        return 2;
    }
    if (argc >= 10) {
        int profile = open(argv[8], O_RDWR | O_NOFOLLOW | O_CLOEXEC);
        if (profile < 0 || flock(profile, LOCK_EX | LOCK_NB)) {
            perror("nanos-krun: lock profile");
            return 1;
        }
    }
    int console[2];
    if (pipe(console)) {
        perror("nanos-krun: console pipe");
        return 1;
    }
    int32_t ctx = krun_create_ctx();
    if (!configured("create context", ctx))
        return 1;
    if (!configured("configure VM", krun_set_vm_config((uint32_t)ctx, (uint8_t)cpus, (uint32_t)memory)) ||
        !configured("configure console", krun_add_serial_console_default((uint32_t)ctx, console[0], STDOUT_FILENO)) ||
        !configured("attach disk", krun_add_disk_dax((uint32_t)ctx, "root", argv[1], false)) ||
        (argc >= 10 && !configured("attach profile", krun_add_disk((uint32_t)ctx, "profile", argv[8], false))) ||
        !configured("load firmware", krun_set_firmware((uint32_t)ctx, argv[2])) ||
        !configured("add vsock", krun_add_vsock((uint32_t)ctx, 0)) ||
        !configured("map CDP", krun_add_vsock_port2((uint32_t)ctx, 9222, argv[3], true)) ||
        !configured("map readiness", krun_add_vsock_port2((uint32_t)ctx, 9223, argv[4], true)) ||
        (argc >= 10 && !configured("map stop", krun_add_vsock_port2((uint32_t)ctx, 9224, argv[9], true))) ||
        !configured("map egress", krun_add_vsock_port2((uint32_t)ctx, 1080, argv[5], false)) ||
        (argc == 11 && !configured("share files", krun_add_virtiofs((uint32_t)ctx, "files", argv[10])))) {
        krun_free_ctx((uint32_t)ctx);
        return 1;
    }
    startup("enter_vm");
    int32_t result = krun_start_enter((uint32_t)ctx);
    configured("start VM", result);
    krun_free_ctx((uint32_t)ctx);
    return 1;
}
