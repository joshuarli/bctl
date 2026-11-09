#include <errno.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

enum { ITERATIONS = 2000, VARIANT_DEADLINE_SECONDS = 60 };

static int passed, failed;
static const char *volatile current_variant = "";
static volatile int current_iteration;

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
		"MINI_CHROMIUM_TEST name=%s status=FAIL detail=hung-at-iteration-%d\n"
		"MINI_CHROMIUM_RESULT status=FAIL passed=%d failed=%d\n",
		current_variant, current_iteration, passed, failed + 1);
	write(1, line, (size_t)len);
	_exit(1);
}

enum child_body { BLOCKED_READ, SPINNING, EXIT_AT_ONCE, PAUSED };

static void run_child(enum child_body body, int hold[2])
{
	char c;
	switch (body) {
	case BLOCKED_READ:
		close(hold[1]);
		read(hold[0], &c, 1);
		break;
	case SPINNING:
		for (volatile unsigned long n = 0;; n++)
			;
	case EXIT_AT_ONCE:
		break;
	case PAUSED:
		for (;;)
			pause();
	}
	_exit(0);
}

static const char *only;

static void variant(const char *name, enum child_body body, int sig, int settle)
{
	if (only && !strstr(name, only))
		return;
	int bad = 0, first_bad = -1, bad_status = 0;
	current_variant = name;
	alarm(VARIANT_DEADLINE_SECONDS);
	for (int i = 0; i < ITERATIONS; i++) {
		int hold[2], ready[2];
		current_iteration = i;
		if (pipe(hold) || pipe(ready)) {
			record(name, 0, "pipe-errno=%d", errno);
			return;
		}
		pid_t child = fork();
		if (child < 0) {
			record(name, 0, "fork-errno=%d,iteration=%d", errno, i);
			return;
		}
		if (child == 0) {
			if (settle)
				write(ready[1], "r", 1);
			run_child(body, hold);
		}
		close(hold[0]);
		if (settle) {
			char c;
			read(ready[0], &c, 1);
		}
		int killed = kill(child, sig);
		int status = 0;
		pid_t got = waitpid(child, &status, 0);
		close(hold[1]);
		close(ready[0]);
		close(ready[1]);
		int ok = killed == 0 && got == child &&
			 (body == EXIT_AT_ONCE ? (WIFEXITED(status) && WEXITSTATUS(status) == 0) ||
							 (WIFSIGNALED(status) && WTERMSIG(status) == sig)
					       : WIFSIGNALED(status) && WTERMSIG(status) == sig);
		if (!ok && bad++ == 0) {
			first_bad = i;
			bad_status = got == child ? status : -errno;
		}
	}
	alarm(0);
	record(name, bad == 0, "iterations=%d,bad=%d,first_bad=%d,status=0x%x", ITERATIONS, bad, first_bad, bad_status);
}

int main(int argc, char **argv)
{
	only = argc > 1 ? argv[1] : 0;
	signal(SIGALRM, deadline);
	variant("kill_race_sigkill_blocked_read", BLOCKED_READ, SIGKILL, 0);
	variant("kill_race_sigkill_spinning", SPINNING, SIGKILL, 0);
	variant("kill_race_sigkill_exit_at_once", EXIT_AT_ONCE, SIGKILL, 0);
	variant("kill_race_sigkill_paused", PAUSED, SIGKILL, 0);
	variant("kill_race_sigkill_settled_read", BLOCKED_READ, SIGKILL, 1);
	variant("kill_race_sigterm_blocked_read", BLOCKED_READ, SIGTERM, 0);
	variant("kill_race_sigterm_spinning", SPINNING, SIGTERM, 0);
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
