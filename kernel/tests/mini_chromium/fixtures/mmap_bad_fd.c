#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void)
{
	int ebadf = 1;
	for (int i = 0; i < 2; i++) {
		errno = 0;
		void *p = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, -1, 0);
		ebadf = ebadf && p == MAP_FAILED && errno == EBADF;
		errno = 0;
		p = mmap(0, 4096, PROT_READ, MAP_PRIVATE, 1000, 0);
		ebadf = ebadf && p == MAP_FAILED && errno == EBADF;
	}
	char *anon = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	int after = anon != MAP_FAILED && !munmap(anon, 4096);
	printf("MINI_CHROMIUM_TEST name=mmap_bad_fd status=%s detail=ebadf=%d after=%d\n", ebadf && after ? "PASS" : "FAIL",
		ebadf, after);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ebadf && after ? "PASS" : "FAIL", ebadf && after,
		!(ebadf && after));
	return ebadf && after ? 0 : 1;
}
