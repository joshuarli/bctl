#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef AF_VSOCK
#define AF_VSOCK 40
#endif

struct vsock_address {
    unsigned short family;
    unsigned short reserved;
    unsigned int port;
    unsigned int cid;
    unsigned char zero[4];
};

static int reconnect(void)
{
    struct vsock_address addr = { .family = AF_VSOCK, .cid = 2, .port = 9222 };
    for (int i = 0; i < 32; i++) {
        int refused = socket(AF_VSOCK, SOCK_STREAM | SOCK_NONBLOCK, 0);
        if (refused < 0 || (connect(refused, (void *)&addr, sizeof(addr)) && errno != EINPROGRESS))
            return 1;
        struct pollfd p = { .fd = refused, .events = POLLOUT };
        if (poll(&p, 1, 5000) != 1 || !(p.revents & POLLERR))
            return 2;
        close(refused);
        addr.port = 1080;
        int connected = socket(AF_VSOCK, SOCK_STREAM, 0);
        if (connected < 0 || connect(connected, (void *)&addr, sizeof(addr)))
            return 3;
        char sent = 'a', received = 0;
        if (write(connected, &sent, 1) != 1 || read(connected, &received, 1) != 1 || received != sent)
            return 4;
        close(connected);
        addr.port = 9222;
    }
    return 0;
}

int main(void)
{
    int result = reconnect();
    printf("MINI_CHROMIUM_TEST name=vsock_reconnect status=%s detail=step-%d-errno-%d\n",
           result ? "FAIL" : "PASS", result, errno);
    return result != 0;
}
