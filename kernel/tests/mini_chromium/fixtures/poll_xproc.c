#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define SENTINEL 0xa5
#define PHASES 3

static struct {
	struct epoll_event events[4];
	fd_set rset;
	struct pollfd pfd[2];
} results;

static int passed, failed;

static void record(const char *name, int ok, const char *detail)
{
	char line[256];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name,
		ok ? "PASS" : "FAIL", detail);
	write(1, line, (size_t)n);
	ok ? passed++ : failed++;
}

static int untouched(void)
{
	const unsigned char *p = (const unsigned char *)&results;
	for (size_t i = 0; i < sizeof results; i++)
		if (p[i] != SENTINEL)
			return 0;
	return 1;
}

static void writer(int ctl, int report, int data)
{
	unsigned char clean[PHASES];
	memset(&results, SENTINEL, sizeof results);
	for (int phase = 0; phase < PHASES; phase++) {
		char go;
		if (read(ctl, &go, 1) != 1)
			_exit(1);
		struct timespec delay = { 0, 50 * 1000 * 1000 };
		nanosleep(&delay, 0);
		if (write(data, "d", 1) != 1)
			_exit(2);
		nanosleep(&delay, 0);
		clean[phase] = (unsigned char)untouched();
	}
	write(report, clean, sizeof clean);
	_exit(0);
}

static void drain(int fd)
{
	char c;
	read(fd, &c, 1);
}

static void check_unix_stream_writable(const char *name, int use_poll)
{
	char detail[128];
	int sv[2];
	if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0, sv)) {
		snprintf(detail, sizeof detail, "socketpair-errno=%d", errno);
		record(name, 0, detail);
		return;
	}
	static char chunk[65536];
	long filled = 0;
	ssize_t w;
	while ((w = write(sv[0], chunk, sizeof chunk)) > 0)
		filled += w;
	int full = w < 0 && errno == EAGAIN && filled > 0;
	long pid = syscall(SYS_clone, SIGCHLD, 0, 0, 0, 0);
	if (pid == 0) {
		struct timespec delay = { 0, 100 * 1000 * 1000 };
		nanosleep(&delay, 0);
		long drained = 0;
		while (drained < filled) {
			ssize_t r = read(sv[1], chunk, sizeof chunk);
			if (r > 0)
				drained += r;
			else if (r < 0 && errno != EAGAIN)
				_exit(1);
		}
		pause();
		_exit(0);
	}
	int n, revents;
	if (use_poll) {
		struct pollfd pfd = { .fd = sv[0], .events = POLLIN | POLLOUT };
		n = poll(&pfd, 1, 5000);
		revents = pfd.revents;
	} else {
		int ep = epoll_create1(0);
		struct epoll_event ev = { .events = EPOLLIN | EPOLLOUT, .data.u64 = 9 }, got = { 0 };
		epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev);
		n = epoll_wait(ep, &got, 1, 5000);
		revents = (int)got.events;
		close(ep);
	}
	if (pid > 0) {
		kill((pid_t)pid, SIGKILL);
		waitpid((pid_t)pid, 0, 0);
	}
	close(sv[0]);
	close(sv[1]);
	snprintf(detail, sizeof detail, "filled=%ld,full=%d,child=%ld,n=%d,revents=0x%x", filled, full, pid, n, revents);
	record(name, full && pid > 0 && n == 1 && (revents & EPOLLOUT), detail);
}

int main(void)
{
	char detail[128];
	int data[2], ctl[2], report[2], idle[2];
	if (pipe(data) || pipe(ctl) || pipe(report) || pipe(idle)) {
		record("poll_xproc_pipes", 0, "pipe-failed");
		goto out;
	}
	long pid = syscall(SYS_clone, SIGCHLD, 0, 0, 0, 0);
	if (pid < 0) {
		snprintf(detail, sizeof detail, "errno=%d", errno);
		record("poll_xproc_fork", 0, detail);
		goto out;
	}
	if (pid == 0) {
		close(data[0]);
		close(ctl[1]);
		close(report[0]);
		writer(ctl[0], report[1], data[1]);
	}
	close(data[1]);
	close(ctl[0]);
	close(report[1]);

	int ep = epoll_create1(0);
	struct epoll_event ev = { .events = EPOLLIN, .data.u64 = 0x1122334455667788ull };
	struct epoll_event other = { .events = EPOLLIN, .data.u64 = 7 };
	epoll_ctl(ep, EPOLL_CTL_ADD, data[0], &ev);
	epoll_ctl(ep, EPOLL_CTL_ADD, idle[0], &other);
	memset(&results, 0, sizeof results);
	write(ctl[1], "g", 1);
	int n = epoll_wait(ep, results.events, 4, 5000);
	snprintf(detail, sizeof detail, "n=%d,events=0x%x,data=0x%llx", n, n > 0 ? results.events[0].events : 0,
		n > 0 ? (unsigned long long)results.events[0].data.u64 : 0ull);
	record("poll_xproc_epoll_wait", n == 1 && (results.events[0].events & EPOLLIN) &&
		results.events[0].data.u64 == ev.data.u64, detail);
	drain(data[0]);

	FD_ZERO(&results.rset);
	FD_SET(data[0], &results.rset);
	FD_SET(idle[0], &results.rset);
	int nfds = (data[0] > idle[0] ? data[0] : idle[0]) + 1;
	struct timeval tv = { 5, 0 };
	write(ctl[1], "g", 1);
	n = select(nfds, &results.rset, 0, 0, &tv);
	int data_set = FD_ISSET(data[0], &results.rset), idle_set = FD_ISSET(idle[0], &results.rset);
	snprintf(detail, sizeof detail, "n=%d,data=%d,idle=%d", n, data_set, idle_set);
	record("poll_xproc_select", n == 1 && data_set && !idle_set, detail);
	drain(data[0]);

	results.pfd[0] = (struct pollfd){ .fd = idle[0], .events = POLLIN, .revents = 0x55 };
	results.pfd[1] = (struct pollfd){ .fd = data[0], .events = POLLIN, .revents = 0x55 };
	write(ctl[1], "g", 1);
	n = poll(results.pfd, 2, 5000);
	snprintf(detail, sizeof detail, "n=%d,idle=0x%x,data=0x%x", n, results.pfd[0].revents, results.pfd[1].revents);
	record("poll_xproc_poll", n == 1 && results.pfd[0].revents == 0 && (results.pfd[1].revents & POLLIN), detail);
	drain(data[0]);

	unsigned char clean[PHASES] = { 0 };
	int got = read(report[0], clean, sizeof clean) == (ssize_t)sizeof clean;
	snprintf(detail, sizeof detail, "report=%d,epoll=%d,select=%d,poll=%d", got, clean[0], clean[1], clean[2]);
	record("poll_xproc_writer_memory_untouched", got && clean[0] && clean[1] && clean[2], detail);
	check_unix_stream_writable("poll_xproc_unix_stream_epollout", 0);
	check_unix_stream_writable("poll_xproc_unix_stream_pollout", 1);
out:;
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
