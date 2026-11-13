#include <pthread.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static void *worker(void *arg)
{
	(void)arg;
	for (int i = 0; i < 10000; i++) {
		char *p = mmap(0, 12288, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (p == MAP_FAILED)
			return (void *)1;
		int ok = mprotect(p + 4096, 4096, PROT_READ) == 0 &&
			mprotect(p, 12288, PROT_READ | PROT_WRITE) == 0;
		if (ok)
			p[4096] = 1;
		ok &= munmap(p, 12288) == 0;
		if (!ok)
			return (void *)1;
	}
	return 0;
}

int main(void)
{
	alarm(60);
	pthread_t threads[4];
	int started = 0, ok = 1;
	for (; started < 4; started++)
		if (pthread_create(&threads[started], 0, worker, 0))
			break;
	ok = started == 4;
	for (int i = 0; i < started; i++) {
		void *result = (void *)1;
		ok &= pthread_join(threads[i], &result) == 0 && !result;
	}
	printf("MINI_CHROMIUM_TEST name=mprotect_merge status=%s\n", ok ? "PASS" : "FAIL");
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
