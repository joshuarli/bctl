#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static long field_kib(const char *text, const char *name)
{
	const char *at = strstr(text, name);
	return at ? strtol(at + strlen(name), 0, 10) : -1;
}

int main(void)
{
	char text[4096], line[256];
	int fd = open("/proc/meminfo", O_RDONLY);
	ssize_t n = fd < 0 ? -1 : read(fd, text, sizeof text - 1);
	text[n < 0 ? 0 : n] = 0;
	long total = field_kib(text, "MemTotal:"), avail = field_kib(text, "MemAvailable:"), free_kib = field_kib(text, "MemFree:");
	int ok = total > 0 && free_kib > 0;
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=meminfo status=%s detail=total_kib=%ld,free_kib=%ld,available_kib=%ld\n",
		ok ? "PASS" : "FAIL", total, free_kib, avail);
	write(1, line, len);
	len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	write(1, line, len);
	return !ok;
}
