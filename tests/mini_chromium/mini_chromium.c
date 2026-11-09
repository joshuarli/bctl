#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <sched.h>
#include <setjmp.h>
#include <signal.h>
#include <spawn.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/un.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

extern char **environ;

struct sock_filter {
	uint16_t code;
	uint8_t jt;
	uint8_t jf;
	uint32_t k;
};

struct sock_fprog {
	unsigned short len;
	struct sock_filter *filter;
};

struct seccomp_data {
	int nr;
	uint32_t arch;
	uint64_t instruction_pointer;
	uint64_t args[6];
};

enum {
	BPF_LD_W_ABS = 0x20,
	BPF_JMP_JEQ_K = 0x15,
	BPF_JMP_JSET_K = 0x45,
	BPF_JMP_JA = 0x05,
	BPF_RET_K = 0x06,
};

#define SECCOMP_SET_MODE_FILTER 1
#define SECCOMP_RET_KILL_PROCESS 0x80000000U
#define SECCOMP_RET_TRAP 0x00030000U
#define SECCOMP_RET_ERRNO 0x00050000U
#define SECCOMP_RET_ALLOW 0x7fff0000U
#define AUDIT_ARCH_X86_64 0xc000003eU
#define X32_SYSCALL_BIT 0x40000000U
#define SYS_SECCOMP_SI_CODE 1
#define TRAPPED_SYSCALL_RESULT 4242

#define STMT(c, v) { (c), 0, 0, (v) }
#define JUMP(c, v, t, f) { (c), (t), (f), (v) }

static char self_path[PATH_MAX];
static int report_fd = 1;
static int passed;
static int failed;

static void emit(const char *line, size_t len)
{
	while (len > 0) {
		ssize_t n = write(report_fd, line, len);
		if (n < 0 && errno == EINTR)
			continue;
		if (n <= 0)
			_exit(111);
		line += n;
		len -= (size_t)n;
	}
}

static void record(const char *name, int ok, const char *fmt, ...)
{
	char detail[256] = "";
	char line[512];
	if (fmt) {
		va_list ap;
		va_start(ap, fmt);
		vsnprintf(detail, sizeof detail, fmt, ap);
		va_end(ap);
	}
	for (char *c = detail; *c; c++)
		if (*c == ' ' || *c == '\n')
			*c = ',';
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s%s%s\n", name, ok ? "PASS" : "FAIL",
		detail[0] ? " detail=" : "", detail);
	emit(line, (size_t)len);
	if (ok)
		passed++;
	else
		failed++;
}

static const char *arg_value(int argc, char **argv, const char *key)
{
	size_t n = strlen(key);
	for (int i = 1; i < argc; i++)
		if (!strncmp(argv[i], key, n) && argv[i][n] == '=')
			return argv[i] + n + 1;
	return 0;
}

static long arg_long(int argc, char **argv, const char *key)
{
	const char *v = arg_value(argc, argv, key);
	return v ? strtol(v, 0, 0) : -1;
}

static void fork_failed(const char *func)
{
	record(func + strlen("check_"), 0, "fork errno=%d", errno);
}

static int wait_exit(pid_t pid, int *status)
{
	pid_t r;
	if (pid <= 0)
		return 0;
	do
		r = waitpid(pid, status, 0);
	while (r < 0 && errno == EINTR);
	return r == pid;
}

static int exited_with(int status, int code)
{
	return WIFEXITED(status) && WEXITSTATUS(status) == code;
}

static int killed_by(int status, int sig)
{
	return WIFSIGNALED(status) && WTERMSIG(status) == sig;
}

static pid_t spawn_role(const char *role, char *const extra[], int nextra)
{
	char *argv[16];
	char role_arg[64];
	int n = 0;
	snprintf(role_arg, sizeof role_arg, "--role=%s", role);
	argv[n++] = self_path;
	argv[n++] = role_arg;
	for (int i = 0; i < nextra && n < 15; i++)
		argv[n++] = extra[i];
	argv[n] = 0;
	pid_t pid = fork();
	if (pid == 0) {
		execve(self_path, argv, environ);
		_exit(100 + (errno & 0x3f));
	}
	return pid;
}

static int status_field(const char *field, char *out, size_t outlen)
{
	char text[4096];
	int fd = open("/proc/self/status", O_RDONLY | O_CLOEXEC);
	if (fd < 0)
		return -errno;
	ssize_t n = read(fd, text, sizeof text - 1);
	close(fd);
	if (n < 0)
		return -EIO;
	text[n] = 0;
	size_t flen = strlen(field);
	for (char *line = text; line && *line; line = strchr(line, '\n') ? strchr(line, '\n') + 1 : 0) {
		if (!strncmp(line, field, flen) && line[flen] == ':') {
			char *v = line + flen + 1;
			while (*v == ' ' || *v == '\t')
				v++;
			size_t len = strcspn(v, "\n");
			if (len >= outlen)
				len = outlen - 1;
			memcpy(out, v, len);
			out[len] = 0;
			return 0;
		}
	}
	return -ENOENT;
}

static void check_process_fork(void)
{
	int fds[2];
	pid_t parent = getpid();
	if (pipe(fds)) {
		record("process_fork", 0, "pipe errno=%d", errno);
		return;
	}
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		pid_t me = getpid();
		write(fds[1], &me, sizeof me);
		_exit(getppid() == parent ? 42 : 43);
	}
	close(fds[1]);
	pid_t reported = -1;
	ssize_t n = read(fds[0], &reported, sizeof reported);
	close(fds[0]);
	int status = 0;
	int waited = pid > 0 && wait_exit(pid, &status);
	record("process_fork", waited && exited_with(status, 42), "pid=%d status=0x%x", pid, status);
	record("pid_identity", n == sizeof reported && reported == pid && pid != parent, "fork=%d child_getpid=%d parent=%d",
		pid, reported, parent);
}

static void check_process_exec(void)
{
	char *code[] = { "--code=7" };
	int status = 0;
	pid_t pid = spawn_role("exit-code", code, 1);
	int ok = pid > 0 && wait_exit(pid, &status) && exited_with(status, 7);
	pid_t bad = fork();
	if (bad == 0) {
		char *argv[] = { "/mini-chromium-does-not-exist", 0 };
		execve(argv[0], argv, environ);
		_exit(errno == ENOENT ? 0 : 1);
	}
	int bad_status = 0;
	int enoent = bad > 0 && wait_exit(bad, &bad_status) && exited_with(bad_status, 0);
	record("process_exec", ok && enoent, "exit_status=0x%x enoent=%d", status, enoent);
}

static void check_process_wait(void)
{
	int gate[2];
	if (pipe(gate)) {
		record("process_wait", 0, "pipe errno=%d", errno);
		return;
	}
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		char c;
		close(gate[1]);
		read(gate[0], &c, 1);
		_exit(5);
	}
	close(gate[0]);
	int status = 0;
	pid_t early = waitpid(pid, &status, WNOHANG);
	write(gate[1], "x", 1);
	close(gate[1]);
	struct rusage ru;
	pid_t r;
	do
		r = wait4(pid, &status, 0, &ru);
	while (r < 0 && errno == EINTR);
	errno = 0;
	pid_t again = waitpid(pid, &status, WNOHANG);
	int echild = again < 0 && errno == ECHILD;
	record("process_wait", early == 0 && r == pid && exited_with(status, 5) && echild,
		"wnohang=%d wait4=%d status=0x%x echild=%d", early, r, status, echild);
}

static void check_posix_spawn(void)
{
	char *argv[] = { self_path, "--role=exit-code", "--code=9", 0 };
	pid_t pid = -1;
	int rc = posix_spawn(&pid, self_path, 0, 0, argv, environ);
	int status = 0;
	int ok = rc == 0 && wait_exit(pid, &status) && exited_with(status, 9);
	record("process_spawn", ok, "rc=%d status=0x%x", rc, status);
}

static void check_vm_isolation(void)
{
	enum { rounds = 1000 };
	volatile uint64_t *page = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	int to_child[2], to_parent[2];
	if (page == MAP_FAILED || pipe(to_child) || pipe(to_parent)) {
		record("vm_isolation", 0, "setup errno=%d", errno);
		return;
	}
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		int bad = 0;
		char c;
		for (int i = 0; i < rounds; i++) {
			if (read(to_child[0], &c, 1) != 1)
				_exit(2);
			*page = 0x5555000000000000ull + (uint64_t)i;
			sched_yield();
			if (*page != 0x5555000000000000ull + (uint64_t)i)
				bad++;
			write(to_parent[1], "c", 1);
		}
		_exit(bad ? 1 : 0);
	}
	int bad = 0;
	char c;
	for (int i = 0; i < rounds; i++) {
		*page = 0xaaaa000000000000ull + (uint64_t)i;
		write(to_child[1], "p", 1);
		if (read(to_parent[0], &c, 1) != 1) {
			bad++;
			break;
		}
		if (*page != 0xaaaa000000000000ull + (uint64_t)i)
			bad++;
	}
	int status = 0;
	wait_exit(pid, &status);
	close(to_child[0]);
	close(to_child[1]);
	close(to_parent[0]);
	close(to_parent[1]);
	munmap((void *)page, 4096);
	record("vm_isolation", !bad && exited_with(status, 0), "rounds=%d parent_bad=%d child_status=0x%x", rounds, bad, status);
}

static void check_cow(void)
{
	enum { pages = 4 };
	size_t len = pages * 4096;
	unsigned char *buf = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (buf == MAP_FAILED) {
		record("cow", 0, "mmap errno=%d", errno);
		return;
	}
	for (size_t i = 0; i < len; i++)
		buf[i] = (unsigned char)(i * 7);
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		for (size_t i = 0; i < len; i++)
			if (buf[i] != (unsigned char)(i * 7))
				_exit(1);
		memset(buf, 0xee, len);
		for (size_t i = 0; i < len; i++)
			if (buf[i] != 0xee)
				_exit(2);
		_exit(0);
	}
	int status = 0;
	wait_exit(pid, &status);
	int intact = 1;
	for (size_t i = 0; i < len; i++)
		if (buf[i] != (unsigned char)(i * 7))
			intact = 0;
	munmap(buf, len);
	record("cow", exited_with(status, 0) && intact, "child_status=0x%x parent_intact=%d", status, intact);
}

static void check_shared_anon(void)
{
	volatile uint32_t *word = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	if (word == MAP_FAILED) {
		record("shared_anon", 0, "mmap errno=%d", errno);
		return;
	}
	*word = 1;
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		*word = 0xc0ffee;
		_exit(0);
	}
	int status = 0;
	wait_exit(pid, &status);
	record("shared_anon", exited_with(status, 0) && *word == 0xc0ffee, "value=0x%x", *word);
	munmap((void *)word, 4096);
}

static void check_mprotect_denied(void)
{
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		volatile char *p = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (p == MAP_FAILED)
			_exit(2);
		p[0] = 1;
		if (mprotect((void *)p, 4096, PROT_READ))
			_exit(3);
		if (p[0] != 1)
			_exit(4);
		p[0] = 2;
		_exit(0);
	}
	int status = 0;
	wait_exit(pid, &status);
	record("mprotect_denied", killed_by(status, SIGSEGV), "status=0x%x", status);
}

static volatile sig_atomic_t sigchld_seen;

static void on_sigchld(int sig)
{
	(void)sig;
	sigchld_seen = 1;
}

static void check_signals(void)
{
	struct sigaction sa = { .sa_handler = on_sigchld };
	sigemptyset(&sa.sa_mask);
	sigaction(SIGCHLD, &sa, 0);
	sigchld_seen = 0;
	int ready[2];
	if (pipe(ready)) {
		record("signals", 0, "pipe errno=%d", errno);
		return;
	}
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		close(ready[0]);
		write(ready[1], "r", 1);
		for (;;)
			pause();
	}
	close(ready[1]);
	char c;
	read(ready[0], &c, 1);
	close(ready[0]);
	int probe = kill(pid, 0);
	int term = kill(pid, SIGTERM);
	int status = 0;
	wait_exit(pid, &status);
	errno = 0;
	int gone = kill(pid, 0) < 0 && errno == ESRCH;
	signal(SIGCHLD, SIG_DFL);
	record("signals", probe == 0 && term == 0 && killed_by(status, SIGTERM) && sigchld_seen && gone,
		"status=0x%x sigchld=%d esrch_after_reap=%d", status, sigchld_seen, gone);
}

struct counter {
	pthread_mutex_t lock;
	long value;
};

static void *bump(void *arg)
{
	struct counter *c = arg;
	for (int i = 0; i < 10000; i++) {
		pthread_mutex_lock(&c->lock);
		c->value++;
		pthread_mutex_unlock(&c->lock);
	}
	return 0;
}

static void check_threads_in_child(void)
{
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		struct counter c = { PTHREAD_MUTEX_INITIALIZER, 0 };
		pthread_t t[4];
		for (int i = 0; i < 4; i++)
			if (pthread_create(&t[i], 0, bump, &c))
				_exit(2);
		for (int i = 0; i < 4; i++)
			pthread_join(t[i], 0);
		_exit(c.value == 40000 ? 0 : 1);
	}
	int status = 0;
	wait_exit(pid, &status);
	record("threads_in_child", exited_with(status, 0), "status=0x%x", status);
}

static long futex(volatile uint32_t *addr, int op, uint32_t val, const struct timespec *ts)
{
	return syscall(SYS_futex, addr, op, val, ts, 0, 0);
}

static void check_futex_xproc(void)
{
	volatile uint32_t *word = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	int ready[2];
	if (word == MAP_FAILED || pipe(ready)) {
		record("futex_xproc", 0, "setup errno=%d", errno);
		return;
	}
	*word = 0;
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		struct timespec ts = { 5, 0 };
		write(ready[1], "r", 1);
		while (__atomic_load_n(word, __ATOMIC_SEQ_CST) == 0) {
			long r = futex(word, 0, 0, &ts);
			if (r < 0 && errno == ETIMEDOUT)
				_exit(3);
		}
		_exit(0);
	}
	char c;
	read(ready[0], &c, 1);
	struct timespec pause_ts = { 0, 50 * 1000 * 1000 };
	nanosleep(&pause_ts, 0);
	__atomic_store_n(word, 1, __ATOMIC_SEQ_CST);
	long woken = futex(word, 1, 1, 0);
	int status = 0;
	wait_exit(pid, &status);
	close(ready[0]);
	close(ready[1]);
	munmap((void *)word, 4096);
	record("futex_xproc", exited_with(status, 0) && woken == 1, "woken=%ld status=0x%x", woken, status);
}

static void check_epoll_eventfd(void)
{
	int efd = eventfd(0, EFD_CLOEXEC);
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event ev = { .events = EPOLLIN, .data.fd = efd };
	if (efd < 0 || ep < 0 || epoll_ctl(ep, EPOLL_CTL_ADD, efd, &ev)) {
		record("epoll_eventfd", 0, "setup errno=%d", errno);
		return;
	}
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		struct timespec ts = { 0, 20 * 1000 * 1000 };
		nanosleep(&ts, 0);
		uint64_t v = 5;
		_exit(write(efd, &v, sizeof v) == sizeof v ? 0 : 1);
	}
	struct epoll_event out;
	int n = epoll_wait(ep, &out, 1, 5000);
	uint64_t v = 0;
	ssize_t r = n == 1 ? read(efd, &v, sizeof v) : -1;
	int status = 0;
	wait_exit(pid, &status);
	close(ep);
	close(efd);
	record("epoll_eventfd", n == 1 && (out.events & EPOLLIN) && r == sizeof v && v == 5 && exited_with(status, 0),
		"ready=%d value=%llu", n, (unsigned long long)v);
}

static void check_process_limit(void)
{
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		struct rlimit rl = { 1, 1 };
		if (setrlimit(RLIMIT_NPROC, &rl))
			_exit(2);
		pid_t c = fork();
		if (c == 0)
			_exit(0);
		if (c > 0) {
			waitpid(c, 0, 0);
			_exit(getuid() == 0 ? 0 : 1);
		}
		_exit(errno == EAGAIN ? 0 : 3);
	}
	int status = 0;
	wait_exit(pid, &status);
	record("process_limit", exited_with(status, 0), "status=0x%x", status);
}

static void check_lifecycle_stress(void)
{
	enum { serial = 50, concurrent = 16 };
	int bad = 0;
	for (int i = 0; i < serial; i++) {
		char code[32];
		snprintf(code, sizeof code, "--code=%d", i % 50);
		char *extra[] = { code };
		int status = 0;
		pid_t pid = spawn_role("exit-code", extra, 1);
		if (pid <= 0 || !wait_exit(pid, &status) || !exited_with(status, i % 50))
			bad++;
	}
	pid_t pids[concurrent];
	for (int i = 0; i < concurrent; i++) {
		char code[32];
		snprintf(code, sizeof code, "--code=%d", i + 1);
		char *extra[] = { code };
		pids[i] = spawn_role("exit-code", extra, 1);
	}
	int seen = 0;
	for (;;) {
		int status;
		errno = 0;
		pid_t r = wait(&status);
		if (r < 0 && errno == EINTR)
			continue;
		if (r <= 0)
			break;
		for (int i = 0; i < concurrent; i++)
			if (pids[i] == r) {
				if (!exited_with(status, i + 1))
					bad++;
				seen++;
			}
	}
	int echild = errno == ECHILD;
	record("lifecycle_stress", !bad && seen == concurrent && echild, "bad=%d reaped=%d echild=%d", bad, seen, echild);
}

static int send_fds(int sock, const int *fds, int nfds, const void *data, size_t len)
{
	char control[CMSG_SPACE(sizeof(int) * 8)];
	memset(control, 0, sizeof control);
	struct iovec iov = { (void *)data, len };
	struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control,
		.msg_controllen = CMSG_SPACE(sizeof(int) * (size_t)nfds) };
	struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
	cm->cmsg_level = SOL_SOCKET;
	cm->cmsg_type = SCM_RIGHTS;
	cm->cmsg_len = CMSG_LEN(sizeof(int) * (size_t)nfds);
	memcpy(CMSG_DATA(cm), fds, sizeof(int) * (size_t)nfds);
	return sendmsg(sock, &msg, MSG_NOSIGNAL) == (ssize_t)len ? 0 : -errno;
}

static int recv_fds(int sock, int *fds, int max, void *data, size_t len, int *nfds)
{
	char control[CMSG_SPACE(sizeof(int) * 8)];
	struct iovec iov = { data, len };
	struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control };
	ssize_t n = recvmsg(sock, &msg, MSG_CMSG_CLOEXEC);
	*nfds = 0;
	if (n < 0)
		return -errno;
	if (msg.msg_flags & MSG_CTRUNC)
		return -EMSGSIZE;
	for (struct cmsghdr *cm = CMSG_FIRSTHDR(&msg); cm; cm = CMSG_NXTHDR(&msg, cm))
		if (cm->cmsg_level == SOL_SOCKET && cm->cmsg_type == SCM_RIGHTS) {
			int count = (int)((cm->cmsg_len - CMSG_LEN(0)) / sizeof(int));
			for (int i = 0; i < count && *nfds < max; i++)
				memcpy(&fds[(*nfds)++], CMSG_DATA(cm) + sizeof(int) * (size_t)i, sizeof(int));
		}
	return (int)n;
}

static void check_scm_negative(void)
{
	int sv[2];
	if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv)) {
		record("scm_rights_negative", 0, "socketpair errno=%d", errno);
		return;
	}
	int bogus = 987654;
	int ebadf = send_fds(sv[0], &bogus, 1, "x", 1) == -EBADF;
	char control[CMSG_SPACE(sizeof(int))];
	memset(control, 0, sizeof control);
	struct iovec iov = { "x", 1 };
	struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control };
	struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
	cm->cmsg_level = SOL_SOCKET;
	cm->cmsg_type = SCM_RIGHTS;
	cm->cmsg_len = sizeof control + 64;
	errno = 0;
	int einval = sendmsg(sv[0], &msg, MSG_NOSIGNAL) < 0 && errno == EINVAL;
	close(sv[1]);
	errno = 0;
	int epipe = send(sv[0], "x", 1, MSG_NOSIGNAL) < 0 && errno == EPIPE;
	close(sv[0]);
	record("scm_rights_negative", ebadf && einval && epipe, "ebadf=%d einval=%d epipe=%d", ebadf, einval, epipe);
}

static void check_seccomp_requires_nnp(void)
{
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		if (geteuid() == 0 && setuid(1000))
			_exit(2);
		if (geteuid() == 0)
			_exit(3);
		if (prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 0)
			_exit(4);
		struct sock_filter allow[] = { STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
		struct sock_fprog prog = { 1, allow };
		long r = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog);
		_exit(r < 0 && errno == EACCES ? 0 : 5);
	}
	int status = 0;
	wait_exit(pid, &status);
	record("seccomp_requires_nnp", exited_with(status, 0), "status=0x%x", status);
}

static void check_credentials(void)
{
	pid_t pid = fork();
	if (pid < 0) {
		fork_failed(__func__);
		return;
	}
	if (pid == 0) {
		if (geteuid() == 0 && (setgid(1000) || setuid(1000)))
			_exit(2);
		if (getuid() == 0 || geteuid() == 0)
			_exit(3);
		uid_t uid = getuid();
		errno = 0;
		if (setuid(0) == 0 || errno != EPERM)
			_exit(4);
		if (getuid() != uid)
			_exit(5);
		char value[128];
		if (status_field("Uid", value, sizeof value))
			_exit(6);
		char want[32];
		snprintf(want, sizeof want, "%u", (unsigned)uid);
		_exit(strncmp(value, want, strlen(want)) ? 7 : 0);
	}
	int status = 0;
	wait_exit(pid, &status);
	record("credentials", exited_with(status, 0), "status=0x%x", status);
}

static volatile sig_atomic_t trapped_syscall = -1;
static volatile sig_atomic_t trapped_arch_ok;

static void on_sigsys(int sig, siginfo_t *info, void *ctx)
{
	(void)sig;
	ucontext_t *uc = ctx;
	trapped_syscall = info->si_syscall;
	trapped_arch_ok = info->si_code == SYS_SECCOMP_SI_CODE && info->si_arch == AUDIT_ARCH_X86_64;
	uc->uc_mcontext.gregs[REG_RAX] = TRAPPED_SYSCALL_RESULT;
}

static int install_renderer_policy(void)
{
	struct sock_filter filter[] = {
		STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, arch)),
		JUMP(BPF_JMP_JEQ_K, AUDIT_ARCH_X86_64, 1, 0),
		STMT(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
		STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)),
		JUMP(BPF_JMP_JSET_K, X32_SYSCALL_BIT, 0, 1),
		STMT(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
		JUMP(BPF_JMP_JEQ_K, SYS_socket, 0, 1),
		STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EACCES),
		JUMP(BPF_JMP_JEQ_K, SYS_ptrace, 0, 1),
		STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EPERM),
		JUMP(BPF_JMP_JEQ_K, SYS_process_vm_readv, 0, 1),
		STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EPERM),
		JUMP(BPF_JMP_JEQ_K, SYS_process_vm_writev, 0, 1),
		STMT(BPF_RET_K, SECCOMP_RET_ERRNO | EPERM),
		JUMP(BPF_JMP_JEQ_K, SYS_uname, 0, 1),
		STMT(BPF_RET_K, SECCOMP_RET_TRAP),
		STMT(BPF_RET_K, SECCOMP_RET_ALLOW),
	};
	struct sock_fprog prog = { sizeof filter / sizeof filter[0], filter };
	if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
		return -errno;
	return syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog) ? -errno : 0;
}

static int malformed_filters_rejected(void)
{
	struct sock_filter out_of_range[] = { JUMP(BPF_JMP_JA, 50, 0, 0), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
	struct sock_filter no_return[] = { STMT(BPF_LD_W_ABS, offsetof(struct seccomp_data, nr)) };
	struct sock_filter misaligned[] = { STMT(BPF_LD_W_ABS, 3), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
	struct sock_filter beyond_data[] = { STMT(BPF_LD_W_ABS, sizeof(struct seccomp_data)), STMT(BPF_RET_K, SECCOMP_RET_ALLOW) };
	struct sock_fprog progs[] = {
		{ 2, out_of_range }, { 1, no_return }, { 2, misaligned }, { 2, beyond_data }, { 0, out_of_range },
	};
	int rejected = 0;
	for (size_t i = 0; i < sizeof progs / sizeof progs[0]; i++) {
		errno = 0;
		if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &progs[i]) < 0 && errno == EINVAL)
			rejected++;
	}
	return rejected == (int)(sizeof progs / sizeof progs[0]);
}

struct renderer_report {
	int scm_rights;
	int payload_ok;
	int seal_enforced;
	int private_unmapped;
	int seccomp_installed;
	int socket_denied;
	int ptrace_denied;
	int vm_read_denied;
	int trap_ok;
	int allowed_ok;
	int malformed_rejected;
	int nnp;
	int seccomp_status;
	int inherit_status;
	int fd_inheritance;
};

static sigjmp_buf fault_jump;

static void on_fault(int sig)
{
	(void)sig;
	siglongjmp(fault_jump, 1);
}

static int address_readable(uintptr_t addr, uint64_t *out)
{
	struct sigaction sa = { .sa_handler = on_fault }, old_segv, old_bus;
	sigemptyset(&sa.sa_mask);
	sigaction(SIGSEGV, &sa, &old_segv);
	sigaction(SIGBUS, &sa, &old_bus);
	int readable = 0;
	if (!sigsetjmp(fault_jump, 1)) {
		*out = *(volatile uint64_t *)addr;
		readable = 1;
	}
	sigaction(SIGSEGV, &old_segv, 0);
	sigaction(SIGBUS, &old_bus, 0);
	return readable;
}

static int address_writable(uintptr_t addr)
{
	struct sigaction sa = { .sa_handler = on_fault }, old_segv, old_bus;
	sigemptyset(&sa.sa_mask);
	sigaction(SIGSEGV, &sa, &old_segv);
	sigaction(SIGBUS, &sa, &old_bus);
	int writable = 0;
	if (!sigsetjmp(fault_jump, 1)) {
		*(volatile uint64_t *)addr = 0x4141414141414141ull;
		writable = 1;
	}
	sigaction(SIGSEGV, &old_segv, 0);
	sigaction(SIGBUS, &old_bus, 0);
	return writable;
}

static void check_kernel_address(void)
{
	static const uintptr_t kernel_addrs[] = { 0xffffffff80000000ull, 0xffffffff80200000ull, 0xffff800000000000ull };
	int ok = 1;
	char detail[128] = "";
	for (size_t i = 0; i < sizeof kernel_addrs / sizeof kernel_addrs[0]; i++) {
		uint64_t v;
		int readable = address_readable(kernel_addrs[i], &v);
		int writable = address_writable(kernel_addrs[i]);
		if (readable || writable) {
			ok = 0;
			snprintf(detail, sizeof detail, "addr=0x%lx readable=%d writable=%d", (unsigned long)kernel_addrs[i], readable,
				writable);
		}
	}
	record("kernel_address_fault", ok, "%s", detail);
}

static int role_renderer(int argc, char **argv)
{
	int ipc = (int)arg_long(argc, argv, "--ipc");
	int cloexec_fd = (int)arg_long(argc, argv, "--cloexec-fd");
	int inherit_fd = (int)arg_long(argc, argv, "--inherit-fd");
	pid_t browser = (pid_t)arg_long(argc, argv, "--browser-pid");
	uintptr_t secret_addr = (uintptr_t)strtoull(arg_value(argc, argv, "--secret-addr") ? arg_value(argc, argv, "--secret-addr") : "0", 0, 0);
	uint64_t secret = strtoull(arg_value(argc, argv, "--secret") ? arg_value(argc, argv, "--secret") : "0", 0, 0);
	struct renderer_report rep = { 0 };
	rep.fd_inheritance = fcntl(cloexec_fd, F_GETFD) < 0 && errno == EBADF && fcntl(inherit_fd, F_GETFD) >= 0;

	int fds[2] = { -1, -1 }, nfds = 0;
	char work[16] = "";
	int n = recv_fds(ipc, fds, 2, work, sizeof work - 1, &nfds);
	rep.scm_rights = n > 0 && nfds == 2 && !strncmp(work, "work", 4);
	int shm = fds[0], notify = fds[1];
	struct stat st;
	unsigned char *shared = MAP_FAILED;
	if (rep.scm_rights && !fstat(shm, &st) && st.st_size == 8192)
		shared = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, shm, 0);
	if (shared != MAP_FAILED) {
		rep.payload_ok = !memcmp(shared, "browser-payload", 16);
		memcpy(shared + 4096, "renderer-result", 16);
		errno = 0;
		rep.seal_enforced = ftruncate(shm, 4096) < 0 && errno == EPERM;
	}

	uint64_t seen = 0;
	rep.private_unmapped = !address_readable(secret_addr, &seen) || seen != secret;

	struct sigaction sa = { .sa_sigaction = on_sigsys, .sa_flags = SA_SIGINFO };
	sigemptyset(&sa.sa_mask);
	sigaction(SIGSYS, &sa, 0);
	rep.seccomp_installed = install_renderer_policy() == 0;
	errno = 0;
	rep.socket_denied = socket(AF_INET, SOCK_STREAM, 0) < 0 && errno == EACCES;
	errno = 0;
	rep.ptrace_denied = ptrace(PTRACE_ATTACH, browser, 0, 0) < 0 && errno == EPERM;
	uint64_t stolen = 0;
	struct iovec local = { &stolen, sizeof stolen }, remote = { (void *)secret_addr, sizeof stolen };
	errno = 0;
	rep.vm_read_denied = process_vm_readv(browser, &local, 1, &remote, 1, 0) < 0 && errno == EPERM && stolen != secret;
	struct utsname u;
	long trapped = syscall(SYS_uname, &u);
	rep.trap_ok = trapped == TRAPPED_SYSCALL_RESULT && trapped_syscall == SYS_uname && trapped_arch_ok;
	int dupfd = dup(ipc);
	rep.allowed_ok = getpid() > 0 && dupfd >= 0 && close(dupfd) == 0;
	rep.malformed_rejected = malformed_filters_rejected();
	rep.nnp = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1;
	char value[64];
	rep.seccomp_status = !status_field("Seccomp", value, sizeof value) && !strcmp(value, "2") &&
		!status_field("NoNewPrivs", value, sizeof value) && !strcmp(value, "1");

	char *probe[] = { self_path, "--role=sandbox-probe", 0 };
	pid_t pid = fork();
	if (pid == 0) {
		execve(self_path, probe, environ);
		_exit(100);
	}
	int status = 0;
	rep.inherit_status = pid > 0 && wait_exit(pid, &status) && exited_with(status, 0);

	uint64_t one = 1;
	write(notify, &one, sizeof one);
	return write(ipc, &rep, sizeof rep) == sizeof rep ? 0 : 1;
}

static int role_sandbox_probe(void)
{
	char value[64];
	errno = 0;
	int socket_denied = socket(AF_INET, SOCK_STREAM, 0) < 0 && errno == EACCES;
	int nnp = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1;
	int status = !status_field("Seccomp", value, sizeof value) && !strcmp(value, "2");
	return socket_denied && nnp && status ? 0 : 1;
}

static void check_browser_renderer(void)
{
	int shm = memfd_create("mini-chromium-shm", MFD_CLOEXEC | MFD_ALLOW_SEALING);
	int sv[2];
	int notify = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
	int inherit = open("/", O_RDONLY | O_DIRECTORY);
	int cloexec = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
	if (shm < 0 || notify < 0 || inherit < 0 || cloexec < 0 || ftruncate(shm, 8192) ||
		socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv)) {
		record("scm_rights", 0, "setup errno=%d", errno);
		return;
	}
	unsigned char *shared = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, shm, 0);
	if (shared == MAP_FAILED || fcntl(shm, F_ADD_SEALS, F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL)) {
		record("scm_rights", 0, "map_or_seal errno=%d", errno);
		return;
	}
	memcpy(shared, "browser-payload", 16);
	volatile uint64_t *secret = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	*secret = 0x5ec2e75ec2e75ec2ull;
	int one = 1;
	setsockopt(sv[0], SOL_SOCKET, SO_PASSCRED, &one, sizeof one);
	int child_end = fcntl(sv[1], F_DUPFD, 10);
	char a_ipc[32], a_cloexec[32], a_inherit[32], a_browser[32], a_addr[48], a_secret[48];
	snprintf(a_ipc, sizeof a_ipc, "--ipc=%d", child_end);
	snprintf(a_cloexec, sizeof a_cloexec, "--cloexec-fd=%d", cloexec);
	snprintf(a_inherit, sizeof a_inherit, "--inherit-fd=%d", inherit);
	snprintf(a_browser, sizeof a_browser, "--browser-pid=%d", getpid());
	snprintf(a_addr, sizeof a_addr, "--secret-addr=0x%lx", (unsigned long)(uintptr_t)secret);
	snprintf(a_secret, sizeof a_secret, "--secret=0x%llx", (unsigned long long)*secret);
	char *extra[] = { a_ipc, a_cloexec, a_inherit, a_browser, a_addr, a_secret };
	pid_t pid = spawn_role("renderer", extra, 6);
	close(child_end);
	close(sv[1]);
	int pass[2] = { shm, notify };
	int sent = send_fds(sv[0], pass, 2, "work", 4) == 0;

	struct ucred peer;
	socklen_t plen = sizeof peer;
	int peer_ok = !getsockopt(sv[0], SOL_SOCKET, SO_PEERCRED, &peer, &plen) && peer.pid == getpid() && peer.uid == getuid();

	struct renderer_report rep;
	memset(&rep, 0, sizeof rep);
	char control[CMSG_SPACE(sizeof(struct ucred))];
	struct iovec iov = { &rep, sizeof rep };
	struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control };
	size_t got = 0;
	int cred_ok = 0;
	while (got < sizeof rep) {
		iov.iov_base = (char *)&rep + got;
		iov.iov_len = sizeof rep - got;
		msg.msg_controllen = sizeof control;
		ssize_t n = recvmsg(sv[0], &msg, 0);
		if (n <= 0)
			break;
		for (struct cmsghdr *cm = CMSG_FIRSTHDR(&msg); cm; cm = CMSG_NXTHDR(&msg, cm))
			if (cm->cmsg_level == SOL_SOCKET && cm->cmsg_type == SCM_CREDENTIALS) {
				struct ucred c;
				memcpy(&c, CMSG_DATA(cm), sizeof c);
				cred_ok = c.pid == pid && c.uid == getuid();
			}
		got += (size_t)n;
	}
	uint64_t note = 0;
	int notified = read(notify, &note, sizeof note) == sizeof note && note == 1;
	int status = 0;
	wait_exit(pid, &status);
	int clean = exited_with(status, 0) && got == sizeof rep;

	record("fd_inheritance", clean && rep.fd_inheritance, 0);
	record("scm_rights", clean && sent && rep.scm_rights, "sent=%d", sent);
	record("peer_credentials", peer_ok && cred_ok, "so_peercred=%d scm_credentials=%d", peer_ok, cred_ok);
	record("shared_memory_fd", clean && rep.payload_ok && !memcmp(shared + 4096, "renderer-result", 16) && notified,
		"payload=%d notified=%d", rep.payload_ok, notified);
	record("seal_enforcement", clean && rep.seal_enforced, 0);
	record("private_stays_private", clean && rep.private_unmapped && *secret == 0x5ec2e75ec2e75ec2ull, 0);
	record("seccomp_enforcement", clean && rep.seccomp_installed && rep.socket_denied && rep.allowed_ok,
		"installed=%d socket_denied=%d allowed=%d", rep.seccomp_installed, rep.socket_denied, rep.allowed_ok);
	record("seccomp_trap", clean && rep.trap_ok, 0);
	record("ptrace_denied", clean && rep.ptrace_denied && rep.vm_read_denied, "ptrace=%d process_vm_readv=%d",
		rep.ptrace_denied, rep.vm_read_denied);
	record("bpf_rejects_malformed", clean && rep.malformed_rejected, 0);
	record("nnp_state", clean && rep.nnp, 0);
	record("proc_status", clean && rep.seccomp_status, 0);
	record("seccomp_inherit", clean && rep.inherit_status, "renderer_status=0x%x", status);
	munmap(shared, 8192);
	munmap((void *)secret, 4096);
	close(sv[0]);
	close(shm);
	close(notify);
	close(inherit);
	close(cloexec);
}

static int write_file(const char *path, const char *text)
{
	int fd = open(path, O_WRONLY | O_CLOEXEC);
	if (fd < 0)
		return -errno;
	ssize_t n = write(fd, text, strlen(text));
	int err = n == (ssize_t)strlen(text) ? 0 : -errno;
	close(fd);
	return err;
}

struct zygote_report {
	int userns;
	int uid_map_once;
	int caps_scoped;
	int pid_one;
	int kill_outside_denied;
	int netns_denied;
	int fs_restricted;
};

static int role_zygote(int argc, char **argv)
{
	int report = (int)arg_long(argc, argv, "--report");
	pid_t browser = (pid_t)arg_long(argc, argv, "--browser-pid");
	struct zygote_report rep = { 0 };
	uid_t uid = getuid();
	gid_t gid = getgid();
	char map[64];
	if (!unshare(CLONE_NEWUSER)) {
		snprintf(map, sizeof map, "0 %u 1\n", (unsigned)uid);
		int u = write_file("/proc/self/uid_map", map);
		int s = write_file("/proc/self/setgroups", "deny\n");
		snprintf(map, sizeof map, "0 %u 1\n", (unsigned)gid);
		int g = write_file("/proc/self/gid_map", map);
		rep.userns = !u && !s && !g && getuid() == 0 && getgid() == 0;
		snprintf(map, sizeof map, "0 %u 1\n", (unsigned)uid);
		rep.uid_map_once = write_file("/proc/self/uid_map", map) == -EPERM;
		errno = 0;
		rep.caps_scoped = sethostname("mini-chromium", 13) < 0 && errno == EPERM;
	}
	if (unshare(CLONE_NEWPID | CLONE_NEWNET)) {
		write(report, &rep, sizeof rep);
		return 1;
	}
	pid_t init = fork();
	if (init == 0) {
		rep.pid_one = getpid() == 1;
		errno = 0;
		rep.kill_outside_denied = kill(browser, 0) < 0 && errno == ESRCH;
		int s = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
		struct sockaddr_in dst = { .sin_family = AF_INET, .sin_port = htons(80) };
		inet_pton(AF_INET, "1.1.1.1", &dst.sin_addr);
		errno = 0;
		rep.netns_denied = s >= 0 && connect(s, (struct sockaddr *)&dst, sizeof dst) < 0 && errno == ENETUNREACH;
		if (s >= 0)
			close(s);
		if (!chroot("/proc/self/fdinfo") && !chdir("/")) {
			errno = 0;
			int fd = open("/etc/passwd", O_RDONLY | O_CLOEXEC);
			int fd2 = open("/mini_chromium", O_RDONLY | O_CLOEXEC);
			rep.fs_restricted = fd < 0 && fd2 < 0;
		}
		_exit(write(report, &rep, sizeof rep) == sizeof rep ? 0 : 1);
	}
	int status = 0;
	return init > 0 && wait_exit(init, &status) && exited_with(status, 0) ? 0 : 1;
}

static void check_zygote_namespaces(void)
{
	int pipefd[2];
	if (pipe(pipefd)) {
		record("userns_map", 0, "pipe errno=%d", errno);
		return;
	}
	char a_report[32], a_browser[32];
	snprintf(a_report, sizeof a_report, "--report=%d", pipefd[1]);
	snprintf(a_browser, sizeof a_browser, "--browser-pid=%d", getpid());
	char *extra[] = { a_report, a_browser };
	pid_t pid = spawn_role("zygote", extra, 2);
	close(pipefd[1]);
	struct zygote_report rep;
	memset(&rep, 0, sizeof rep);
	ssize_t n = read(pipefd[0], &rep, sizeof rep);
	close(pipefd[0]);
	int status = 0;
	wait_exit(pid, &status);
	int clean = n == sizeof rep && exited_with(status, 0);
	record("userns_map", clean && rep.userns && rep.uid_map_once, "mapped=%d second_write_eperm=%d", rep.userns, rep.uid_map_once);
	record("userns_caps_scoped", clean && rep.caps_scoped, 0);
	record("pidns_isolation", clean && rep.pid_one, 0);
	record("kill_outside_denied", clean && rep.kill_outside_denied, 0);
	record("netns_denied", clean && rep.netns_denied, 0);
	record("fs_restricted", clean && rep.fs_restricted, "zygote_status=0x%x", status);
}

static int role_browser(void)
{
	check_process_fork();
	check_process_exec();
	check_process_wait();
	check_posix_spawn();
	check_vm_isolation();
	check_cow();
	check_shared_anon();
	check_mprotect_denied();
	check_kernel_address();
	check_signals();
	check_threads_in_child();
	check_futex_xproc();
	check_epoll_eventfd();
	check_process_limit();
	check_scm_negative();
	check_credentials();
	check_seccomp_requires_nnp();
	check_browser_renderer();
	check_zygote_namespaces();
	check_lifecycle_stress();
	char line[128];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	emit(line, (size_t)len);
	return failed ? 1 : 0;
}

int main(int argc, char **argv)
{
	ssize_t n = readlink("/proc/self/exe", self_path, sizeof self_path - 1);
	if (n > 0)
		self_path[n] = 0;
	else if (!realpath(argv[0], self_path))
		snprintf(self_path, sizeof self_path, "%s", argv[0]);
	const char *role = arg_value(argc, argv, "--role");
	if (!role || !strcmp(role, "browser"))
		return role_browser();
	if (!strcmp(role, "exit-code"))
		return (int)arg_long(argc, argv, "--code");
	if (!strcmp(role, "renderer"))
		return role_renderer(argc, argv);
	if (!strcmp(role, "sandbox-probe"))
		return role_sandbox_probe();
	if (!strcmp(role, "zygote"))
		return role_zygote(argc, argv);
	return 64;
}
