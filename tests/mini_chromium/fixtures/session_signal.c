#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed, failed;

static void record(const char *name, int ok, const char *detail)
{
	printf("MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name, ok ? "PASS" : "FAIL", detail);
	fflush(stdout);
	if (ok)
		passed++;
	else
		failed++;
}

static int child_status(pid_t pid)
{
	int st = 0;
	return waitpid(pid, &st, 0) == pid ? st : -1;
}

static int exit_code(int st)
{
	return WIFEXITED(st) ? WEXITSTATUS(st) : 200 + (WIFSIGNALED(st) ? WTERMSIG(st) : 0);
}

static int child_setsid(void)
{
	pid_t self = getpid();
	if (setsid() != self)
		return 2;
	if (getsid(0) != self || getpgrp() != self || getpgid(0) != self)
		return 3;
	errno = 0;
	if (setsid() != -1 || errno != EPERM)
		return 4;
	errno = 0;
	if (setpgid(0, 0) != -1 || errno != EPERM)
		return 5;
	return 0;
}

static void check_setsid(void)
{
	char detail[64];
	pid_t parent_sid = getsid(0);
	pid_t pid = fork();
	if (pid == 0)
		_exit(child_setsid());
	int code = exit_code(child_status(pid));
	snprintf(detail, sizeof detail, "stage=%d,parent_sid_kept=%d", code, getsid(0) == parent_sid);
	record("session_setsid", code == 0 && getsid(0) == parent_sid, detail);
}

static void check_setpgid(void)
{
	char detail[96];
	int ready[2], hold[2];
	if (pipe(ready) || pipe(hold)) {
		record("session_setpgid", 0, "pipe-failed");
		return;
	}
	pid_t pid = fork();
	if (pid == 0) {
		char c;
		if (setpgid(0, 0) || getpgrp() != getpid())
			_exit(2);
		write(ready[1], "r", 1);
		read(hold[0], &c, 1);
		_exit(0);
	}
	char c;
	read(ready[0], &c, 1);
	int child_group = getpgid(pid) == pid;
	int same_session = getsid(pid) == getsid(0);
	errno = 0;
	int bad_group = setpgid(pid, 999999) == -1 && errno == EPERM;
	errno = 0;
	int not_child = setpgid(1, 0) == -1 && (errno == ESRCH || errno == EPERM);
	errno = 0;
	int negative = setpgid(-1, 0) == -1 && errno == EINVAL;
	errno = 0;
	int missing = getsid(999999) == -1 && errno == ESRCH;
	write(hold[1], "h", 1);
	int code = exit_code(child_status(pid));
	snprintf(detail, sizeof detail, "child_group=%d,same_session=%d,bad_group=%d,not_child=%d,negative=%d,missing=%d,code=%d",
		child_group, same_session, bad_group, not_child, negative, missing, code);
	record("session_setpgid", child_group && same_session && bad_group && not_child && negative && missing && code == 0,
		detail);
}

struct kernel_sigaction {
	void (*handler)(int);
	unsigned long flags;
	void (*restorer)(void);
	unsigned long mask;
};

static void on_usr1(int sig)
{
	(void)sig;
}

static int child_no_restorer_handler(void)
{
	struct kernel_sigaction ka = { on_usr1, 0, 0, 0 };
	if (syscall(SYS_rt_sigaction, SIGUSR1, &ka, 0, 8))
		return 2;
	kill(getpid(), SIGUSR1);
	return 3;
}

static void check_sigaction_without_restorer(void)
{
	char detail[96];
	struct kernel_sigaction dfl = { SIG_DFL, 0, 0, 0 }, ign = { SIG_IGN, 0, 0, 0 }, old;
	int set_dfl = syscall(SYS_rt_sigaction, SIGTERM, &dfl, 0, 8) == 0;
	int set_ign = syscall(SYS_rt_sigaction, SIGUSR2, &ign, &old, 8) == 0;
	int ignored = set_ign && kill(getpid(), SIGUSR2) == 0;
	syscall(SYS_rt_sigaction, SIGUSR2, &dfl, 0, 8);
	pid_t pid = fork();
	if (pid == 0)
		_exit(child_no_restorer_handler());
	int st = child_status(pid);
	int segv = WIFSIGNALED(st) && WTERMSIG(st) == SIGSEGV;
	snprintf(detail, sizeof detail, "set_dfl=%d,set_ign=%d,ignored=%d,handler_status=0x%x", set_dfl, set_ign, ignored, st);
	record("sigaction_without_restorer", set_dfl && set_ign && ignored && segv, detail);
}

int main(void)
{
	check_setsid();
	check_setpgid();
	check_sigaction_without_restorer();
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	return failed ? 1 : 0;
}
