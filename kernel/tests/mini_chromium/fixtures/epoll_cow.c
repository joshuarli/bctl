#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

enum { PAGE = 4096, PAGES = 16384, ACTORS = 3 };
static atomic_int start, stop, failed, ready;
static int sv[2], epfd;
static uint64_t work[ACTORS];

static void *actor(void *arg)
{
	uintptr_t id = (uintptr_t)arg;
	unsigned char buf[4096] = { 0 };
	struct epoll_event events[2];
	atomic_fetch_add(&ready, 1);
	while (!atomic_load(&start))
		sched_yield();
	while (!atomic_load(&stop)) {
		ssize_t n;
		if (id == 0)
			n = epoll_wait(epfd, events, 2, 0);
		else if (id == 1)
			n = send(sv[1], buf, sizeof buf, MSG_NOSIGNAL);
		else
			n = recv(sv[0], buf, sizeof buf, 0);
		if (n > 0)
			work[id] += (uint64_t)n;
		else if (n < 0 && errno != EAGAIN && errno != EINTR) {
			atomic_store(&failed, 1);
			break;
		}
	}
	return 0;
}

int main(void)
{
	unsigned char *cow = mmap(0, PAGES * PAGE, PROT_READ | PROT_WRITE,
		MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	int hold[2], ok = 1, started = 0, status = 0;
	pid_t child = -1;
	pthread_t threads[ACTORS];
	if (cow == MAP_FAILED || pipe(hold))
		return 1;
	memset(cow, 0x5a, PAGES * PAGE);
	child = fork();
	if (!child) {
		close(hold[1]);
		unsigned char byte;
		while (read(hold[0], &byte, 1) < 0 && errno == EINTR) {}
		for (size_t page = 0; page < PAGES; page++)
			if (cow[page * PAGE] != 0x5a)
				_exit(2);
		_exit(0);
	}
	close(hold[0]);
	if (child < 0 || socketpair(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0, sv))
		ok = 0;
	epfd = ok ? epoll_create1(0) : -1;
	struct epoll_event event = { .events = EPOLLIN | EPOLLOUT };
	if (epfd < 0)
		ok = 0;
	for (int i = 0; ok && i < 2; i++) {
		event.data.fd = sv[i];
		if (epoll_ctl(epfd, EPOLL_CTL_ADD, sv[i], &event))
			ok = 0;
	}
	for (; ok && started < ACTORS; started++)
		if (pthread_create(&threads[started], 0, actor, (void *)(uintptr_t)started)) {
			ok = 0;
			break;
		}
	while (ok && atomic_load(&ready) != ACTORS)
		sched_yield();
	atomic_store(&start, 1);
	for (size_t page = 0; ok && page < PAGES; page++)
		cow[page * PAGE] = 0xa5;
	atomic_store(&stop, 1);
	for (int i = 0; i < started; i++)
		if (pthread_join(threads[i], 0))
			ok = 0;
	close(hold[1]);
	if (child > 0 && (waitpid(child, &status, 0) != child || status))
		ok = 0;
	if (epfd >= 0)
		close(epfd);
	if (child > 0) {
		close(sv[0]);
		close(sv[1]);
	}
	munmap(cow, PAGES * PAGE);
	ok = ok && !atomic_load(&failed) && work[0] && work[1] && work[2];
	printf("MINI_CHROMIUM_TEST name=epoll_cow status=%s detail=pages%d,events%llu,sent%llu,received%llu\n",
		ok ? "PASS" : "FAIL", PAGES, (unsigned long long)work[0],
		(unsigned long long)work[1], (unsigned long long)work[2]);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
