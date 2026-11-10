#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int link_is(const char *path, const char *target)
{
    char value[128];
    ssize_t length = readlink(path, value, sizeof value);
    return length == (ssize_t)strlen(target) && !memcmp(value, target, (size_t)length);
}

int main(void)
{
    int fd = open("/data/relative", O_RDONLY);
    char value[5];
    int content = fd >= 0 && read(fd, value, sizeof value) == sizeof value && !memcmp(value, "hello", sizeof value);
    if (fd >= 0)
        close(fd);
    int ok = link_is("/data/relative", "target") && link_is("/data/absolute", "/data/target") && content;
    printf("MINI_CHROMIUM_TEST name=image_links status=%s\n", ok ? "PASS" : "FAIL");
    printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
    return !ok;
}
