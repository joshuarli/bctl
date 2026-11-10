#define _GNU_SOURCE
#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define ROUNDS 12
#define MAX_RTT_MS 50.0

static const size_t sizes[] = {100, 1460, 1700, 2000, 2920, 3100, 16384, 70000};
static int passed, failed;
static char buf[2][1 << 17];

static void record(const char *name, int ok, const char *detail)
{
	printf("MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name, ok ? "PASS" : "FAIL", detail);
	fflush(stdout);
	ok ? passed++ : failed++;
}

static double now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

static int read_full(int fd, char *p, size_t n)
{
	while (n) {
		ssize_t r = read(fd, p, n);
		if (r <= 0)
			return -1;
		p += r;
		n -= (size_t)r;
	}
	return 0;
}

static int write_full(int fd, const char *p, size_t n)
{
	while (n) {
		ssize_t r = write(fd, p, n);
		if (r <= 0)
			return -1;
		p += r;
		n -= (size_t)r;
	}
	return 0;
}

static void *echo(void *arg)
{
	int c = accept(*(int *)arg, 0, 0);
	size_t n;
	while (c >= 0 && read_full(c, (char *)&n, sizeof n) == 0 && n <= sizeof buf[1] && read_full(c, buf[1], n) == 0)
		if (write_full(c, buf[1], n))
			break;
	if (c >= 0)
		close(c);
	return 0;
}

static void check_request_reply(int nodelay)
{
	char name[64], detail[256];
	struct sockaddr_in a = {.sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
	socklen_t len = sizeof a;
	int l = socket(AF_INET, SOCK_STREAM, 0);
	snprintf(name, sizeof name, "tcp_loopback_rtt_nodelay%d", nodelay);
	if (l < 0 || bind(l, (struct sockaddr *)&a, sizeof a) || listen(l, 1) || getsockname(l, (struct sockaddr *)&a, &len)) {
		record(name, 0, "listener");
		return;
	}
	pthread_t t;
	pthread_create(&t, 0, echo, &l);
	int c = socket(AF_INET, SOCK_STREAM, 0);
	if (connect(c, (struct sockaddr *)&a, sizeof a)) {
		record(name, 0, "connect");
		return;
	}
	setsockopt(c, IPPROTO_TCP, TCP_NODELAY, &nodelay, sizeof nodelay);
	int ok = 1, off = 0;
	off += snprintf(detail + off, sizeof detail - off, "max_ms");
	for (size_t s = 0; s < sizeof sizes / sizeof *sizes; s++) {
		double worst = 0;
		fflush(stdout);
		for (int round = 0; round < ROUNDS && ok; round++) {
			size_t n = sizes[s];
			memcpy(buf[0], &n, sizeof n);
			double start = now_ms();
			if (write_full(c, buf[0], sizeof n + n) || read_full(c, buf[0], n)) {
				ok = 0;
				break;
			}
			double rtt = now_ms() - start;
			worst = rtt > worst ? rtt : worst;
		}
		ok = ok && worst < MAX_RTT_MS;
		off += snprintf(detail + off, sizeof detail - off, ",%zu:%.1f", sizes[s], worst);
	}
	close(c);
	pthread_join(t, 0);
	close(l);
	record(name, ok, detail);
}

int main(void)
{
	alarm(40);
	check_request_reply(0);
	check_request_reply(1);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	return failed ? 1 : 0;
}
