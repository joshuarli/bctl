#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static long free_kib(void)
{
	char buf[1024] = {0};
	int fd = open("/proc/meminfo", O_RDONLY);
	ssize_t n = fd < 0 ? -1 : read(fd, buf, sizeof buf - 1);
	if (fd >= 0)
		close(fd);
	char *p = n > 0 ? strstr(buf, "MemFree:") : 0;
	return p ? strtol(p + 8, 0, 10) : -1;
}

int main(void)
{
	alarm(30);
	int fd = memfd_create("unmap-empty", MFD_CLOEXEC);
	if (fd < 0 || ftruncate(fd, 4096))
		return 1;
	long before = free_kib();
	int ok = 1;
	for (int i = 0; i < 8192 && ok; i++) {
		void *anon = mmap(0, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		void *file = mmap(0, 4096, PROT_READ, MAP_PRIVATE, fd, 0);
		ok = anon != MAP_FAILED && file != MAP_FAILED;
		if (anon != MAP_FAILED)
			ok &= munmap(anon, 4096) == 0;
		if (file != MAP_FAILED)
			ok &= munmap(file, 4096) == 0;
	}
	close(fd);
	long after = free_kib();
	ok &= before > 0 && after > 0 && before - after <= 1024;
	printf("MINI_CHROMIUM_TEST name=unmap_empty status=%s detail=before_kib=%ld,after_kib=%ld,delta_kib=%ld\n",
	       ok ? "PASS" : "FAIL", before, after, before - after);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
