#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define MEMBARRIER_CMD_QUERY 0
#define MEMBARRIER_CMD_PRIVATE_EXPEDITED 8
#define MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED 16

static int passed, failed;
static volatile sig_atomic_t sigpipes;
static volatile pid_t sigpipe_tid;

__attribute__((format(printf, 3, 4))) static void record(const char *name, int ok, const char *fmt, ...)
{
	char detail[192], line[320];
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(detail, sizeof detail, fmt, ap);
	va_end(ap);
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name, ok ? "PASS" : "FAIL",
		detail);
	write(1, line, (size_t)n);
	ok ? passed++ : failed++;
}

static void on_sigpipe(int sig)
{
	(void)sig;
	sigpipes++;
	sigpipe_tid = (pid_t)syscall(SYS_gettid);
}

static void pause_ms(long ms)
{
	struct timespec ts = { ms / 1000, (ms % 1000) * 1000 * 1000 };
	nanosleep(&ts, 0);
}

static int reap(pid_t pid)
{
	int status = -1;
	while (waitpid(pid, &status, 0) < 0 && errno == EINTR)
		;
	return status;
}

static unsigned revents(int fd)
{
	struct pollfd p = { .fd = fd, .events = POLLIN | POLLOUT | POLLRDHUP };
	return poll(&p, 1, 0) == 1 ? (unsigned)p.revents : 0;
}

static void epoll_on_child_write(const char *name, int rfd, int wfd, int close_in_child)
{
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event ev = { .events = EPOLLIN, .data.u64 = 0xfeedull };
	epoll_ctl(ep, EPOLL_CTL_ADD, rfd, &ev);
	pid_t pid = fork();
	if (pid == 0) {
		if (close_in_child >= 0)
			close(close_in_child);
		pause_ms(100);
		uint64_t one = 1;
		_exit(write(wfd, &one, sizeof one) == (ssize_t)sizeof one ? 0 : 1);
	}
	struct epoll_event out = { 0 };
	int n = epoll_wait(ep, &out, 1, 5000);
	int status = reap(pid);
	record(name, n == 1 && (out.events & EPOLLIN) && out.data.u64 == 0xfeedull && status == 0,
		"n=%d,events=0x%x,status=0x%x", n, n == 1 ? out.events : 0, status);
	close(ep);
}

static void check_epoll_xproc(void)
{
	int efd = eventfd(0, EFD_CLOEXEC);
	epoll_on_child_write("epoll_eventfd_xproc", efd, efd, -1);
	uint64_t value = 0;
	int got = read(efd, &value, sizeof value) == (ssize_t)sizeof value;
	record("eventfd_xproc_value", got && value == 1, "read=%d,value=%llu", got, (unsigned long long)value);
	close(efd);

	int p[2];
	pipe2(p, O_CLOEXEC);
	epoll_on_child_write("epoll_pipe_xproc", p[0], p[1], p[0]);
	close(p[0]);
	close(p[1]);

	int sv[2];
	socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
	epoll_on_child_write("epoll_socket_xproc", sv[0], sv[1], sv[0]);
	close(sv[0]);
	close(sv[1]);
}

static void check_rdhup_after_peer_shut_wr(void)
{
	int sv[2];
	socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event ev = { .events = EPOLLIN | EPOLLRDHUP, .data.fd = sv[0] };
	epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev);
	pid_t pid = fork();
	if (pid == 0) {
		close(sv[0]);
		pause_ms(100);
		int rc = shutdown(sv[1], SHUT_WR);
		pause_ms(300);
		_exit(rc == 0 ? 0 : 1);
	}
	struct epoll_event out = { 0 };
	int n = epoll_wait(ep, &out, 1, 5000);
	unsigned want = EPOLLIN | EPOLLRDHUP;
	int epoll_ok = n == 1 && (out.events & want) == want && !(out.events & EPOLLHUP);
	unsigned pev = revents(sv[0]);
	int poll_ok = (pev & (POLLIN | POLLRDHUP)) == (POLLIN | POLLRDHUP) && !(pev & POLLHUP) && (pev & POLLOUT);
	fd_set r;
	FD_ZERO(&r);
	FD_SET(sv[0], &r);
	struct timeval tv = { 0, 0 };
	int sel = select(sv[0] + 1, &r, 0, 0, &tv);
	char c;
	ssize_t eof = recv(sv[0], &c, 1, 0);
	int still_writable = send(sv[0], "w", 1, MSG_NOSIGNAL) == 1;
	int status = reap(pid);
	record("rdhup_after_peer_shut_wr", epoll_ok && poll_ok && sel == 1 && eof == 0 && still_writable && status == 0,
		"n=%d,events=0x%x,poll=0x%x,select=%d,recv=%zd,writable=%d,status=0x%x", n, n == 1 ? out.events : 0, pev, sel,
		eof, still_writable, status);
	close(ep);
	close(sv[0]);
	close(sv[1]);
}

static void check_hup_on_peer_exit(void)
{
	int sv[2];
	socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event ev = { .events = EPOLLIN | EPOLLRDHUP, .data.fd = sv[0] };
	epoll_ctl(ep, EPOLL_CTL_ADD, sv[0], &ev);
	pid_t pid = fork();
	if (pid == 0) {
		close(sv[0]);
		pause_ms(100);
		_exit(0);
	}
	close(sv[1]);
	struct epoll_event out = { 0 };
	int n = epoll_wait(ep, &out, 1, 5000);
	unsigned want = EPOLLIN | EPOLLRDHUP | EPOLLHUP;
	unsigned pev = revents(sv[0]);
	char c;
	ssize_t eof = recv(sv[0], &c, 1, 0);
	int status = reap(pid);
	record("hup_on_peer_exit", n == 1 && (out.events & want) == want && (pev & (POLLHUP | POLLRDHUP)) == (POLLHUP | POLLRDHUP) &&
		eof == 0 && status == 0, "n=%d,events=0x%x,poll=0x%x,recv=%zd,status=0x%x", n, n == 1 ? out.events : 0, pev, eof,
		status);
	close(ep);
	close(sv[0]);
}

static void check_shut_rd(void)
{
	int sv[2];
	socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
	send(sv[1], "ab", 2, 0);
	int rc = shutdown(sv[0], SHUT_RD);
	char buf[8] = "";
	ssize_t queued = recv(sv[0], buf, sizeof buf, 0);
	ssize_t eof = recv(sv[0], buf + 2, sizeof buf - 2, 0);
	unsigned own = revents(sv[0]), peer = revents(sv[1]);
	sigpipes = 0;
	errno = 0;
	ssize_t peer_send = send(sv[1], "x", 1, 0);
	int peer_errno = errno;
	record("shut_rd_read_zero", rc == 0 && queued == 2 && !memcmp(buf, "ab", 2) && eof == 0 && peer_send < 0 &&
		peer_errno == EPIPE && sigpipes == 1 && (own & (POLLIN | POLLRDHUP)) == (POLLIN | POLLRDHUP) && !(own & POLLHUP) &&
		!(peer & (POLLHUP | POLLRDHUP)), "rc=%d,queued=%zd,eof=%zd,peer_send=%zd,errno=%d,sigpipes=%d,own=0x%x,peer=0x%x",
		rc, queued, eof, peer_send, peer_errno, (int)sigpipes, own, peer);
	shutdown(sv[0], SHUT_WR);
	own = revents(sv[0]);
	peer = revents(sv[1]);
	record("shut_rdwr_hup", (own & POLLHUP) && (peer & (POLLHUP | POLLRDHUP | POLLIN)) == (POLLHUP | POLLRDHUP | POLLIN),
		"own=0x%x,peer=0x%x", own, peer);
	close(sv[0]);
	close(sv[1]);
}

struct writer_arg {
	int fd;
	int flags;
	int use;
	ssize_t rv;
	int err;
	pid_t tid;
};

static void *shut_wr_writer(void *p)
{
	struct writer_arg *a = p;
	a->tid = (pid_t)syscall(SYS_gettid);
	errno = 0;
	struct iovec iov = { "x", 1 };
	struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1 };
	switch (a->use) {
	case 0: a->rv = write(a->fd, "x", 1); break;
	case 1: a->rv = send(a->fd, "x", 1, a->flags); break;
	case 2: a->rv = sendto(a->fd, "x", 1, a->flags, 0, 0); break;
	default: a->rv = sendmsg(a->fd, &msg, a->flags); break;
	}
	a->err = errno;
	return 0;
}

static void check_shut_wr_sigpipe(void)
{
	static const char *const uses[] = { "write", "send", "sendto", "sendmsg" };
	int sv[2];
	socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
	int rc = shutdown(sv[0], SHUT_WR);
	for (int use = 0; use < 4; use++) {
		for (int nosignal = 0; nosignal < 2; nosignal++) {
			if (use == 0 && nosignal)
				continue;
			struct writer_arg a = { .fd = sv[0], .flags = nosignal ? MSG_NOSIGNAL : 0, .use = use };
			sigpipes = 0;
			sigpipe_tid = 0;
			pthread_t t;
			pthread_create(&t, 0, shut_wr_writer, &a);
			pthread_join(t, 0);
			int want = nosignal ? 0 : 1;
			char name[64];
			snprintf(name, sizeof name, "shut_wr_%s%s", uses[use], nosignal ? "_nosignal" : "_sigpipe");
			record(name, rc == 0 && a.rv < 0 && a.err == EPIPE && sigpipes == want && (!want || sigpipe_tid == a.tid),
				"rv=%zd,errno=%d,sigpipes=%d,on_writer=%d", a.rv, a.err, (int)sigpipes, sigpipe_tid == a.tid);
		}
	}
	close(sv[0]);
	close(sv[1]);

	int sq[2];
	socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sq);
	shutdown(sq[0], SHUT_WR);
	sigpipes = 0;
	errno = 0;
	ssize_t r = send(sq[0], "x", 1, 0);
	record("shut_wr_seqpacket_no_sigpipe", r < 0 && errno == EPIPE && sigpipes == 0, "rv=%zd,errno=%d,sigpipes=%d", r, errno,
		(int)sigpipes);
	close(sq[0]);
	close(sq[1]);
}

static void check_shutdown_einval(void)
{
	int sv[2];
	socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
	errno = 0;
	int three = shutdown(sv[0], 3);
	int e3 = errno;
	errno = 0;
	int neg = shutdown(sv[0], -1);
	int en = errno;
	int intact = send(sv[0], "k", 1, MSG_NOSIGNAL) == 1;
	record("shutdown_einval", three < 0 && e3 == EINVAL && neg < 0 && en == EINVAL && intact, "three=%d/%d,neg=%d/%d,intact=%d",
		three, e3, neg, en, intact);
	close(sv[0]);
	close(sv[1]);
}

static void blocking_read_woken(const char *name, int rfd, int wfd, int other, int by_shutdown)
{
	pid_t pid = fork();
	if (pid == 0) {
		close(other);
		pause_ms(150);
		if (by_shutdown)
			_exit(shutdown(wfd, SHUT_WR) == 0 ? 0 : 1);
		_exit(write(wfd, "z", 1) == 1 ? 0 : 1);
	}
	char c = 0;
	ssize_t r = read(rfd, &c, 1);
	int status = reap(pid);
	int ok = by_shutdown ? r == 0 : (r == 1 && c == 'z');
	record(name, ok && status == 0, "read=%zd,status=0x%x", r, status);
}

static void check_blocking_reader_xproc(void)
{
	int sv[2];
	socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
	blocking_read_woken("blocking_socket_reader_xproc", sv[0], sv[1], sv[0], 0);
	blocking_read_woken("blocking_reader_woken_by_shut_wr", sv[0], sv[1], sv[0], 1);
	close(sv[0]);
	close(sv[1]);
	int p[2];
	pipe2(p, O_CLOEXEC);
	blocking_read_woken("blocking_pipe_reader_xproc", p[0], p[1], p[0], 0);
	close(p[0]);
	close(p[1]);
	pipe2(p, O_CLOEXEC);
	pid_t pid = fork();
	if (pid == 0) {
		close(p[0]);
		pause_ms(150);
		_exit(close(p[1]) == 0 ? 0 : 1);
	}
	close(p[1]);
	char c;
	errno = 0;
	ssize_t r = read(p[0], &c, 1);
	int e = errno;
	int status = reap(pid);
	record("blocking_pipe_reader_eof_on_last_writer_close", r == 0 && status == 0, "read=%zd,errno=%d,status=0x%x", r, e,
		status);
	close(p[0]);
}

static void check_pipe_sigpipe(void)
{
	int p[2];
	pipe2(p, O_CLOEXEC);
	close(p[0]);
	sigpipes = 0;
	errno = 0;
	ssize_t r = write(p[1], "x", 1);
	int e = errno;
	record("pipe_closed_reader_sigpipe", r < 0 && e == EPIPE && sigpipes == 1, "rv=%zd,errno=%d,sigpipes=%d", r, e,
		(int)sigpipes);
	signal(SIGPIPE, SIG_IGN);
	errno = 0;
	r = write(p[1], "x", 1);
	e = errno;
	signal(SIGPIPE, on_sigpipe);
	record("pipe_closed_reader_ignored", r < 0 && e == EPIPE && sigpipes == 1, "rv=%zd,errno=%d,sigpipes=%d", r, e,
		(int)sigpipes);
	close(p[1]);

	pipe2(p, O_CLOEXEC);
	pid_t pid = fork();
	if (pid == 0) {
		signal(SIGPIPE, SIG_DFL);
		close(p[0]);
		for (;;)
			if (write(p[1], "x", 1) < 0)
				_exit(1);
	}
	pause_ms(100);
	close(p[0]);
	int status = reap(pid);
	close(p[1]);
	record("pipe_sigpipe_default_kills", WIFSIGNALED(status) && WTERMSIG(status) == SIGPIPE, "status=0x%x", status);

	pipe2(p, O_CLOEXEC);
	pid = fork();
	if (pid == 0) {
		close(p[1]);
		pause_ms(100);
		_exit(0);
	}
	close(p[0]);
	sigpipes = 0;
	errno = 0;
	ssize_t first = write(p[1], "a", 1);
	reap(pid);
	r = write(p[1], "b", 1);
	e = errno;
	record("pipe_reader_exit_xproc", first == 1 && r < 0 && e == EPIPE && sigpipes == 1, "first=%zd,rv=%zd,errno=%d,sigpipes=%d",
		first, r, e, (int)sigpipes);
	close(p[1]);
}

static void check_msg_dontwait(void)
{
	int sv[2];
	socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
	char buf[512];
	errno = 0;
	ssize_t r = recv(sv[0], buf, sizeof buf, MSG_DONTWAIT);
	int recv_eagain = r < 0 && errno == EAGAIN;
	struct iovec iov = { buf, sizeof buf };
	struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1 };
	errno = 0;
	r = recvmsg(sv[0], &msg, MSG_DONTWAIT);
	int recvmsg_eagain = r < 0 && errno == EAGAIN;
	memset(buf, 'q', sizeof buf);
	long sent = 0;
	int send_eagain = 0;
	for (int i = 0; i < 100000; i++) {
		ssize_t s = send(sv[1], buf, sizeof buf, MSG_DONTWAIT);
		if (s < 0) {
			send_eagain = errno == EAGAIN;
			break;
		}
		sent += s;
	}
	errno = 0;
	r = sendmsg(sv[1], &msg, MSG_DONTWAIT);
	int sendmsg_eagain = r < 0 && errno == EAGAIN;
	record("msg_dontwait", recv_eagain && recvmsg_eagain && send_eagain && sendmsg_eagain && sent > 0,
		"recv=%d,recvmsg=%d,send=%d,sendmsg=%d,sent=%ld", recv_eagain, recvmsg_eagain, send_eagain, sendmsg_eagain, sent);
	close(sv[0]);
	close(sv[1]);
}

static volatile int spin_stop;

static void *spinner(void *p)
{
	(void)p;
	while (!__atomic_load_n(&spin_stop, __ATOMIC_RELAXED))
		;
	return 0;
}

static long membarrier(int cmd, unsigned flags)
{
	return syscall(SYS_membarrier, cmd, flags, 0);
}

static void check_membarrier(void)
{
	long query = membarrier(MEMBARRIER_CMD_QUERY, 0);
	int want = MEMBARRIER_CMD_PRIVATE_EXPEDITED | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED;
	record("membarrier_query", query >= 0 && (query & want) == want, "query=0x%lx", query);

	errno = 0;
	long unreg = membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0);
	int unreg_errno = errno;
	record("membarrier_unregistered_eperm", unreg < 0 && unreg_errno == EPERM, "rv=%ld,errno=%d", unreg, unreg_errno);

	errno = 0;
	long qflags = membarrier(MEMBARRIER_CMD_QUERY, 1);
	int qflags_errno = errno;
	errno = 0;
	long rflags = membarrier(MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED, 1);
	int rflags_errno = errno;
	errno = 0;
	long bad = membarrier(1 << 20, 0);
	int bad_errno = errno;
	record("membarrier_einval", qflags < 0 && qflags_errno == EINVAL && rflags < 0 && rflags_errno == EINVAL && bad < 0 &&
		bad_errno == EINVAL, "query_flags=%d,register_flags=%d,bad_cmd=%d", qflags_errno, rflags_errno, bad_errno);

	errno = 0;
	long still_unreg = membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0);
	int still_errno = errno;
	long reg = membarrier(MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED, 0);
	pthread_t t[3];
	for (int i = 0; i < 3; i++)
		pthread_create(&t[i], 0, spinner, 0);
	long fails = 0;
	for (int i = 0; i < 200; i++)
		if (membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0))
			fails++;
	__atomic_store_n(&spin_stop, 1, __ATOMIC_RELAXED);
	for (int i = 0; i < 3; i++)
		pthread_join(t[i], 0);
	pid_t pid = fork();
	if (pid == 0)
		_exit(membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED, 0) == 0 ? 0 : 1);
	int status = reap(pid);
	record("membarrier_expedited", still_unreg < 0 && still_errno == EPERM && reg == 0 && fails == 0 && status == 0,
		"flags_left_unregistered=%d,register=%ld,fails=%ld,fork_child=0x%x", still_errno == EPERM, reg, fails, status);
}

int main(void)
{
	signal(SIGPIPE, on_sigpipe);
	check_epoll_xproc();
	check_rdhup_after_peer_shut_wr();
	check_hup_on_peer_exit();
	check_shut_rd();
	check_shut_wr_sigpipe();
	check_shutdown_einval();
	check_blocking_reader_xproc();
	check_pipe_sigpipe();
	check_msg_dontwait();
	check_membarrier();
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
