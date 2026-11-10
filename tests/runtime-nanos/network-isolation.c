#include <arpa/inet.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <unistd.h>

static int connect_to(const char *ip, unsigned short port)
{
    struct sockaddr_in address = { .sin_family = AF_INET, .sin_port = htons(port) };
    if (inet_pton(AF_INET, ip, &address.sin_addr) != 1)
        return -1;
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0)
        return -1;
    int result = connect(fd, (void *)&address, sizeof(address));
    int error = result == 0 ? 0 : errno;
    close(fd);
    return error;
}

int main(int argc, char **argv)
{
    if (argc != 2)
        return 1;
    unsigned long port = strtoul(argv[1], 0, 10);
    if (port == 0 || port > 65535)
        return 1;
    int loopback = connect_to("127.0.0.1", (unsigned short)port);
    if (loopback <= 0) {
        printf("MINI_CHROMIUM_TEST name=network_isolation status=FAIL detail=loopback-%d\n", loopback);
        return 1;
    }
    const char *addresses[] = {"10.0.0.1", "172.16.0.1", "192.168.1.1", "169.254.169.254", "93.184.215.14"};
    for (unsigned int i = 0; i < sizeof(addresses) / sizeof(addresses[0]); i++) {
        int error = connect_to(addresses[i], 80);
        if (error != ENETUNREACH && error != EHOSTUNREACH) {
            printf("MINI_CHROMIUM_TEST name=network_isolation status=FAIL detail=%s-%d\n", addresses[i], error);
            return 1;
        }
    }
    puts("MINI_CHROMIUM_TEST name=network_isolation status=PASS detail=no-external-interface");
    return 0;
}
