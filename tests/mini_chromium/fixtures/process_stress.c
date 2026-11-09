#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed, failed;
static char self[256];

static void record(const char *name, int ok, const char *detail)
{
	char line[256];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name, ok ? "PASS" : "FAIL", detail);
	write(1, line, (size_t)n);
	ok ? passed++ : failed++;
}

static pid_t spawn_exit_code(int code)
{
	char arg[16];
	snprintf(arg, sizeof arg, "%d", code);
	pid_t pid = fork();
	if (pid == 0) {
		char *argv[] = { self, "--exit", arg, 0 };
		execv(self, argv);
		_exit(120);
	}
	return pid;
}

static void *spin(void *arg)
{
	(void)arg;
	for (;;)
		pause();
	return 0;
}

static void *crash(void *arg)
{
	(void)arg;
	*(volatile int *)8 = 1;
	return 0;
}

static void *leave(void *arg)
{
	(void)arg;
	exit(23);
	return 0;
}

static int multithreaded_child(void *(*fn)(void *))
{
	pthread_t t[3];
	for (int i = 0; i < 2; i++)
		pthread_create(&t[i], 0, spin, 0);
	if (fn)
		pthread_create(&t[2], 0, fn, 0);
	for (;;)
		pause();
}

int main(int argc, char **argv)
{
	if (argc == 3 && !strcmp(argv[1], "--exit"))
		return atoi(argv[2]);
	snprintf(self, sizeof self, "%s", argv[0][0] == '/' ? argv[0] : "/process_stress");
	if (argv[0][0] != '/' && realpath(argv[0], self) == 0)
		snprintf(self, sizeof self, "/process_stress");

	char detail[128];
	int bad = 0;
	for (int i = 0; i < 1000; i++) {
		int status = 0;
		pid_t pid = spawn_exit_code(i % 100);
		if (pid <= 0 || waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status) != i % 100)
			bad++;
	}
	snprintf(detail, sizeof detail, "cycles=1000,bad=%d", bad);
	record("fork_exec_wait_1000", bad == 0, detail);

	pid_t pids[64];
	bad = 0;
	for (int i = 0; i < 64; i++)
		pids[i] = spawn_exit_code(i + 1);
	int reaped = 0;
	for (;;) {
		int status;
		pid_t r = wait(&status);
		if (r < 0)
			break;
		for (int i = 0; i < 64; i++)
			if (pids[i] == r) {
				reaped++;
				if (!WIFEXITED(status) || WEXITSTATUS(status) != i + 1)
					bad++;
			}
	}
	int echild = errno == ECHILD;
	snprintf(detail, sizeof detail, "reaped=%d,bad=%d,echild=%d", reaped, bad, echild);
	record("concurrent_children_64", reaped == 64 && bad == 0 && echild, detail);

	struct {
		const char *name;
		void *(*fn)(void *);
		int want_signal;
		int want_exit;
		int kill_it;
	} cases[] = {
		{ "thread_fault_kills_process", crash, SIGSEGV, -1, 0 },
		{ "thread_exit_group", leave, 0, 23, 0 },
		{ "sigkill_multithreaded", 0, SIGKILL, -1, 1 },
		{ "sigterm_default_multithreaded", 0, SIGTERM, -1, 2 },
	};
	for (size_t c = 0; c < sizeof cases / sizeof cases[0]; c++) {
		int ready[2];
		pipe(ready);
		pid_t pid = fork();
		if (pid == 0) {
			close(ready[0]);
			if (cases[c].kill_it)
				write(ready[1], "r", 1);
			multithreaded_child(cases[c].fn);
		}
		close(ready[1]);
		if (cases[c].kill_it) {
			char ch;
			read(ready[0], &ch, 1);
			kill(pid, cases[c].kill_it == 1 ? SIGKILL : SIGTERM);
		}
		close(ready[0]);
		int status = 0;
		pid_t r = waitpid(pid, &status, 0);
		int ok = r == pid && (cases[c].want_signal ? WIFSIGNALED(status) && WTERMSIG(status) == cases[c].want_signal
						: WIFEXITED(status) && WEXITSTATUS(status) == cases[c].want_exit);
		snprintf(detail, sizeof detail, "status=0x%x", status);
		record(cases[c].name, ok, detail);
	}

	char line[96];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	write(1, line, (size_t)n);
	return failed ? 1 : 0;
}
