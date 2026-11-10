#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define ROUNDS 300
#define CHUNK 65536
#define CHUNKS 4

static int finish(const char *detail, int ok)
{
	char line[256];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=unix_read_into_cow_pages_while_polled status=%s detail=%s\n",
		ok ? "PASS" : "FAIL", detail);
	write(1, line, (size_t)n);
	n = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	write(1, line, (size_t)n);
	return ok ? 0 : 1;
}

static atomic_int stop;
static int poll_epfd;

static void *poller(void *arg)
{
	(void)arg;
	struct epoll_event ev[2];
	while (!atomic_load(&stop))
		epoll_wait(poll_epfd, ev, 2, 0);
	return 0;
}

static int child(int fd, unsigned char *cow)
{
	poll_epfd = epoll_create1(0);
	struct epoll_event ev = { .events = EPOLLIN | EPOLLOUT, .data.fd = fd };
	if (poll_epfd < 0 || epoll_ctl(poll_epfd, EPOLL_CTL_ADD, fd, &ev))
		return 10;
	pthread_t t;
	if (pthread_create(&t, 0, poller, 0))
		return 11;
	int rv = 0;
	for (int i = 0; i < CHUNKS && !rv; i++) {
		size_t got = 0;
		while (got < CHUNK) {
			ssize_t n = read(fd, cow + i * CHUNK + got, CHUNK - got);
			if (n <= 0) {
				rv = 12;
				break;
			}
			got += (size_t)n;
		}
		if (!rv && cow[i * CHUNK] != (unsigned char)(i + 1))
			rv = 13;
	}
	atomic_store(&stop, 1);
	pthread_join(t, 0);
	return rv;
}

int main(void)
{
	char detail[128];
	unsigned char *cow = mmap(0, CHUNK * CHUNKS, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	static unsigned char chunk[CHUNK];
	if (cow == MAP_FAILED) {
		snprintf(detail, sizeof detail, "mmap-errno=%d", errno);
		return finish(detail, 0);
	}
	for (int round = 0; round < ROUNDS; round++) {
		memset(cow, 0xee, CHUNK * CHUNKS);
		int sv[2];
		if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) {
			snprintf(detail, sizeof detail, "round=%d socketpair-errno=%d", round, errno);
			return finish(detail, 0);
		}
		pid_t pid = fork();
		if (pid == 0) {
			close(sv[0]);
			_exit(child(sv[1], cow));
		}
		close(sv[1]);
		for (int i = 0; i < CHUNKS; i++) {
			memset(chunk, i + 1, sizeof chunk);
			for (size_t sent = 0; sent < sizeof chunk;) {
				ssize_t n = write(sv[0], chunk + sent, sizeof chunk - sent);
				if (n <= 0) {
					snprintf(detail, sizeof detail, "round=%d write-errno=%d", round, errno);
					return finish(detail, 0);
				}
				sent += (size_t)n;
			}
		}
		int status;
		if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status)) {
			snprintf(detail, sizeof detail, "round=%d child-status=0x%x", round, status);
			return finish(detail, 0);
		}
		close(sv[0]);
	}
	snprintf(detail, sizeof detail, "rounds=%d", ROUNDS);
	return finish(detail, 1);
}
