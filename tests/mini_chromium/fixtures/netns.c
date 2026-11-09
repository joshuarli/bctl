#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef CLONE_NEWUSER
#define CLONE_NEWUSER 0x10000000
#endif
#ifndef CLONE_NEWNET
#define CLONE_NEWNET 0x40000000
#endif

static const char unix_path[] = "netns_fixture.sock";
static int passed, failed;

static void record(const char *name, int ok, const char *detail)
{
	printf("MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name, ok ? "PASS" : "FAIL", detail);
	fflush(stdout);
	ok ? passed++ : failed++;
}

static int is_nanos(void)
{
	struct utsname u;
	return !uname(&u) && !strcmp(u.sysname, "Nanos");
}

static int wait_code(pid_t pid)
{
	int status;
	pid_t r;
	while ((r = waitpid(pid, &status, 0)) < 0 && errno == EINTR)
		;
	if (r != pid)
		return -1;
	return WIFEXITED(status) ? WEXITSTATUS(status) : 200 + (WIFSIGNALED(status) ? WTERMSIG(status) : 0);
}

static pid_t clone_into(unsigned long flags)
{
	return (pid_t)syscall(SYS_clone, flags | SIGCHLD, 0, 0, 0, 0);
}

static int net_ns_link(char *out, size_t len)
{
	ssize_t n = readlink("/proc/self/ns/net", out, len - 1);
	if (n < 0)
		return -1;
	out[n] = 0;
	return 0;
}

static struct sockaddr_in inet4(const char *addr, int port)
{
	struct sockaddr_in sin = { .sin_family = AF_INET, .sin_port = htons(port) };
	inet_pton(AF_INET, addr, &sin.sin_addr);
	return sin;
}

static int connect_errno(int type, const char *addr)
{
	int s = socket(AF_INET, type | SOCK_CLOEXEC, 0);
	if (s < 0)
		return -1;
	struct sockaddr_in dst = inet4(addr, 80);
	errno = 0;
	int r = connect(s, (struct sockaddr *)&dst, sizeof dst) < 0 ? errno : 0;
	close(s);
	return r;
}

static int isolated_inet(void)
{
	if (connect_errno(SOCK_STREAM, "1.1.1.1") != ENETUNREACH)
		return 2;
	if (connect_errno(SOCK_STREAM, "127.0.0.1") != ENETUNREACH)
		return 3;
	int u = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	struct sockaddr_in dst = inet4("1.1.1.1", 53);
	errno = 0;
	if (u < 0 || sendto(u, "x", 1, 0, (struct sockaddr *)&dst, sizeof dst) != -1 || errno != ENETUNREACH)
		return 4;
	close(u);
	int l = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
	struct sockaddr_in any = inet4("0.0.0.0", 0);
	if (l < 0 || bind(l, (struct sockaddr *)&any, sizeof any) || listen(l, 4))
		return 5;
	errno = 0;
	if (accept(l, 0, 0) != -1 || errno != EAGAIN)
		return 6;
	struct sockaddr_in name;
	socklen_t len = sizeof name;
	if (getsockname(l, (struct sockaddr *)&name, &len) || len != sizeof name || name.sin_family != AF_INET ||
		name.sin_addr.s_addr != htonl(INADDR_ANY))
		return 7;
	close(l);
	int b = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	struct sockaddr_in lo = inet4("127.0.0.1", 0), bound;
	len = sizeof bound;
	if (b < 0 || bind(b, (struct sockaddr *)&lo, sizeof lo) || getsockname(b, (struct sockaddr *)&bound, &len) ||
		bound.sin_addr.s_addr != lo.sin_addr.s_addr || !bound.sin_port)
		return 8;
	errno = 0;
	if (bind(b, (struct sockaddr *)&lo, sizeof lo) != -1 || errno != EINVAL)
		return 9;
	close(b);
	int s6 = socket(AF_INET6, SOCK_STREAM | SOCK_CLOEXEC, 0);
	struct sockaddr_in6 lo6 = { .sin6_family = AF_INET6, .sin6_addr = IN6ADDR_LOOPBACK_INIT };
	errno = 0;
	if (s6 < 0 || bind(s6, (struct sockaddr *)&lo6, sizeof lo6) != -1 || errno != EADDRNOTAVAIL)
		return 12;
	close(s6);
	return 0;
}

static int child_zygote_netns(const char *parent_link)
{
	char link[64];
	if (net_ns_link(link, sizeof link) || !strcmp(link, parent_link))
		return 10;
	int r = isolated_inet();
	if (r)
		return r;
	errno = 0;
	int nl = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, 0);
	if (is_nanos() ? (nl != -1 || errno != EAFNOSUPPORT) : nl < 0)
		return 11;
	return 0;
}

static void check_zygote_netns(void)
{
	char parent_link[64], detail[64];
	if (net_ns_link(parent_link, sizeof parent_link)) {
		record("netns_isolated_inet", 0, "no-ns-link");
		return;
	}
	pid_t pid = clone_into(CLONE_NEWUSER | CLONE_NEWNET);
	if (pid == 0)
		_exit(child_zygote_netns(parent_link));
	int code = pid < 0 ? -errno : wait_code(pid);
	snprintf(detail, sizeof detail, "stage=%d", code);
	record("netns_isolated_inet", code == 0, detail);
}

static int child_existing_sockets(int tcp_client, struct sockaddr_in *server)
{
	if (connect(tcp_client, (struct sockaddr *)server, sizeof *server))
		return 2;
	if (write(tcp_client, "n", 1) != 1)
		return 3;
	int u = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	struct sockaddr_un sun = { .sun_family = AF_UNIX };
	strcpy(sun.sun_path, unix_path);
	if (u < 0 || connect(u, (struct sockaddr *)&sun, sizeof sun))
		return 4;
	if (write(u, "u", 1) != 1)
		return 5;
	return 0;
}

static void check_existing_sockets(void)
{
	char detail[96];
	int listener = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	struct sockaddr_in server = inet4("127.0.0.1", 0);
	socklen_t len = sizeof server;
	if (listener < 0 || bind(listener, (struct sockaddr *)&server, sizeof server) || listen(listener, 4) ||
		getsockname(listener, (struct sockaddr *)&server, &len)) {
		record("netns_existing_sockets", 0, "tcp-listener");
		return;
	}
	unlink(unix_path);
	int ulistener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	struct sockaddr_un sun = { .sun_family = AF_UNIX };
	strcpy(sun.sun_path, unix_path);
	if (ulistener < 0 || bind(ulistener, (struct sockaddr *)&sun, sizeof sun) || listen(ulistener, 4)) {
		record("netns_existing_sockets", 0, "unix-listener");
		return;
	}
	int client = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	pid_t pid = clone_into(CLONE_NEWUSER | CLONE_NEWNET);
	if (pid == 0)
		_exit(child_existing_sockets(client, &server));
	char c = 0, uc = 0;
	int tcp_peer = accept(listener, 0, 0);
	int tcp_ok = tcp_peer >= 0 && read(tcp_peer, &c, 1) == 1 && c == 'n';
	int unix_peer = accept(ulistener, 0, 0);
	int unix_ok = unix_peer >= 0 && read(unix_peer, &uc, 1) == 1 && uc == 'u';
	int code = wait_code(pid);
	snprintf(detail, sizeof detail, "tcp=%d,unix=%d,code=%d", tcp_ok, unix_ok, code);
	record("netns_existing_sockets", tcp_ok && unix_ok && code == 0, detail);
	close(tcp_peer);
	close(unix_peer);
	close(client);
	close(listener);
	close(ulistener);
	unlink(unix_path);
}

static int child_unshare_netns(void)
{
	char before[64], after[64];
	if (net_ns_link(before, sizeof before))
		return 2;
	if (unshare(CLONE_NEWNET))
		return 3;
	if (net_ns_link(after, sizeof after) || !strcmp(before, after))
		return 4;
	return connect_errno(SOCK_STREAM, "127.0.0.1") == ENETUNREACH ? 0 : 5;
}

static void check_unshare_netns(void)
{
	char detail[64];
	pid_t pid = clone_into(CLONE_NEWUSER);
	if (pid == 0)
		_exit(child_unshare_netns());
	int code = pid < 0 ? -errno : wait_code(pid);
	snprintf(detail, sizeof detail, "stage=%d", code);
	record("netns_unshare", code == 0, detail);
}

int main(void)
{
	alarm(60);
	check_zygote_netns();
	check_existing_sockets();
	check_unshare_netns();
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	return failed ? 1 : 0;
}
