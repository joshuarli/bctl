#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

enum { CHECK_DEADLINE_SECONDS = 40 };

#ifndef CLONE_NEWPID
#define CLONE_NEWPID 0x20000000
#endif
#ifndef CLONE_NEWUSER
#define CLONE_NEWUSER 0x10000000
#endif
#ifndef CLONE_NEWNS
#define CLONE_NEWNS 0x00020000
#endif

static int passed, failed;
static const char *volatile current_check = "";
static uid_t map_uid;
static gid_t map_gid;
static pid_t outside_pid;

static void record(const char *name, int ok, const char *fmt, ...)
{
	char detail[192] = "", line[320];
	if (fmt) {
		va_list ap;
		va_start(ap, fmt);
		vsnprintf(detail, sizeof detail, fmt, ap);
		va_end(ap);
	}
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s%s%s\n", name,
		ok ? "PASS" : "FAIL", detail[0] ? " detail=" : "", detail);
	write(1, line, (size_t)len);
	ok ? passed++ : failed++;
}

static void deadline(int sig)
{
	(void)sig;
	char line[160];
	int len = snprintf(line, sizeof line,
		"MINI_CHROMIUM_TEST name=%s status=FAIL detail=watchdog\n"
		"MINI_CHROMIUM_RESULT status=FAIL passed=%d failed=%d\n",
		current_check, passed, failed + 1);
	write(1, line, (size_t)len);
	_exit(1);
}

static int wait_status(pid_t pid)
{
	int status;
	pid_t r;
	while ((r = waitpid(pid, &status, 0)) < 0 && errno == EINTR)
		;
	return r == pid ? status : -1;
}

static int child_exit_code(int status)
{
	return WIFEXITED(status) ? WEXITSTATUS(status) : 1000 + status;
}

static ssize_t read_file(const char *path, char *buf, size_t len)
{
	int fd = open(path, O_RDONLY | O_CLOEXEC);
	if (fd < 0)
		return -errno;
	size_t got = 0;
	ssize_t n;
	while (got < len - 1 && (n = read(fd, buf + got, len - 1 - got)) > 0)
		got += (size_t)n;
	close(fd);
	buf[got] = 0;
	return (ssize_t)got;
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

static int write_identity_maps(void)
{
	char map[64];
	int r = write_file("/proc/self/setgroups", "deny\n");
	if (r)
		return r;
	snprintf(map, sizeof map, "%u %u 1\n", (unsigned)map_gid, (unsigned)map_gid);
	r = write_file("/proc/self/gid_map", map);
	if (r)
		return r;
	snprintf(map, sizeof map, "%u %u 1\n", (unsigned)map_uid, (unsigned)map_uid);
	return write_file("/proc/self/uid_map", map);
}

static int status_field(const char *path, const char *field, char *out, size_t outlen)
{
	char text[8192];
	if (read_file(path, text, sizeof text) < 0)
		return -1;
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
	return -1;
}

static int field_equals(const char *path, const char *field, const char *want)
{
	char v[256];
	return !status_field(path, field, v, sizeof v) && !strcmp(v, want);
}

static int split_tabs(char *s, char **fields, int max)
{
	int n = 0;
	char *p = s;
	while (p && *p && n < max) {
		fields[n++] = p;
		p = strchr(p, '\t');
		if (p)
			*p++ = 0;
	}
	return n;
}

static int dir_has(const char *path, const char *name)
{
	DIR *d = opendir(path);
	if (!d)
		return 0;
	struct dirent *e;
	int found = 0;
	while ((e = readdir(d)))
		if (!strcmp(e->d_name, name)) {
			found = 1;
			break;
		}
	closedir(d);
	return found;
}

static volatile sig_atomic_t usr1_pid, usr1_code, chld_pid, chld_code;

static void on_usr1(int sig, siginfo_t *si, void *uc)
{
	(void)sig;
	(void)uc;
	usr1_pid = si->si_pid;
	usr1_code = si->si_code;
}

static void on_chld(int sig, siginfo_t *si, void *uc)
{
	(void)sig;
	(void)uc;
	chld_pid = si->si_pid;
	chld_code = si->si_code;
}

static int catch_signals(void)
{
	struct sigaction sa = { 0 };
	sa.sa_sigaction = on_usr1;
	sa.sa_flags = SA_SIGINFO;
	if (sigaction(SIGUSR1, &sa, 0))
		return -1;
	memset(&sa, 0, sizeof sa);
	sa.sa_sigaction = on_chld;
	sa.sa_flags = SA_SIGINFO;
	return sigaction(SIGCHLD, &sa, 0);
}

static int body_init(void)
{
	long c = syscall(SYS_clone, CLONE_NEWUSER | CLONE_NEWPID | SIGCHLD, 0, 0, 0, 0);
	if (c < 0)
		return 2;
	if (c == 0) {
		if (write_identity_maps())
			_exit(10);
		if (getpid() != 1)
			_exit(11);
		if (syscall(SYS_gettid) != 1)
			_exit(12);
		if (getppid() != 0)
			_exit(13);
		if (syscall(SYS_set_tid_address, 0) != 1)
			_exit(14);
		if (getpgrp() != 0 || getpgid(0) != 0 || getsid(0) != 0)
			_exit(15);
		if (setpgid(0, 0) || getpgrp() != 1 || getpgid(0) != 1)
			_exit(16);
		void *hp = 0;
		size_t rlen = 0;
		if (syscall(SYS_get_robust_list, 1, &hp, &rlen))
			_exit(17);
		struct rlimit rl;
		if (syscall(SYS_prlimit64, 1, RLIMIT_NOFILE, 0, &rl) || rl.rlim_cur == 0)
			_exit(18);
		errno = 0;
		if (syscall(SYS_prlimit64, outside_pid, RLIMIT_NOFILE, 0, &rl) != -1 || errno != ESRCH)
			_exit(19);
		cpu_set_t aff;
		CPU_ZERO(&aff);
		if (sched_getaffinity(1, sizeof aff, &aff))
			_exit(20);
		if (prctl(PR_SET_PTRACER, 1, 0, 0, 0))
			_exit(21);
		errno = 0;
		if (kill(outside_pid, 0) != -1 || errno != ESRCH)
			_exit(22);
		errno = 0;
		if (kill(-outside_pid, 0) != -1 || errno != ESRCH)
			_exit(23);
		errno = 0;
		if (waitpid(-1, 0, WNOHANG) != -1 || errno != ECHILD)
			_exit(24);
		errno = 0;
		if (waitpid(outside_pid, 0, WNOHANG) != -1 || errno != ECHILD)
			_exit(25);
		errno = 0;
		if (getpgid(outside_pid) != -1 || errno != ESRCH)
			_exit(26);
		char v[256];
		if (status_field("/proc/self/status", "NSpid", v, sizeof v))
			_exit(27);
		char *fields[8];
		int nf = split_tabs(v, fields, 8);
		if (nf < 1 || strcmp(fields[nf - 1], "1"))
			_exit(28);
		char want[32];
		if (!field_equals("/proc/self/status", "Pid", fields[0]) ||
		    !field_equals("/proc/self/status", "Tgid", fields[0]))
			_exit(34);
		snprintf(want, sizeof want, "%d", (int)outside_pid);
		if (!field_equals("/proc/self/status", "PPid", want))
			_exit(35);
		if (!dir_has("/proc", want))
			_exit(36);
		_exit(0);
	}
	siginfo_t si;
	memset(&si, 0, sizeof si);
	if (waitid(P_PID, (int)c, &si, WEXITED | WNOWAIT) || si.si_pid != c)
		return 3;
	char path[64], want[64];
	snprintf(path, sizeof path, "/proc/%ld/status", c);
	snprintf(want, sizeof want, "%ld", c);
	if (!field_equals(path, "Pid", want) || !field_equals(path, "Tgid", want))
		return 4;
	snprintf(want, sizeof want, "%d", (int)getpid());
	if (!field_equals(path, "PPid", want))
		return 5;
	snprintf(want, sizeof want, "%ld\t1", c);
	if (!field_equals(path, "NSpid", want))
		return 6;
	char a[64] = "", b[64] = "";
	ssize_t n = readlink("/proc/self/ns/pid", a, sizeof a - 1);
	if (n <= 0)
		return 7;
	a[n] = 0;
	snprintf(path, sizeof path, "/proc/%ld/ns/pid", c);
	n = readlink(path, b, sizeof b - 1);
	if (n <= 0)
		return 8;
	b[n] = 0;
	if (!strcmp(a, b))
		return 9;
	snprintf(want, sizeof want, "%ld", c);
	if (!dir_has("/proc", want))
		return 10;
	int st = wait_status((pid_t)c);
	if (child_exit_code(st))
		return 100 + child_exit_code(st);
	return 0;
}

static int body_unshare(void)
{
	if (unshare(CLONE_NEWUSER))
		return 2;
	if (write_identity_maps())
		return 3;
	pid_t before = getpid();
	if (unshare(CLONE_NEWPID))
		return 4;
	if (getpid() != before)
		return 5;
	int ptid = -1, ctid = -1;
#if defined(__aarch64__)
	long c = syscall(SYS_clone, CLONE_PARENT_SETTID | CLONE_CHILD_SETTID | SIGCHLD, 0,
		&ptid, 0, &ctid);
#else
	long c = syscall(SYS_clone, CLONE_PARENT_SETTID | CLONE_CHILD_SETTID | SIGCHLD, 0,
		&ptid, &ctid, 0);
#endif
	if (c < 0)
		return 6;
	if (c == 0) {
		if (getpid() != 1 || syscall(SYS_gettid) != 1 || getppid() != 0)
			_exit(10);
		if (ctid != 1)
			_exit(11);
		_exit(0);
	}
	if (ptid != c)
		return 7;
	int st = wait_status((pid_t)c);
	if (child_exit_code(st))
		return 100 + child_exit_code(st);
	errno = 0;
	pid_t f = fork();
	if (f != -1 || errno != ENOMEM)
		return 8;
	errno = 0;
	if (unshare(CLONE_NEWPID) != -1 || errno != EINVAL)
		return 9;
	return 0;
}

static int body_nested(void)
{
	int rep[2];
	if (pipe(rep))
		return 2;
	long c = syscall(SYS_clone, CLONE_NEWUSER | CLONE_NEWPID | SIGCHLD, 0, 0, 0, 0);
	if (c < 0)
		return 3;
	if (c == 0) {
		if (write_identity_maps())
			_exit(10);
		if (getpid() != 1)
			_exit(11);
		long g = syscall(SYS_clone, CLONE_NEWPID | SIGCHLD, 0, 0, 0, 0);
		if (g < 0)
			_exit(12);
		if (g == 0) {
			if (getpid() != 1 || syscall(SYS_gettid) != 1 || getppid() != 0)
				_exit(20);
			char v[128];
			if (status_field("/proc/self/status", "NSpid", v, sizeof v))
				_exit(21);
			ssize_t n = write(rep[1], v, strlen(v));
			_exit(n == (ssize_t)strlen(v) ? 0 : 22);
		}
		char v[128];
		ssize_t n = read(rep[0], v, sizeof v - 1);
		if (n <= 0)
			_exit(13);
		v[n] = 0;
		char *fields[8];
		int nf = split_tabs(v, fields, 8);
		if (nf < 1 || strcmp(fields[nf - 1], "1"))
			_exit(14);
		if (nf < 2 || atoi(fields[nf - 2]) != g)
			_exit(17);
		if (kill((pid_t)g, 0))
			_exit(18);
		int st = wait_status((pid_t)g);
		if (child_exit_code(st))
			_exit(19);
		_exit(0);
	}
	close(rep[1]);
	siginfo_t si;
	memset(&si, 0, sizeof si);
	if (waitid(P_PID, (int)c, &si, WEXITED | WNOWAIT) || si.si_pid != c)
		return 3;
	char path[64], want[64];
	snprintf(path, sizeof path, "/proc/%ld/status", c);
	snprintf(want, sizeof want, "%ld\t1", c);
	if (!field_equals(path, "NSpid", want))
		return 4;
	int st = wait_status((pid_t)c);
	if (child_exit_code(st))
		return 100 + child_exit_code(st);
	return 0;
}

static int body_signals(void)
{
	int ready[2];
	if (pipe(ready))
		return 2;
	long c = syscall(SYS_clone, CLONE_NEWUSER | CLONE_NEWPID | SIGCHLD, 0, 0, 0, 0);
	if (c < 0)
		return 3;
	if (c == 0) {
		close(ready[0]);
		if (write_identity_maps())
			_exit(10);
		errno = 0;
		if (kill(outside_pid, 0) != -1 || errno != ESRCH)
			_exit(25);
		errno = 0;
		if (kill(-outside_pid, 0) != -1 || errno != ESRCH)
			_exit(26);
		if (kill(getpid(), SIGTERM))
			_exit(11);
		if (kill(getpid(), SIGKILL))
			_exit(12);
		if (catch_signals())
			_exit(13);
		pid_t f = fork();
		if (f < 0)
			_exit(14);
		if (f == 0) {
			if (kill(1, SIGTERM) || kill(1, SIGKILL))
				_exit(20);
			_exit(0);
		}
		int st;
		while (waitpid(f, &st, 0) < 0 && errno == EINTR)
			;
		if (child_exit_code(st))
			_exit(15);
		pid_t g = fork();
		if (g < 0)
			_exit(16);
		if (g == 0) {
			if (kill(1, SIGUSR1))
				_exit(23);
			_exit(0);
		}
		while (waitpid(g, &st, 0) < 0 && errno == EINTR)
			;
		if (child_exit_code(st))
			_exit(17);
		if (usr1_pid != g || usr1_code != SI_USER)
			_exit(18);
		if (chld_pid != g || chld_code != CLD_EXITED)
			_exit(19);
		char b = 'r';
		if (write(ready[1], &b, 1) != 1)
			_exit(24);
		for (;;)
			pause();
	}
	close(ready[1]);
	char b;
	if (read(ready[0], &b, 1) != 1)
		return 4;
	if (kill((pid_t)c, SIGTERM))
		return 5;
	usleep(300000);
	int st;
	pid_t w = waitpid((pid_t)c, &st, WNOHANG);
	if (w != 0)
		return 6;
	if (kill((pid_t)c, 0))
		return 7;
	if (kill((pid_t)c, SIGKILL))
		return 8;
	st = wait_status((pid_t)c);
	if (!WIFSIGNALED(st) || WTERMSIG(st) != SIGKILL)
		return 9;
	return 0;
}

static int body_init_exit(void)
{
	if (unshare(CLONE_NEWUSER))
		return 2;
	if (write_identity_maps())
		return 3;
	if (unshare(CLONE_NEWPID))
		return 4;
	int watch[2];
	if (pipe(watch))
		return 5;
	pid_t c = fork();
	if (c < 0)
		return 6;
	if (c == 0) {
		close(watch[0]);
		pid_t g = fork();
		if (g < 0)
			_exit(10);
		if (g == 0)
			for (;;)
				pause();
		if (write(watch[1], &g, sizeof g) != (ssize_t)sizeof g)
			_exit(11);
		for (;;)
			pause();
	}
	close(watch[1]);
	pid_t g = 0;
	if (read(watch[0], &g, sizeof g) != (ssize_t)sizeof g)
		return 7;
	if (kill(c, SIGKILL))
		return 8;
	int st = wait_status(c);
	if (!WIFSIGNALED(st) || WTERMSIG(st) != SIGKILL)
		return 9;
	char b;
	if (read(watch[0], &b, 1) != 0)
		return 10;
	errno = 0;
	pid_t f = fork();
	if (f != -1 || errno != ENOMEM)
		return 11;
	return 0;
}

static int body_orphan(void)
{
	int rep[2];
	if (pipe(rep))
		return 2;
	long c = syscall(SYS_clone, CLONE_NEWUSER | CLONE_NEWPID | SIGCHLD, 0, 0, 0, 0);
	if (c < 0)
		return 3;
	if (c == 0) {
		if (write_identity_maps())
			_exit(10);
		pid_t a = fork();
		if (a < 0)
			_exit(11);
		if (a == 0) {
			pid_t b = fork();
			if (b < 0)
				_exit(20);
			if (b == 0) {
				for (int i = 0; i < 500 && getppid() != 1; i++)
					usleep(10000);
				if (getppid() != 1)
					_exit(21);
				char ok = 'k';
				if (write(rep[1], &ok, 1) != 1)
					_exit(22);
				_exit(7);
			}
			if (write(rep[1], &b, sizeof b) != (ssize_t)sizeof b)
				_exit(23);
			_exit(0);
		}
		pid_t b = 0;
		if (read(rep[0], &b, sizeof b) != (ssize_t)sizeof b)
			_exit(12);
		int st = wait_status(a);
		if (child_exit_code(st))
			_exit(13);
		char ok;
		if (read(rep[0], &ok, 1) != 1 || ok != 'k')
			_exit(14);
		siginfo_t si;
		memset(&si, 0, sizeof si);
		if (waitid(P_ALL, 0, &si, WEXITED))
			_exit(15);
		if (si.si_pid != b || si.si_code != CLD_EXITED || si.si_status != 7)
			_exit(16);
		_exit(0);
	}
	close(rep[1]);
	int st = wait_status((pid_t)c);
	if (child_exit_code(st))
		return 100 + child_exit_code(st);
	return 0;
}

static int body_proc_shared_view(void)
{
	int ready[2], go[2];
	if (pipe(ready) || pipe(go))
		return 2;
	long c = syscall(SYS_clone, CLONE_NEWUSER | CLONE_NEWPID | SIGCHLD, 0, 0, 0, 0);
	if (c < 0)
		return 3;
	if (c == 0) {
		char ch;
		close(ready[0]);
		close(go[1]);
		int dir = open("/proc/self/fdinfo", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
		if (dir < 0)
			_exit(10);
		struct stat st;
		if (stat("/proc", &st))
			_exit(11);
		if (write(ready[1], "r", 1) != 1 || read(go[0], &ch, 1) != 1)
			_exit(12);
		if (fstatat(dir, "0", &st, 0) || stat("/proc/self/status", &st))
			_exit(13);
		_exit(0);
	}
	close(ready[1]);
	close(go[0]);
	char ch;
	if (read(ready[0], &ch, 1) != 1)
		return 4;
	char path[64];
	snprintf(path, sizeof path, "/proc/%ld", c);
	struct stat st;
	int seen = !stat(path, &st) && S_ISDIR(st.st_mode);
	int listed = dir_has("/proc", path + 6);
	write(go[1], "g", 1);
	int code = child_exit_code(wait_status((pid_t)c));
	if (!seen || !listed)
		return 5;
	return code ? 20 + code : 0;
}

static int run_runner(int (*fn)(void))
{
	pid_t pid = fork();
	if (pid < 0)
		return -1;
	if (pid == 0) {
		outside_pid = getpid();
		_exit(fn());
	}
	return child_exit_code(wait_status(pid));
}

static void run_check(const char *name, int (*fn)(void))
{
	current_check = name;
	alarm(CHECK_DEADLINE_SECONDS);
	int code = run_runner(fn);
	alarm(0);
	record(name, code == 0, "stage=%d", code);
}

int main(int argc, char **argv)
{
	(void)argc;
	(void)argv;
	map_uid = getuid();
	map_gid = getgid();
	signal(SIGALRM, deadline);
	run_check("pidns_init", body_init);
	run_check("pidns_unshare", body_unshare);
	run_check("pidns_nested", body_nested);
	run_check("pidns_signals", body_signals);
	run_check("pidns_init_exit", body_init_exit);
	run_check("pidns_orphan", body_orphan);
	run_check("pidns_proc_shared_view", body_proc_shared_view);
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n",
		failed ? "FAIL" : "PASS", passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
