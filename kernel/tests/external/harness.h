#ifndef EXTERNAL_HARNESS_H
#define EXTERNAL_HARNESS_H

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define WATCHDOG_SECONDS 10
#define MAX_TRACKED_CHILDREN 8

struct test_case {
	const char *name;
	void (*run)(void);
};

static char failure_detail[256];
static int failure_recorded;
static sigjmp_buf watchdog_jump;
static pid_t tracked_children[MAX_TRACKED_CHILDREN];

static void emit(const char *line, int len)
{
	while (len > 0) {
		ssize_t n = write(1, line, (size_t)len);
		if (n < 0) {
			if (errno == EINTR)
				continue;
			return;
		}
		line += n;
		len -= (int)n;
	}
}

static void record_failure(int line, const char *expr)
{
	if (failure_recorded)
		return;
	failure_recorded = 1;
	snprintf(failure_detail, sizeof failure_detail, "line %d: %s (errno=%d)", line, expr, errno);
	for (char *p = failure_detail; *p; p++)
		if (*p == '\n' || *p == '\r')
			*p = ' ';
}

#define EXPECT(cond)                                         \
	do {                                                 \
		if (!(cond))                                 \
			record_failure(__LINE__, #cond);     \
	} while (0)

#define ASSERT(cond)                                         \
	do {                                                 \
		if (!(cond)) {                               \
			record_failure(__LINE__, #cond);     \
			return;                              \
		}                                            \
	} while (0)

#define CHILD_CHECK(cond)                                    \
	do {                                                 \
		if (!(cond))                                 \
			_exit(100 + (__LINE__ % 100));       \
	} while (0)

static void track_child(pid_t pid)
{
	for (int i = 0; i < MAX_TRACKED_CHILDREN; i++) {
		if (tracked_children[i] == 0) {
			tracked_children[i] = pid;
			return;
		}
	}
}

static void untrack_child(pid_t pid)
{
	for (int i = 0; i < MAX_TRACKED_CHILDREN; i++)
		if (tracked_children[i] == pid)
			tracked_children[i] = 0;
}

static void reap_tracked_children(void)
{
	for (int i = 0; i < MAX_TRACKED_CHILDREN; i++) {
		if (tracked_children[i] > 0) {
			kill(tracked_children[i], SIGKILL);
			waitpid(tracked_children[i], NULL, 0);
			tracked_children[i] = 0;
		}
	}
}

static pid_t fork_tracked(void)
{
	pid_t pid = fork();
	if (pid > 0)
		track_child(pid);
	return pid;
}

static pid_t wait_tracked(pid_t pid, int *status)
{
	pid_t r;
	do {
		r = waitpid(pid, status, 0);
	} while (r < 0 && errno == EINTR);
	if (r == pid)
		untrack_child(pid);
	return r;
}

static int wait_exit_code(pid_t pid)
{
	int status;
	if (wait_tracked(pid, &status) != pid)
		return -1;
	if (WIFEXITED(status))
		return WEXITSTATUS(status);
	return 256 + WTERMSIG(status);
}

static void fill_pattern(char *buf, size_t len, unsigned seed)
{
	for (size_t i = 0; i < len; i++)
		buf[i] = (char)((seed * 131u + i * 17u + 7u) & 0xff);
}

static ssize_t read_full(int fd, void *buf, size_t len)
{
	size_t done = 0;
	while (done < len) {
		ssize_t n = read(fd, (char *)buf + done, len - done);
		if (n < 0) {
			if (errno == EINTR)
				continue;
			return -1;
		}
		if (n == 0)
			break;
		done += (size_t)n;
	}
	return (ssize_t)done;
}

static ssize_t write_full(int fd, const void *buf, size_t len)
{
	size_t done = 0;
	while (done < len) {
		ssize_t n = write(fd, (const char *)buf + done, len - done);
		if (n < 0) {
			if (errno == EINTR)
				continue;
			return -1;
		}
		done += (size_t)n;
	}
	return (ssize_t)done;
}

static int open_scratch_file(int flags, size_t size)
{
	char path[64];
	char block[512];
	int fd;
	size_t done = 0;
	snprintf(path, sizeof path, "external_scratch_%d.tmp", (int)getpid());
	fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (fd < 0)
		return -1;
	memset(block, 'x', sizeof block);
	while (done < size) {
		size_t n = size - done < sizeof block ? size - done : sizeof block;
		if (write(fd, block, n) != (ssize_t)n) {
			close(fd);
			unlink(path);
			return -1;
		}
		done += n;
	}
	close(fd);
	fd = open(path, flags);
	unlink(path);
	return fd;
}

static int same_file(int a, int b)
{
	struct stat sa, sb;
	if (fstat(a, &sa) < 0 || fstat(b, &sb) < 0)
		return 0;
	return sa.st_dev == sb.st_dev && sa.st_ino == sb.st_ino;
}

static void on_watchdog(int sig)
{
	(void)sig;
	siglongjmp(watchdog_jump, 1);
}

static int run_cases(const struct test_case *cases, int count)
{
	int passed = 0, failed = 0;
	struct sigaction sa;
	memset(&sa, 0, sizeof sa);
	sa.sa_handler = on_watchdog;
	sigemptyset(&sa.sa_mask);
	sigaction(SIGALRM, &sa, NULL);
	signal(SIGPIPE, SIG_IGN);

	for (int i = 0; i < count; i++) {
		char line[512];
		int len, ok;
		failure_recorded = 0;
		failure_detail[0] = 0;
		if (sigsetjmp(watchdog_jump, 1) == 0) {
			alarm(WATCHDOG_SECONDS);
			cases[i].run();
			alarm(0);
		} else {
			failure_recorded = 1;
			snprintf(failure_detail, sizeof failure_detail, "watchdog");
		}
		reap_tracked_children();
		ok = !failure_recorded;
		len = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", cases[i].name,
			ok ? "PASS" : "FAIL", ok ? "ok" : failure_detail);
		emit(line, len);
		ok ? passed++ : failed++;
	}
	{
		char line[128];
		int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n",
			failed ? "FAIL" : "PASS", passed, failed);
		emit(line, len);
	}
	return failed ? 1 : 0;
}

#endif
