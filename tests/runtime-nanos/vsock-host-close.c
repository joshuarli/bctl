#include <errno.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>
#ifndef AF_VSOCK
#define AF_VSOCK 40
#endif
struct vsock_address { unsigned short family, reserved; unsigned int port, cid; unsigned char zero[4]; };

static unsigned int ports[800];
static int port_count;

static int remember_port(int fd) {
    struct vsock_address local = {0};
    socklen_t length = sizeof(local);
    if (getsockname(fd, (void *)&local, &length) || length != sizeof(local))
        return -1;
    for (int i = 0; i < port_count; i++)
        if (ports[i] == local.port)
            return -1;
    ports[port_count++] = local.port;
    return 0;
}

int main(void) {
    struct vsock_address addr = { .family = AF_VSOCK, .cid = 2, .port = 1080 };
    for (int i = 0; i < 400; i++) {
        int fd = socket(AF_VSOCK, SOCK_STREAM, 0);
        if (fd < 0 || connect(fd, (void *)&addr, sizeof(addr)) || remember_port(fd)) {
            printf("MINI_CHROMIUM_TEST name=vsock_host_close status=FAIL detail=connect-%d-errno-%d\n", i, errno);
            return 1;
        }
        char send = 'a', recv = 0;
        if (write(fd, &send, 1) != 1 || read(fd, &recv, 1) != 1 || recv != send) {
            printf("MINI_CHROMIUM_TEST name=vsock_host_close status=FAIL detail=io-%d-errno-%d\n", i, errno);
            return 1;
        }
        close(fd);
    }
    for (int i = 0; i < 400; i++) {
        int fd = socket(AF_VSOCK, SOCK_STREAM, 0);
        char byte = 'b';
        if (fd < 0 || connect(fd, (void *)&addr, sizeof(addr)) || remember_port(fd) || write(fd, &byte, 1) != 1 ||
            shutdown(fd, SHUT_RD)) {
            printf("MINI_CHROMIUM_TEST name=vsock_host_close status=FAIL detail=shutdown-%d-errno-%d\n", i, errno);
            return 1;
        }
        close(fd);
    }
    puts("MINI_CHROMIUM_TEST name=vsock_host_close status=PASS detail=800-connections");
    return 0;
}
