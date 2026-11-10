#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define PORT 18181

static int finish(const char *detail, int ok)
{
	printf("MINI_CHROMIUM_TEST name=inherited_listener status=%s detail=%s\n", ok ? "PASS" : "FAIL", detail);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return ok ? 0 : 1;
}

int main(void)
{
	char detail[128];
	struct sockaddr_in sa = { .sin_family = AF_INET, .sin_port = htons(PORT) };
	sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	int ls = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (ls < 0 || bind(ls, (struct sockaddr *)&sa, sizeof sa) < 0 || listen(ls, 4) < 0) {
		snprintf(detail, sizeof detail, "listen-errno=%d", errno);
		return finish(detail, 0);
	}
	pid_t child = fork();
	if (child < 0) {
		snprintf(detail, sizeof detail, "fork-errno=%d", errno);
		return finish(detail, 0);
	}
	if (child == 0) {
		int c = accept4(ls, 0, 0, SOCK_CLOEXEC);
		_exit(c < 0 || write(c, "ok", 2) != 2 ? 1 : 0);
	}
	close(ls);
	int fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (fd < 0 || connect(fd, (struct sockaddr *)&sa, sizeof sa) < 0) {
		snprintf(detail, sizeof detail, "connect-errno=%d", errno);
		kill(child, SIGKILL);
		return finish(detail, 0);
	}
	struct pollfd p = { .fd = fd, .events = POLLIN };
	char reply[2] = "";
	int ready = poll(&p, 1, 5000);
	ssize_t got = ready == 1 ? read(fd, reply, sizeof reply) : -1;
	if (got != 2 || memcmp(reply, "ok", 2)) {
		snprintf(detail, sizeof detail, "poll=%d,read=%zd,errno=%d", ready, got, errno);
		kill(child, SIGKILL);
		return finish(detail, 0);
	}
	int status;
	if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status)) {
		snprintf(detail, sizeof detail, "child-status=0x%x", status);
		return finish(detail, 0);
	}
	return finish("child-accepted-after-parent-close", 1);
}
