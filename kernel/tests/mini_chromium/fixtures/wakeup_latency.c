#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum { ROUNDS = 20000 };

static long long now_ns(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int cmp(const void *a, const void *b)
{
	long long x = *(const long long *)a, y = *(const long long *)b;
	return (x > y) - (x < y);
}

int main(void)
{
	int sv[2];
	char c = 0;
	static long long rtt[ROUNDS];
	if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) {
		printf("MINI_CHROMIUM_RESULT status=FAIL passed=0 failed=1\n");
		return 1;
	}
	pid_t pid = fork();
	if (pid == 0) {
		for (int i = 0; i < ROUNDS; i++) {
			if (read(sv[1], &c, 1) != 1 || write(sv[1], &c, 1) != 1)
				_exit(1);
		}
		_exit(0);
	}
	for (int i = 0; i < ROUNDS; i++) {
		long long t = now_ns();
		if (write(sv[0], &c, 1) != 1 || read(sv[0], &c, 1) != 1) {
			printf("MINI_CHROMIUM_RESULT status=FAIL passed=0 failed=1\n");
			return 1;
		}
		rtt[i] = now_ns() - t;
	}
	int status;
	waitpid(pid, &status, 0);
	qsort(rtt, ROUNDS, sizeof(rtt[0]), cmp);
	printf("MINI_CHROMIUM_TEST name=wakeup_latency status=PASS detail=p50_us=%.1f,p90_us=%.1f,p99_us=%.1f\n",
	       rtt[ROUNDS / 2] / 1000.0, rtt[ROUNDS * 9 / 10] / 1000.0, rtt[ROUNDS * 99 / 100] / 1000.0);
	printf("MINI_CHROMIUM_RESULT status=PASS passed=1 failed=0\n");
	return 0;
}
