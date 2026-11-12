#define _GNU_SOURCE
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *const entries[] = {"status", "stat", "cmdline"};

/* fstat on a procfs descriptor whose process has exited and been reaped. */
int main(void)
{
	alarm(30);
	int ok = 1, checked = 0;
	for (unsigned e = 0; e < sizeof entries / sizeof entries[0]; e++) {
		pid_t child = fork();
		if (child < 0)
			return 1;
		if (child == 0) {
			pause();
			_exit(0);
		}
		char path[64];
		snprintf(path, sizeof path, "/proc/%d/%s", child, entries[e]);
		int fd = open(path, O_RDONLY | O_CLOEXEC);
		int status;
		if (fd < 0 || kill(child, SIGKILL) || waitpid(child, &status, 0) != child) {
			ok = 0;
			continue;
		}
		struct stat st;
		int r = fstat(fd, &st);
		if (r != 0 && r != -1)
			ok = 0;
		close(fd);
		checked++;
	}
	ok = ok && checked == sizeof entries / sizeof entries[0];
	printf("MINI_CHROMIUM_TEST name=proc_fstat_exited status=%s detail=entries%d\n", ok ? "PASS" : "FAIL", checked);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
