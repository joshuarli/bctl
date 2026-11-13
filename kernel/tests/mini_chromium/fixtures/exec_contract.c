#define _GNU_SOURCE
#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum { CHECK_DEADLINE_SECONDS = 20, ARG_STRLEN_MAX = 32 * 4096 };

extern char **environ;
extern void _start(void);

static int passed, failed;
static const char *volatile current_check = "";
static char self[4096], helper[4096], not_elf[4096];

static void record(const char *name, int ok, const char *fmt, ...)
{
	char detail[192] = "", line[320];
	if (fmt) {
		va_list ap;
		va_start(ap, fmt);
		vsnprintf(detail, sizeof detail, fmt, ap);
		va_end(ap);
	}
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s%s%s\n", name, ok ? "PASS" : "FAIL",
		detail[0] ? " detail=" : "", detail);
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
	do
		r = waitpid(pid, &status, 0);
	while (r < 0 && errno == EINTR);
	return r == pid ? status : -1;
}

static int exit_code(pid_t pid)
{
	int status = wait_status(pid);
	return status >= 0 && WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

static char *const verify_argv_tail[] = { "a b", "", "\xc3\xbc", "--last", 0 };
static char *const verify_envp[] = { "EXEC_CONTRACT=verify", "EMPTY=", "SPACED=x y", 0 };

static int role_verify_args(int argc, char **argv)
{
	int n = sizeof verify_argv_tail / sizeof verify_argv_tail[0] - 1;
	if (argc != n + 2 || strcmp(argv[0], self) != 0)
		return 20;
	for (int i = 0; i < n; i++)
		if (strcmp(argv[i + 2], verify_argv_tail[i]) != 0)
			return 21;
	if (argv[argc] != 0)
		return 22;
	int envc = 0;
	while (environ[envc])
		envc++;
	if (envc != 3)
		return 23;
	for (int i = 0; i < envc; i++)
		if (strcmp(environ[i], verify_envp[i]) != 0)
			return 24;
	const Elf64_Ehdr *e = (const Elf64_Ehdr *)(getauxval(AT_PHDR) & ~0xfffUL);
	if (memcmp(e->e_ident, ELFMAG, SELFMAG) != 0)
		return 25;
	if ((unsigned long)e + e->e_phoff != getauxval(AT_PHDR) || e->e_phnum != getauxval(AT_PHNUM) ||
	    getauxval(AT_PHENT) != sizeof(Elf64_Phdr))
		return 26;
	if (getauxval(AT_ENTRY) != (unsigned long)_start)
		return 27;
	const unsigned char *random = (const unsigned char *)getauxval(AT_RANDOM);
	int nonzero = 0;
	for (int i = 0; random && i < 16; i++)
		nonzero |= random[i];
	if (!nonzero)
		return 28;
	const char *execfn = (const char *)getauxval(AT_EXECFN);
	if (!execfn || strcmp(execfn, self) != 0)
		return 29;
	if (getauxval(AT_PAGESZ) != 4096)
		return 30;
	if (getauxval(AT_BASE) != 0)
		return 31;
	return 0;
}

static void check_exec_self(void)
{
	pid_t pid = fork();
	if (pid == 0) {
		char *argv[8] = { self, "--role=verify-args" };
		for (int i = 0; verify_argv_tail[i]; i++)
			argv[i + 2] = verify_argv_tail[i];
		execve(self, argv, verify_envp);
		_exit(126);
	}
	int code = exit_code(pid);
	record("exec_self_args_auxv", code == 0, "exit=%d", code);
}

static void check_exec_dynamic(void)
{
	char *argv[] = { helper, "dynamic", 0 };
	char *envp[] = { "EXEC_CONTRACT=helper", 0 };
	pid_t pid = fork();
	if (pid == 0) {
		execve(helper, argv, envp);
		_exit(126);
	}
	int code = exit_code(pid);
	pid_t spawned = -1;
	int rc = posix_spawn(&spawned, helper, 0, 0, argv, envp);
	int spawn_code = rc == 0 ? exit_code(spawned) : -1;
	record("exec_dynamic_helper", code == 0 && rc == 0 && spawn_code == 0, "exit=%d,spawn_rc=%d,spawn_exit=%d", code,
		rc, spawn_code);
}

static int parse_fds(const char *s, int *fds, int max)
{
	int n = 0;
	while (*s && n < max) {
		char *end;
		fds[n++] = (int)strtol(s, &end, 10);
		s = *end == ',' ? end + 1 : end;
	}
	return n;
}

static int role_fds(char **argv)
{
	int closed[8], open_fds[8];
	int nclosed = parse_fds(argv[2], closed, 8), nopen = parse_fds(argv[3], open_fds, 8);
	for (int i = 0; i < nclosed; i++) {
		errno = 0;
		if (fcntl(closed[i], F_GETFD) != -1 || errno != EBADF)
			return 40 + i;
	}
	for (int i = 0; i < nopen; i++)
		if (fcntl(open_fds[i], F_GETFD) != 0)
			return 50 + i;
	if (write(open_fds[0], "k", 1) != 1)
		return 60;
	return 0;
}

static void check_cloexec(void)
{
	int with_flag[2], without_flag[2], set_later[2];
	if (pipe2(with_flag, O_CLOEXEC) || pipe(without_flag) || pipe(set_later)) {
		record("exec_cloexec", 0, "pipe-errno=%d", errno);
		return;
	}
	int getfd_set = fcntl(with_flag[0], F_GETFD);
	int getfd_clear = fcntl(without_flag[0], F_GETFD);
	fcntl(set_later[0], F_SETFD, FD_CLOEXEC);
	int getfd_after_set = fcntl(set_later[0], F_GETFD);
	int dup_plain = dup(with_flag[0]);
	int getfd_dup = fcntl(dup_plain, F_GETFD);
	int dup_cloexec = dup3(without_flag[0], 100, O_CLOEXEC);
	int getfd_dup3 = fcntl(dup_cloexec, F_GETFD);
	int dupfd_cloexec = fcntl(without_flag[0], F_DUPFD_CLOEXEC, 50);
	int getfd_dupfd = fcntl(dupfd_cloexec, F_GETFD);
	char closed[64], open_fds[64];
	snprintf(closed, sizeof closed, "%d,%d,%d,%d,%d", with_flag[0], with_flag[1], set_later[0], dup_cloexec,
		dupfd_cloexec);
	snprintf(open_fds, sizeof open_fds, "%d,%d,%d,%d", without_flag[1], without_flag[0], set_later[1], dup_plain);
	pid_t pid = fork();
	if (pid == 0) {
		char *argv[] = { self, "--role=fds", closed, open_fds, 0 };
		execve(self, argv, environ);
		_exit(126);
	}
	close(without_flag[1]);
	char c = 0;
	ssize_t got = read(without_flag[0], &c, 1);
	int code = exit_code(pid);
	int flags_ok = getfd_set == FD_CLOEXEC && getfd_clear == 0 && getfd_after_set == FD_CLOEXEC &&
		       getfd_dup == 0 && dup_cloexec == 100 && getfd_dup3 == FD_CLOEXEC && dupfd_cloexec >= 50 &&
		       getfd_dupfd == FD_CLOEXEC;
	record("exec_cloexec", flags_ok && code == 0 && got == 1 && c == 'k',
		"exit=%d,read=%zd,getfd=%d/%d/%d/%d/%d/%d", code, got, getfd_set, getfd_clear, getfd_after_set,
		getfd_dup, getfd_dup3, getfd_dupfd);
	close(with_flag[0]);
	close(with_flag[1]);
	close(without_flag[0]);
	close(set_later[0]);
	close(set_later[1]);
	close(dup_plain);
	close(dup_cloexec);
	close(dupfd_cloexec);
}

static void *unmapped(void)
{
	void *volatile address = (void *)1;
	return address;
}

static int exec_errno(const char *path, char *const argv[], char *const envp[])
{
	pid_t pid = fork();
	if (pid == 0) {
		execve(path, argv, envp);
		_exit(errno);
	}
	return exit_code(pid);
}

static void check_errors(void)
{
	char *argv[] = { self, "--role=verify-args", 0 };
	char *envp[] = { 0 };
	int enoent = exec_errno("/exec-contract-does-not-exist", argv, envp);
	int enoexec = exec_errno(not_elf, argv, envp);
	int eacces = exec_errno("/", argv, envp);
	int efault_path = exec_errno(unmapped(), argv, envp);
	int efault_argv = exec_errno(self, unmapped(), envp);
	char *bad_string[] = { self, unmapped(), 0 };
	int efault_string = exec_errno(self, bad_string, envp);
	int efault_envp = exec_errno(self, argv, unmapped());
	record("exec_errors", enoent == ENOENT && enoexec == ENOEXEC && eacces == EACCES && efault_path == EFAULT &&
		efault_argv == EFAULT && efault_string == EFAULT && efault_envp == EFAULT,
		"enoent=%d,enoexec=%d,eacces=%d,efault=%d/%d/%d/%d", enoent, enoexec, eacces, efault_path, efault_argv,
		efault_string, efault_envp);
}

static void check_e2big(void)
{
	char *long_arg = malloc(ARG_STRLEN_MAX + 1);
	memset(long_arg, 'x', ARG_STRLEN_MAX);
	long_arg[ARG_STRLEN_MAX] = 0;
	char *argv[] = { self, long_arg, 0 };
	char *envp[] = { 0 };
	int single = exec_errno(self, argv, envp);
	enum { CHUNKS = 80, CHUNK = 100000 };
	char *chunk = malloc(CHUNK);
	memset(chunk, 'y', CHUNK - 1);
	chunk[CHUNK - 1] = 0;
	char *many[CHUNKS + 1];
	for (int i = 0; i < CHUNKS; i++)
		many[i] = chunk;
	many[CHUNKS] = 0;
	char *short_argv[] = { self, 0 };
	int total = exec_errno(self, short_argv, many);
	record("exec_e2big", single == E2BIG && total == E2BIG, "single=%d,total=%d", single, total);
	free(chunk);
	free(long_arg);
}

static volatile unsigned long *shared_counter;
static int blocker_fd;

static void *spinner(void *arg)
{
	(void)arg;
	for (;;)
		__atomic_add_fetch(shared_counter, 1, __ATOMIC_RELAXED);
	return 0;
}

static void *blocker(void *arg)
{
	(void)arg;
	char c;
	read(blocker_fd, &c, 1);
	__atomic_store_n(&shared_counter[1], 1, __ATOMIC_RELAXED);
	return 0;
}

struct exec_from_thread {
	char ready[16], go[16];
};

static void *execer(void *arg)
{
	struct exec_from_thread *x = arg;
	char *argv[] = { self, "--role=after-threads", x->ready, x->go, 0 };
	execve(self, argv, environ);
	_exit(126);
}

static int role_after_threads(char **argv)
{
	int ready = atoi(argv[2]), go = atoi(argv[3]);
	if (getpid() != (pid_t)syscall(SYS_gettid))
		return 70;
	char c = 'r';
	if (write(ready, &c, 1) != 1)
		return 71;
	if (read(go, &c, 1) != 1)
		return 72;
	return 0;
}

static void check_exec_multithreaded(void)
{
	shared_counter = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	int ready[2], go[2], block[2];
	if (shared_counter == MAP_FAILED || pipe(ready) || pipe(go) || pipe(block)) {
		record("exec_multithreaded", 0, "setup-errno=%d", errno);
		return;
	}
	pid_t pid = fork();
	if (pid == 0) {
		pthread_t t;
		blocker_fd = block[0];
		pthread_create(&t, 0, spinner, 0);
		pthread_create(&t, 0, blocker, 0);
		while (__atomic_load_n(shared_counter, __ATOMIC_RELAXED) < 1000)
			;
		static struct exec_from_thread x;
		snprintf(x.ready, sizeof x.ready, "%d", ready[1]);
		snprintf(x.go, sizeof x.go, "%d", go[0]);
		pthread_create(&t, 0, execer, &x);
		for (;;)
			pause();
	}
	char c;
	ssize_t r = read(ready[0], &c, 1);
	unsigned long before = __atomic_load_n(shared_counter, __ATOMIC_RELAXED);
	struct timespec pause_for = { 0, 100 * 1000 * 1000 };
	nanosleep(&pause_for, 0);
	unsigned long after = __atomic_load_n(shared_counter, __ATOMIC_RELAXED);
	write(block[1], "b", 1);
	nanosleep(&pause_for, 0);
	unsigned long blocker_ran = __atomic_load_n(&shared_counter[1], __ATOMIC_RELAXED);
	write(go[1], "g", 1);
	int code = exit_code(pid);
	record("exec_multithreaded", r == 1 && before >= 1000 && before == after && !blocker_ran && code == 0,
		"ready=%zd,counter=%lu/%lu,blocker_ran=%lu,exit=%d", r, before, after, blocker_ran, code);
	close(ready[0]);
	close(ready[1]);
	close(go[0]);
	close(go[1]);
	close(block[0]);
	close(block[1]);
	munmap((void *)shared_counter, 4096);
}

static int role_wait_pipe(char **argv)
{
	char c;
	return read(atoi(argv[2]), &c, 1) == 1 ? 3 : 80;
}

static void check_vfork_exec(void)
{
	int gate[2];
	if (pipe(gate)) {
		record("exec_vfork_resumes_parent", 0, "pipe-errno=%d", errno);
		return;
	}
	char fd[16];
	snprintf(fd, sizeof fd, "%d", gate[0]);
	char *argv[] = { self, "--role=wait-pipe", fd, 0 };
	pid_t pid = vfork();
	if (pid == 0) {
		execve(self, argv, environ);
		_exit(126);
	}
	ssize_t w = write(gate[1], "g", 1);
	int code = exit_code(pid);
	record("exec_vfork_resumes_parent", pid > 0 && w == 1 && code == 3, "pid=%d,exit=%d", pid, code);
	close(gate[0]);
	close(gate[1]);
}

static void check_unlimited_stack(void)
{
	pid_t pid = fork();
	if (pid == 0) {
		struct rlimit rl = { RLIM_INFINITY, RLIM_INFINITY };
		if (setrlimit(RLIMIT_STACK, &rl))
			_exit(125);
		char *argv[8] = { self, "--role=verify-args" };
		for (int i = 0; verify_argv_tail[i]; i++)
			argv[i + 2] = verify_argv_tail[i];
		execve(self, argv, verify_envp);
		_exit(126);
	}
	int code = exit_code(pid);
	record("exec_unlimited_stack", code == 0, "exit=%d", code);
}

static void on_usr1(int sig)
{
	(void)sig;
}

static int role_signals(void)
{
	struct sigaction sa;
	if (sigaction(SIGUSR1, 0, &sa) || sa.sa_handler != SIG_DFL || sa.sa_flags & SA_SIGINFO)
		return 90;
	if (sigaction(SIGUSR2, 0, &sa) || sa.sa_handler != SIG_IGN)
		return 91;
	if (sigaction(SIGCHLD, 0, &sa) || sa.sa_handler != SIG_DFL)
		return 92;
	sigset_t mask;
	if (sigprocmask(SIG_BLOCK, 0, &mask) || !sigismember(&mask, SIGTERM) || sigismember(&mask, SIGUSR1))
		return 93;
	return 0;
}

static void check_signal_reset(void)
{
	pid_t pid = fork();
	if (pid == 0) {
		struct sigaction sa = { 0 };
		sa.sa_handler = on_usr1;
		sa.sa_flags = SA_RESTART;
		sigaction(SIGUSR1, &sa, 0);
		sigaction(SIGCHLD, &sa, 0);
		signal(SIGUSR2, SIG_IGN);
		sigset_t mask;
		sigemptyset(&mask);
		sigaddset(&mask, SIGTERM);
		sigprocmask(SIG_BLOCK, &mask, 0);
		char *argv[] = { self, "--role=signals", 0 };
		execve(self, argv, environ);
		_exit(126);
	}
	int code = exit_code(pid);
	record("exec_signal_reset", code == 0, "exit=%d", code);
}

static void run_check(const char *name, void (*check)(void))
{
	current_check = name;
	alarm(CHECK_DEADLINE_SECONDS);
	check();
	alarm(0);
}

static void sibling_path(char *out, const char *name)
{
	const char *slash = strrchr(self, '/');
	if (slash)
		snprintf(out, 4096, "%.*s/%s", (int)(slash - self), self, name);
	else
		snprintf(out, 4096, "%s", name);
}

int main(int argc, char **argv)
{
	snprintf(self, sizeof self, "%s", argv[0]);
	if (argc > 1 && strncmp(argv[1], "--role=", 7) == 0) {
		const char *role = argv[1] + 7;
		if (!strcmp(role, "verify-args"))
			return role_verify_args(argc, argv);
		if (!strcmp(role, "fds") && argc == 4)
			return role_fds(argv);
		if (!strcmp(role, "after-threads") && argc == 4)
			return role_after_threads(argv);
		if (!strcmp(role, "wait-pipe") && argc == 3)
			return role_wait_pipe(argv);
		if (!strcmp(role, "signals"))
			return role_signals();
		return 99;
	}
	sibling_path(helper, "exec_contract_helper");
	snprintf(not_elf, sizeof not_elf, "exec_contract_not_elf");
	int fd = open(not_elf, O_WRONLY | O_CREAT | O_TRUNC, 0755);
	if (fd < 0 || write(fd, "#not an ELF file\n", 17) != 17 || close(fd)) {
		record("exec_setup", 0, "errno=%d", errno);
		return 1;
	}
	signal(SIGALRM, deadline);
	run_check("exec_self_args_auxv", check_exec_self);
	run_check("exec_dynamic_helper", check_exec_dynamic);
	run_check("exec_cloexec", check_cloexec);
	run_check("exec_errors", check_errors);
	run_check("exec_e2big", check_e2big);
	run_check("exec_multithreaded", check_exec_multithreaded);
	run_check("exec_vfork_resumes_parent", check_vfork_exec);
	run_check("exec_signal_reset", check_signal_reset);
	run_check("exec_unlimited_stack", check_unlimited_stack);
	unlink(not_elf);
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
