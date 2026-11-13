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

#define ROUNDS 100
#define CHUNK 65536
#define CHUNKS 4

static int finish(const char *detail, int ok)
{
	char line[256];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=vsock_read_into_cow_pages_while_polled status=%s detail=%s\n",
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
		for (size_t j = 0; !rv && j < CHUNK; j++)
			if (cow[i * CHUNK + j] != (unsigned char)(i + 1))
				rv = 13;
	}
	atomic_store(&stop, 1);
	pthread_join(t, 0);
	return rv;
}

struct address { unsigned short family, reserved; unsigned int port, cid; unsigned char zero[4]; };
int main(void)
{
    unsigned char *cow = mmap(0, CHUNK * CHUNKS, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int listener = socket(40, SOCK_STREAM, 0);
    struct address a = { .family = 40, .port = 9224, .cid = 0xffffffff };
    if (cow == MAP_FAILED || listener < 0 || bind(listener, (void *)&a, sizeof(a)) || listen(listener, 8))
        return finish("setup", 0);
    puts("cow-ready"); fflush(stdout);
    for (int round = 0; round < ROUNDS; round++) {
        memset(cow, 0xee, CHUNK * CHUNKS);
        int fd = accept(listener, 0, 0);
        if (fd < 0) return finish("accept", 0);
        pid_t pid = fork();
        if (pid == 0) {
            int result = child(fd, cow);
            if (!result && write(fd, "done\n", 5) != 5) result = 14;
            _exit(result);
        }
        int status = 0;
        if (pid < 0 || waitpid(pid, &status, 0) != pid || status) return finish("child", 0);
        for (size_t j = 0; j < CHUNK * CHUNKS; j++)
            if (cow[j] != 0xee) return finish("parent-cow", 0);
        close(fd);
    }
    return finish("rounds=100", 1);
}
