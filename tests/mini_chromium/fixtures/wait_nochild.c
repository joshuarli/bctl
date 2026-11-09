#include <errno.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed, failed;

static void expect(const char *name, long rc, int want_errno)
{
	int err = errno;
	int ok = rc == -1 && err == want_errno;
	char line[160];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s detail=rc=%ld,errno=%d,want=%d\n", name,
		ok ? "PASS" : "FAIL", rc, rc < 0 ? err : 0, want_errno);
	write(1, line, (size_t)n);
	ok ? passed++ : failed++;
}

int main(void)
{
	int status = 0x5a5a;
	siginfo_t info;
	errno = 0;
	expect("wait4_no_children", wait4(-1, &status, 0, 0), ECHILD);
	errno = 0;
	expect("wait4_wnohang_no_children", wait4(-1, &status, WNOHANG, 0), ECHILD);
	errno = 0;
	expect("wait4_specific_pid", wait4(4242, &status, 0, 0), ECHILD);
	errno = 0;
	expect("wait4_bad_options", wait4(-1, &status, 0x40000, 0), EINVAL);
	errno = 0;
	expect("waitid_no_children", waitid(P_ALL, 0, &info, WEXITED), ECHILD);
	errno = 0;
	expect("waitid_no_event_options", waitid(P_ALL, 0, &info, WNOHANG), EINVAL);
	errno = 0;
	expect("waitid_bad_idtype", waitid((idtype_t)77, 0, &info, WEXITED), EINVAL);
	char line[96];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	write(1, line, (size_t)n);
	return failed ? 1 : 0;
}
