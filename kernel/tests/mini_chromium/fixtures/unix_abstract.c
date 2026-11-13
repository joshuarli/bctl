#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef CLONE_NEWUSER
#define CLONE_NEWUSER 0x10000000
#endif
#ifndef CLONE_NEWNET
#define CLONE_NEWNET 0x40000000
#endif

static const char abstract_name[] = "\0hydrogen-abstract\0tail";
static int passed, failed;

static void record(const char *name, int ok, const char *detail)
{
	printf("MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name, ok ? "PASS" : "FAIL", detail);
	fflush(stdout);
	ok ? passed++ : failed++;
}

static socklen_t abstract_addr(struct sockaddr_un *sun)
{
	memset(sun, 0, sizeof *sun);
	sun->sun_family = AF_UNIX;
	memcpy(sun->sun_path, abstract_name, sizeof abstract_name - 1);
	return (socklen_t)(offsetof(struct sockaddr_un, sun_path) + sizeof abstract_name - 1);
}

static int stream_socket(void)
{
	return socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
}

static int connect_errno(void)
{
	struct sockaddr_un sun;
	socklen_t len = abstract_addr(&sun);
	int c = stream_socket();
	errno = 0;
	int r = connect(c, (struct sockaddr *)&sun, len) ? errno : 0;
	close(c);
	return r;
}

static int wait_code(pid_t pid)
{
	int status;
	if (waitpid(pid, &status, 0) != pid)
		return -1;
	return WIFEXITED(status) ? WEXITSTATUS(status) : 200 + (WIFSIGNALED(status) ? WTERMSIG(status) : 0);
}

static void check_abstract_bind(void)
{
	char detail[160];
	struct sockaddr_un sun, got;
	socklen_t len = abstract_addr(&sun), got_len = sizeof got;
	int l = stream_socket();
	int bound = l >= 0 && !bind(l, (struct sockaddr *)&sun, len) && !listen(l, 4);
	int name = bound && !getsockname(l, (struct sockaddr *)&got, &got_len) && got_len == len &&
		!memcmp(got.sun_path, abstract_name, sizeof abstract_name - 1);
	errno = 0;
	int rebind = bound && bind(l, (struct sockaddr *)&sun, len) == -1 && errno == EINVAL;
	int dup = stream_socket();
	errno = 0;
	int in_use = bind(dup, (struct sockaddr *)&sun, len) == -1 && errno == EADDRINUSE;
	close(dup);
	int connected = connect_errno() == 0;
	struct sockaddr_un prefix = sun;
	errno = 0;
	int c = stream_socket();
	int prefix_refused = connect(c, (struct sockaddr *)&prefix, len - 5) == -1 && errno == ECONNREFUSED;
	close(c);
	close(l);
	int freed = connect_errno() == ECONNREFUSED;
	int again = stream_socket();
	int reusable = !bind(again, (struct sockaddr *)&sun, len);
	close(again);
	snprintf(detail, sizeof detail, "bound=%d,name=%d,rebind=%d,in_use=%d,connected=%d,prefix_refused=%d,freed=%d,reusable=%d",
		bound, name, rebind, in_use, connected, prefix_refused, freed, reusable);
	record("unix_abstract_bind", bound && name && rebind && in_use && connected && prefix_refused && freed && reusable,
		detail);
}

static void check_autobind(void)
{
	char detail[96];
	struct sockaddr_un sun = { .sun_family = AF_UNIX }, got;
	socklen_t got_len = sizeof got;
	int s = stream_socket();
	int bound = !bind(s, (struct sockaddr *)&sun, offsetof(struct sockaddr_un, sun_path));
	int named = bound && !getsockname(s, (struct sockaddr *)&got, &got_len) &&
		got_len == offsetof(struct sockaddr_un, sun_path) + 6 && got.sun_path[0] == 0;
	int hex = named;
	for (int i = 1; hex && i < 6; i++)
		hex = (got.sun_path[i] >= '0' && got.sun_path[i] <= '9') || (got.sun_path[i] >= 'a' && got.sun_path[i] <= 'f');
	int t = stream_socket();
	int connected = hex && !listen(s, 1) && !connect(t, (struct sockaddr *)&got, got_len);
	close(t);
	close(s);
	snprintf(detail, sizeof detail, "bound=%d,named=%d,hex=%d,connected=%d", bound, named, hex, connected);
	record("unix_abstract_autobind", bound && named && hex && connected, detail);
}

static void check_netns_scope(void)
{
	char detail[64];
	struct sockaddr_un sun;
	socklen_t len = abstract_addr(&sun);
	int l = stream_socket();
	if (l < 0 || bind(l, (struct sockaddr *)&sun, len) || listen(l, 4)) {
		record("unix_abstract_netns_scope", 0, "listener");
		return;
	}
	pid_t pid = (pid_t)syscall(SYS_clone, CLONE_NEWUSER | CLONE_NEWNET | SIGCHLD, 0, 0, 0, 0);
	if (pid == 0) {
		if (connect_errno() != ECONNREFUSED)
			_exit(2);
		int own = stream_socket();
		_exit(bind(own, (struct sockaddr *)&sun, len) ? 3 : 0);
	}
	int code = pid < 0 ? -errno : wait_code(pid);
	int still_visible = connect_errno() == 0;
	close(l);
	snprintf(detail, sizeof detail, "stage=%d,still_visible=%d", code, still_visible);
	record("unix_abstract_netns_scope", code == 0 && still_visible, detail);
}

int main(void)
{
	alarm(30);
	check_abstract_bind();
	check_autobind();
	check_netns_scope();
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	return failed ? 1 : 0;
}
