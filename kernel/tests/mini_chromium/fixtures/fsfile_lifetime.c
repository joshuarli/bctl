#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <unistd.h>

#define CHURNERS 2
#define SPRAYERS 2
#define ROUNDS 20000
#define QUEUED 32

static atomic_int churning = CHURNERS;

static void *churn(void *unused)
{
	(void)unused;
	char data[64];
	memset(data, 'c', sizeof data);
	void *result = 0;
	for (int i = 0; i < ROUNDS; i++) {
		int fd = memfd_create("fsfile_lifetime", MFD_CLOEXEC);
		if (fd < 0 || write(fd, data, sizeof data) != sizeof data || close(fd)) {
			result = (void *)1;
			break;
		}
	}
	atomic_fetch_sub(&churning, 1);
	return result;
}

/* Fills freed kernel heap objects with bytes whose low bits mark a closure as heap-owned. */
static void *spray(void *unused)
{
	(void)unused;
	int sv[2];
	if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sv))
		return (void *)1;
	static char data[4096];
	memset(data, 'c', sizeof data);
	char sink[4096];
	size_t len = 32;
	while (atomic_load(&churning)) {
		for (int i = 0; i < QUEUED; i++) {
			len = len * 5 % 4061 + 32;
			if (send(sv[0], data, len, 0) < 0)
				return (void *)1;
		}
		for (int i = 0; i < QUEUED; i++)
			if (recv(sv[1], sink, sizeof sink, 0) < 0)
				return (void *)1;
	}
	close(sv[0]);
	close(sv[1]);
	return 0;
}

int main(void)
{
	alarm(80);
	pthread_t threads[CHURNERS + SPRAYERS];
	int started = 0, ok = 1;
	for (; started < CHURNERS + SPRAYERS; started++)
		if (pthread_create(&threads[started], 0, started < CHURNERS ? churn : spray, 0)) {
			ok = 0;
			atomic_store(&churning, 0);
			break;
		}
	for (int i = 0; i < started; i++) {
		void *result;
		if (pthread_join(threads[i], &result) || result)
			ok = 0;
	}
	printf("MINI_CHROMIUM_TEST name=fsfile_lifetime status=%s detail=memfds%d\n", ok ? "PASS" : "FAIL",
	       CHURNERS * ROUNDS);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
