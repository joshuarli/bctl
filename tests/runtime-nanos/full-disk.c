#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/statvfs.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static const unsigned short port = 47217;
static char data[1024 * 1024];

static int fill_disk(void)
{
    int fd = open("/profile/full-disk-fixture", O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0)
        return errno;
    for (unsigned int i = 0; i < 4096; i++) {
        ssize_t count = write(fd, data, sizeof(data));
        if (count < 0) {
            int error = errno;
            close(fd);
            return error;
        }
        if (count == 0) {
            close(fd);
            return EIO;
        }
    }
    close(fd);
    return EFBIG;
}

int main(void)
{
    pid_t child = fork();
    if (child < 0)
        return 1;
    if (child == 0) {
        execl("/usr/local/bin/bctl-guest", "bctl-guest", "run-headless-persistent", (char *)0);
        return 1;
    }
    int server = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in address = { .sin_family = AF_INET, .sin_port = htons(port), .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) } };
    if (server < 0 || bind(server, (void *)&address, sizeof(address)) || listen(server, 4))
        return 1;
    int client = accept(server, 0, 0);
    if (client < 0)
        return 1;
    char request[1024];
    if (read(client, request, sizeof(request)) <= 0)
        return 1;
    int result = fill_disk();
    struct statvfs space;
    const char *body = result == ENOSPC && statvfs("/profile", &space) == 0 && space.f_bavail == 0 ?
        "<title>disk full</title>" : "<title>disk fill failed</title>";
    char response[256];
    int length = snprintf(response, sizeof(response), "HTTP/1.1 200 OK\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n%s", strlen(body), body);
    if (write(client, response, (size_t)length) != length)
        return 1;
    close(client);
    close(server);
    int status;
    if (waitpid(child, &status, 0) < 0)
        return 1;
    return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}
