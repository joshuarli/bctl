#define _GNU_SOURCE
#include <arpa/inet.h>
#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <limits.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define FIXTURE_SUM 17575
#define FIXTURE_MARKER "g5-fixture-ok"
#define MAX_PROCS 256
#define WS_MAX_MESSAGE (8u << 20)

static const char *const TEST_NAMES[] = {
	"fixture_server", "helium_browser_up", "cdp_version", "cdp_navigate", "cdp_evaluate", "cdp_dom",
	"helium_zygote", "helium_renderer_sandboxed", "sandbox_confinement", "clean_exit",
};
#define NTESTS (sizeof TEST_NAMES / sizeof TEST_NAMES[0])

static struct {
	const char *root;
	const char *profile;
	const char *logdir;
	int cdp_port;
	int fixture_port;
	int timeout_s;
	uid_t uid;
	gid_t gid;
} opt = {
	.root = "",
	.profile = "/profile",
	.logdir = "/tmp",
	.cdp_port = 9222,
	.fixture_port = 18080,
	.timeout_s = 240,
	.uid = 1000,
	.gid = 1000,
};

static int done[NTESTS];
static int passed;
static int failed;
static pid_t fixture_pid = -1;
static pid_t helium_pid = -1;

static void emit(const char *line, size_t len)
{
	while (len > 0) {
		ssize_t n = write(1, line, len);
		if (n < 0 && errno == EINTR)
			continue;
		if (n <= 0)
			_exit(111);
		line += n;
		len -= (size_t)n;
	}
}

static void note(const char *fmt, ...)
{
	char line[1024];
	va_list ap;
	va_start(ap, fmt);
	int len = vsnprintf(line, sizeof line, fmt, ap);
	va_end(ap);
	if (len > (int)sizeof line - 2)
		len = sizeof line - 2;
	line[len++] = '\n';
	emit(line, (size_t)len);
}

static void record(const char *name, int ok, const char *fmt, ...)
{
	char detail[512] = "";
	char line[768];
	if (fmt) {
		va_list ap;
		va_start(ap, fmt);
		vsnprintf(detail, sizeof detail, fmt, ap);
		va_end(ap);
	}
	for (char *c = detail; *c; c++)
		if (*c == ' ' || *c == '\n' || *c == '\t')
			*c = ',';
	for (size_t i = 0; i < NTESTS; i++)
		if (!strcmp(TEST_NAMES[i], name)) {
			if (done[i])
				return;
			done[i] = 1;
		}
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s%s%s\n", name, ok ? "PASS" : "FAIL",
		detail[0] ? " detail=" : "", detail);
	emit(line, (size_t)len);
	if (ok)
		passed++;
	else
		failed++;
}

static long now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec * 1000L + ts.tv_nsec / 1000000L;
}

static void sleep_ms(long ms)
{
	struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
	while (nanosleep(&ts, &ts) < 0 && errno == EINTR)
		;
}

static void join_path(char *out, size_t n, const char *rel)
{
	snprintf(out, n, "%s%s", opt.root, rel);
}

static int mkdir_p(const char *path, mode_t mode)
{
	char buf[PATH_MAX];
	snprintf(buf, sizeof buf, "%s", path);
	for (char *p = buf + 1; *p; p++) {
		if (*p != '/')
			continue;
		*p = 0;
		if (mkdir(buf, 0755) < 0 && errno != EEXIST)
			return -1;
		*p = '/';
	}
	if (mkdir(buf, mode) < 0 && errno != EEXIST)
		return -1;
	return 0;
}

static ssize_t read_file(const char *path, char *buf, size_t n)
{
	int fd = open(path, O_RDONLY | O_CLOEXEC);
	if (fd < 0)
		return -1;
	size_t used = 0;
	for (;;) {
		ssize_t r = read(fd, buf + used, n - 1 - used);
		if (r < 0 && errno == EINTR)
			continue;
		if (r <= 0)
			break;
		used += (size_t)r;
		if (used == n - 1)
			break;
	}
	close(fd);
	buf[used] = 0;
	return (ssize_t)used;
}

static int wait_readable(int fd, long deadline)
{
	for (;;) {
		long left = deadline - now_ms();
		if (left < 0)
			left = 0;
		struct pollfd p = { .fd = fd, .events = POLLIN };
		int r = poll(&p, 1, (int)left);
		if (r < 0 && errno == EINTR)
			continue;
		if (r == 0)
			return -1;
		if (r < 0)
			return -1;
		return 0;
	}
}

static int read_exact(int fd, void *buf, size_t n, long deadline)
{
	char *p = buf;
	while (n > 0) {
		if (wait_readable(fd, deadline) < 0)
			return -1;
		ssize_t r = read(fd, p, n);
		if (r < 0 && errno == EINTR)
			continue;
		if (r <= 0)
			return -1;
		p += r;
		n -= (size_t)r;
	}
	return 0;
}

static int write_all(int fd, const void *buf, size_t n)
{
	const char *p = buf;
	while (n > 0) {
		ssize_t w = write(fd, p, n);
		if (w < 0 && errno == EINTR)
			continue;
		if (w <= 0)
			return -1;
		p += w;
		n -= (size_t)w;
	}
	return 0;
}

static int tcp_connect(int port)
{
	int fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (fd < 0)
		return -1;
	struct sockaddr_in sa = { .sin_family = AF_INET, .sin_port = htons((uint16_t)port) };
	sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	if (connect(fd, (struct sockaddr *)&sa, sizeof sa) < 0) {
		close(fd);
		return -1;
	}
	return fd;
}

static const char *json_find(const char *json, const char *key)
{
	char pat[96];
	snprintf(pat, sizeof pat, "\"%s\":", key);
	const char *p = strstr(json, pat);
	if (!p)
		return NULL;
	p += strlen(pat);
	while (*p == ' ')
		p++;
	return p;
}

static int json_string(const char *json, const char *key, char *out, size_t n)
{
	const char *p = json_find(json, key);
	if (!p || *p != '"')
		return -1;
	p++;
	size_t i = 0;
	while (*p && *p != '"') {
		if (*p == '\\' && p[1])
			p++;
		if (i + 1 >= n)
			return -1;
		out[i++] = *p++;
	}
	out[i] = 0;
	return 0;
}

static int json_long(const char *json, const char *key, long *out)
{
	const char *p = json_find(json, key);
	char *end;
	if (!p)
		return -1;
	*out = strtol(p, &end, 10);
	return end == p ? -1 : 0;
}

static void json_escape(char *out, size_t n, const char *in)
{
	size_t i = 0;
	for (; *in && i + 2 < n; in++) {
		if (*in == '"' || *in == '\\')
			out[i++] = '\\';
		out[i++] = *in;
	}
	out[i] = 0;
}

static char *http_request(const char *method, const char *path, int *status)
{
	int fd = tcp_connect(opt.cdp_port);
	if (fd < 0)
		return NULL;
	char req[1024];
	int len = snprintf(req, sizeof req,
		"%s %s HTTP/1.1\r\nHost: 127.0.0.1:%d\r\nConnection: close\r\nContent-Length: 0\r\n\r\n", method, path,
		opt.cdp_port);
	if (write_all(fd, req, (size_t)len) < 0) {
		close(fd);
		return NULL;
	}
	size_t cap = 65536, used = 0;
	char *buf = malloc(cap);
	long deadline = now_ms() + 20000;
	for (;;) {
		if (used + 1 >= cap) {
			cap *= 2;
			buf = realloc(buf, cap);
		}
		if (wait_readable(fd, deadline) < 0)
			break;
		ssize_t r = read(fd, buf + used, cap - 1 - used);
		if (r < 0 && errno == EINTR)
			continue;
		if (r <= 0)
			break;
		used += (size_t)r;
	}
	close(fd);
	buf[used] = 0;
	if (used < 12 || strncmp(buf, "HTTP/1.1 ", 9)) {
		free(buf);
		return NULL;
	}
	*status = atoi(buf + 9);
	char *body = strstr(buf, "\r\n\r\n");
	if (!body) {
		free(buf);
		return NULL;
	}
	char *out = strdup(body + 4);
	free(buf);
	return out;
}

struct ws {
	int fd;
	int next_id;
};

static int ws_send_frame(struct ws *w, int opcode, const char *data, size_t n)
{
	size_t cap = n + 16;
	unsigned char *f = malloc(cap);
	size_t h = 0;
	f[h++] = (unsigned char)(0x80 | opcode);
	if (n < 126) {
		f[h++] = (unsigned char)(0x80 | n);
	} else if (n < 65536) {
		f[h++] = 0x80 | 126;
		f[h++] = (unsigned char)(n >> 8);
		f[h++] = (unsigned char)n;
	} else {
		f[h++] = 0x80 | 127;
		for (int i = 7; i >= 0; i--)
			f[h++] = (unsigned char)((uint64_t)n >> (8 * i));
	}
	unsigned char mask[4] = { 0x37, 0xfa, 0x21, 0x3d };
	memcpy(f + h, mask, 4);
	h += 4;
	for (size_t i = 0; i < n; i++)
		f[h + i] = (unsigned char)data[i] ^ mask[i & 3];
	int r = write_all(w->fd, f, h + n);
	free(f);
	return r;
}

static char *ws_recv(struct ws *w, long deadline)
{
	char *msg = NULL;
	size_t used = 0;
	for (;;) {
		unsigned char h[2];
		if (read_exact(w->fd, h, 2, deadline) < 0)
			goto fail;
		int opcode = h[0] & 0x0f, fin = h[0] >> 7;
		uint64_t len = h[1] & 0x7f;
		if (h[1] & 0x80)
			goto fail;
		if (len == 126) {
			unsigned char e[2];
			if (read_exact(w->fd, e, 2, deadline) < 0)
				goto fail;
			len = (uint64_t)e[0] << 8 | e[1];
		} else if (len == 127) {
			unsigned char e[8];
			if (read_exact(w->fd, e, 8, deadline) < 0)
				goto fail;
			len = 0;
			for (int i = 0; i < 8; i++)
				len = len << 8 | e[i];
		}
		if (len > WS_MAX_MESSAGE || used + len > WS_MAX_MESSAGE)
			goto fail;
		char *payload = malloc(len + 1);
		if (read_exact(w->fd, payload, len, deadline) < 0) {
			free(payload);
			goto fail;
		}
		if (opcode == 8) {
			free(payload);
			goto fail;
		}
		if (opcode == 9) {
			ws_send_frame(w, 10, payload, len);
			free(payload);
			continue;
		}
		if (opcode == 10) {
			free(payload);
			continue;
		}
		msg = realloc(msg, used + len + 1);
		memcpy(msg + used, payload, len);
		used += len;
		msg[used] = 0;
		free(payload);
		if (fin)
			return msg;
	}
fail:
	free(msg);
	return NULL;
}

static int ws_open(struct ws *w, const char *url)
{
	const char *p = strstr(url, "ws://");
	if (!p)
		return -1;
	p = strchr(p + 5, '/');
	if (!p)
		return -1;
	w->fd = tcp_connect(opt.cdp_port);
	w->next_id = 0;
	if (w->fd < 0)
		return -1;
	char req[1024];
	int len = snprintf(req, sizeof req,
		"GET %s HTTP/1.1\r\nHost: 127.0.0.1:%d\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
		"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
		p, opt.cdp_port);
	if (write_all(w->fd, req, (size_t)len) < 0)
		goto fail;
	char resp[2048];
	size_t used = 0;
	long deadline = now_ms() + 15000;
	while (used < sizeof resp - 1) {
		if (read_exact(w->fd, resp + used, 1, deadline) < 0)
			goto fail;
		used++;
		resp[used] = 0;
		if (used >= 4 && !memcmp(resp + used - 4, "\r\n\r\n", 4))
			break;
	}
	if (strncmp(resp, "HTTP/1.1 101", 12))
		goto fail;
	return 0;
fail:
	close(w->fd);
	w->fd = -1;
	return -1;
}

static void ws_close(struct ws *w)
{
	if (w->fd >= 0)
		close(w->fd);
	w->fd = -1;
}

static char *cdp_call(struct ws *w, const char *method, const char *params, long timeout_ms)
{
	char msg[8192];
	int id = ++w->next_id;
	int len = snprintf(msg, sizeof msg, "{\"id\":%d,\"method\":\"%s\",\"params\":%s}", id, method, params ? params : "{}");
	if (ws_send_frame(w, 1, msg, (size_t)len) < 0)
		return NULL;
	long deadline = now_ms() + timeout_ms;
	for (;;) {
		char *r = ws_recv(w, deadline);
		if (!r)
			return NULL;
		long got;
		if (!strncmp(r, "{\"id\":", 6) && json_long(r, "id", &got) == 0 && got == id)
			return r;
		free(r);
	}
}

static int cdp_ok(const char *resp)
{
	return resp && !strstr(resp, "\"error\":{");
}

static int cdp_eval_string(struct ws *w, const char *expr, char *out, size_t n)
{
	char esc[2048], params[4096];
	json_escape(esc, sizeof esc, expr);
	snprintf(params, sizeof params, "{\"expression\":\"%s\",\"returnByValue\":true}", esc);
	char *r = cdp_call(w, "Runtime.evaluate", params, 30000);
	if (!cdp_ok(r) || strstr(r, "\"exceptionDetails\"")) {
		free(r);
		return -1;
	}
	const char *res = strstr(r, "\"result\":{\"result\":");
	int rc = -1;
	if (res) {
		const char *v = json_find(res, "value");
		if (v) {
			size_t i = 0;
			if (*v == '"') {
				v++;
				while (*v && *v != '"' && i + 1 < n) {
					if (*v == '\\' && v[1])
						v++;
					out[i++] = *v++;
				}
			} else {
				while (*v && *v != ',' && *v != '}' && i + 1 < n)
					out[i++] = *v++;
			}
			out[i] = 0;
			rc = 0;
		}
	}
	free(r);
	return rc;
}

static const char FIXTURE_PAGE[] =
	"<!doctype html><html><head><meta charset=\"utf-8\"><title>g5 fixture</title></head><body>"
	"<div id=\"out\">pending</div>"
	"<script>var s=0;for(var i=1;i<=37;i++){s+=i*i;}window.__sum=s;"
	"var o=document.getElementById('out');o.setAttribute('data-marker','" FIXTURE_MARKER "');"
	"o.textContent='sum-squares='+s;</script></body></html>";

static void fixture_serve(int ls)
{
	signal(SIGPIPE, SIG_IGN);
	for (;;) {
		int c = accept4(ls, NULL, NULL, SOCK_CLOEXEC);
		if (c < 0) {
			if (errno == EINTR)
				continue;
			_exit(3);
		}
		char req[4096];
		size_t used = 0;
		long deadline = now_ms() + 5000;
		while (used < sizeof req - 1) {
			if (wait_readable(c, deadline) < 0)
				break;
			ssize_t r = read(c, req + used, sizeof req - 1 - used);
			if (r <= 0)
				break;
			used += (size_t)r;
			req[used] = 0;
			if (strstr(req, "\r\n\r\n"))
				break;
		}
		char head[256];
		int hl;
		if (!strncmp(req, "GET / ", 6)) {
			hl = snprintf(head, sizeof head,
				"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n",
				sizeof FIXTURE_PAGE - 1);
			write_all(c, head, (size_t)hl);
			write_all(c, FIXTURE_PAGE, sizeof FIXTURE_PAGE - 1);
		} else {
			hl = snprintf(head, sizeof head, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
			write_all(c, head, (size_t)hl);
		}
		close(c);
	}
}

static int open_log(const char *name)
{
	char path[PATH_MAX];
	snprintf(path, sizeof path, "%s/%s", opt.logdir, name);
	int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
	return fd;
}

static pid_t spawn(const char *log, char *const argv[], const char *const env[], const char *cwd)
{
	int logfd = open_log(log);
	pid_t pid = fork();
	if (pid < 0)
		return -1;
	if (pid > 0) {
		if (logfd >= 0)
			close(logfd);
		return pid;
	}
	int null = open("/dev/null", O_RDONLY);
	if (null >= 0)
		dup2(null, 0);
	if (logfd >= 0) {
		dup2(logfd, 1);
		dup2(logfd, 2);
	}
	if (getuid() == 0) {
		if (setgroups(0, NULL) < 0 || setgid(opt.gid) < 0 || setuid(opt.uid) < 0)
			_exit(125);
	}
	if (cwd && chdir(cwd) < 0)
		_exit(124);
	clearenv();
	putenv("PATH=/usr/bin:/bin");
	for (; *env; env++)
		putenv((char *)*env);
	execv(argv[0], argv);
	_exit(127);
}

static int child_alive(pid_t pid, int *status)
{
	int st;
	pid_t r = waitpid(pid, &st, WNOHANG);
	if (r == 0)
		return 1;
	if (status)
		*status = r == pid ? st : -1;
	return 0;
}

struct proc {
	int pid;
	char role[32];
	int nnp;
	int seccomp;
	long uid;
	int nspid_depth;
	char nspid[64];
};

static int scan_procs(struct proc *out, int max)
{
	DIR *d = opendir("/proc");
	if (!d)
		return -1;
	char needle[PATH_MAX + 32];
	snprintf(needle, sizeof needle, "--user-data-dir=%s", opt.profile);
	int n = 0;
	struct dirent *e;
	while ((e = readdir(d)) && n < max) {
		char *end;
		long pid = strtol(e->d_name, &end, 10);
		if (*end || pid <= 0)
			continue;
		char path[64], cmd[8192];
		snprintf(path, sizeof path, "/proc/%ld/cmdline", pid);
		ssize_t len = read_file(path, cmd, sizeof cmd);
		if (len <= 0)
			continue;
		for (ssize_t i = 0; i < len - 1; i++)
			if (cmd[i] == 0)
				cmd[i] = 0x20;
		int match = strstr(cmd, needle) != NULL;
		char role_buf[32];
		const char *role = "browser";
		const char *type = strstr(cmd, "--type=");
		if (type) {
			size_t k = strcspn(type + 7, " ");
			snprintf(role_buf, sizeof role_buf, "%.*s", (int)k, type + 7);
			role = role_buf;
		}
		char exe[PATH_MAX];
		snprintf(exe, sizeof exe, "%.*s", (int)strcspn(cmd, " "), cmd);
		const char *base = strrchr(exe, 0x2f);
		base = base ? base + 1 : exe;
		if (!strcmp(base, "helium_crashpad_handler"))
			role = "crashpad";
		if (!strcmp(role, "crashpad"))
			match = strstr(cmd, opt.profile) != NULL;
		if (!match)
			continue;
		struct proc *p = &out[n];
		memset(p, 0, sizeof *p);
		p->pid = (int)pid;
		p->nnp = p->seccomp = -1;
		p->uid = -1;
		snprintf(p->role, sizeof p->role, "%s", role);
		snprintf(path, sizeof path, "/proc/%ld/status", pid);
		char st[8192];
		if (read_file(path, st, sizeof st) < 0)
			continue;
		for (char *line = st; line && *line;) {
			char *nl = strchr(line, '\n');
			if (nl)
				*nl = 0;
			if (!strncmp(line, "NoNewPrivs:", 11))
				p->nnp = atoi(line + 11);
			else if (!strncmp(line, "Seccomp:", 8))
				p->seccomp = atoi(line + 8);
			else if (!strncmp(line, "Uid:", 4))
				p->uid = strtol(line + 4, NULL, 10);
			else if (!strncmp(line, "NSpid:", 6)) {
				const char *v = line + 6;
				while (*v == ' ' || *v == '\t')
					v++;
				snprintf(p->nspid, sizeof p->nspid, "%s", v);
				for (char *c = p->nspid; *c; c++)
					if (*c == '\t' || *c == ' ')
						*c = '/';
				p->nspid_depth = 1;
				for (const char *c = p->nspid; *c; c++)
					if (*c == '/')
						p->nspid_depth++;
			}
			line = nl ? nl + 1 : NULL;
		}
		n++;
	}
	closedir(d);
	return n;
}

static void kill_profile_procs(int sig)
{
	struct proc ps[MAX_PROCS];
	int n = scan_procs(ps, MAX_PROCS);
	for (int i = 0; i < n; i++)
		kill(ps[i].pid, sig);
}

static void reap_children(long timeout_ms, int *remaining)
{
	long deadline = now_ms() + timeout_ms;
	for (;;) {
		int st;
		pid_t r = waitpid(-1, &st, WNOHANG);
		if (r > 0) {
			if (r == fixture_pid)
				fixture_pid = -1;
			if (r == helium_pid)
				helium_pid = -1;
			continue;
		}
		if (r < 0 && errno == ECHILD) {
			*remaining = 0;
			return;
		}
		if (now_ms() >= deadline) {
			*remaining = 1;
			return;
		}
		sleep_ms(50);
	}
}

static int arg_str(const char *arg, const char *key, const char **out)
{
	size_t n = strlen(key);
	if (strncmp(arg, key, n) || arg[n] != '=')
		return 0;
	*out = arg + n + 1;
	return 1;
}

static void parse_args(int argc, char **argv)
{
	for (int i = 1; i < argc; i++) {
		const char *v;
		if (arg_str(argv[i], "--root", &v)) {
			opt.root = !strcmp(v, "/") ? "" : v;
		} else if (arg_str(argv[i], "--profile", &v)) {
			opt.profile = v;
		} else if (arg_str(argv[i], "--logdir", &v)) {
			opt.logdir = v;
		} else if (arg_str(argv[i], "--cdp-port", &v)) {
			opt.cdp_port = atoi(v);
		} else if (arg_str(argv[i], "--fixture-port", &v)) {
			opt.fixture_port = atoi(v);
		} else if (arg_str(argv[i], "--timeout", &v)) {
			opt.timeout_s = atoi(v);
		} else if (arg_str(argv[i], "--uid", &v)) {
			opt.uid = (uid_t)atoi(v);
			opt.gid = (gid_t)atoi(v);
		} else {
			note("guest_init: unknown argument %s", argv[i]);
			exit(2);
		}
	}
}

static int prepare_dirs(void)
{
	const char *dirs[] = { opt.profile, opt.logdir };
	for (size_t i = 0; i < 2; i++) {
		if (mkdir_p(dirs[i], 0700) < 0) {
			note("guest_init: cannot create %s: %s", dirs[i], strerror(errno));
			return -1;
		}
		if (getuid() == 0 && chown(dirs[i], opt.uid, opt.gid) < 0) {
			note("guest_init: cannot chown %s: %s", dirs[i], strerror(errno));
			return -1;
		}
	}
	return 0;
}

static int start_fixture(char *error, size_t n)
{
	int ls = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	int one = 1;
	struct sockaddr_in sa = { .sin_family = AF_INET, .sin_port = htons((uint16_t)opt.fixture_port) };
	sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	if (ls < 0 || setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one) < 0 ||
	    bind(ls, (struct sockaddr *)&sa, sizeof sa) < 0 || listen(ls, 16) < 0) {
		snprintf(error, n, "%s", strerror(errno));
		if (ls >= 0)
			close(ls);
		return -1;
	}
	fixture_pid = fork();
	if (fixture_pid < 0) {
		snprintf(error, n, "fork:%s", strerror(errno));
		close(ls);
		return -1;
	}
	if (fixture_pid == 0) {
		if (getuid() == 0 && (setgroups(0, NULL) < 0 || setgid(opt.gid) < 0 || setuid(opt.uid) < 0))
			_exit(125);
		fixture_serve(ls);
		_exit(0);
	}
	close(ls);
	return 0;
}

static int start_helium(void)
{
	static char helium[PATH_MAX], user_data[PATH_MAX + 32], port_arg[64];
	static char e_lib[PATH_MAX * 2 + 32], e_home[PATH_MAX + 16];
	static char e_fc[PATH_MAX + 32], e_fcp[PATH_MAX + 32];
	join_path(helium, sizeof helium, "/opt/helium/helium");
	snprintf(user_data, sizeof user_data, "--user-data-dir=%s", opt.profile);
	snprintf(port_arg, sizeof port_arg, "--remote-debugging-port=%d", opt.cdp_port);
	snprintf(e_lib, sizeof e_lib, "LD_LIBRARY_PATH=%s/opt/helium/lib", opt.root);
	snprintf(e_home, sizeof e_home, "HOME=%s", opt.profile);
	snprintf(e_fc, sizeof e_fc, "FONTCONFIG_FILE=%s/opt/helium/fontconfig/fonts.conf", opt.root);
	snprintf(e_fcp, sizeof e_fcp, "FONTCONFIG_PATH=%s/opt/helium/fontconfig", opt.root);
	char *const argv[] = { helium, "--headless=new", port_arg, user_data, "--no-first-run",
		"--no-default-browser-check", "--disable-gpu", "--enable-logging=stderr", "about:blank", NULL };
	const char *const env[] = { e_lib, e_home, e_fc, e_fcp, "GTK_USE_PORTAL=0",
		"NO_AT_BRIDGE=1", NULL };
	helium_pid = spawn("helium.log", argv, env, opt.profile);
	return helium_pid < 0 ? -1 : 0;
}

static int helium_status = -1;

static void emit_log_tail(const char *name, off_t max)
{
	char path[PATH_MAX], buf[4096];
	snprintf(path, sizeof path, "%s/%s", opt.logdir, name);
	int fd = open(path, O_RDONLY | O_CLOEXEC);
	if (fd < 0)
		return;
	off_t size = lseek(fd, 0, SEEK_END);
	lseek(fd, size > max ? size - max : 0, SEEK_SET);
	emit("GUEST_LOG_BEGIN ", 16);
	emit(name, strlen(name));
	emit("\n", 1);
	ssize_t n;
	while ((n = read(fd, buf, sizeof buf)) > 0)
		emit(buf, (size_t)n);
	emit("\nGUEST_LOG_END\n", 15);
	close(fd);
}

static int wait_cdp_port(long timeout_ms)
{
	long deadline = now_ms() + timeout_ms;
	while (now_ms() < deadline) {
		int fd = tcp_connect(opt.cdp_port);
		if (fd >= 0) {
			close(fd);
			return 0;
		}
		if (!child_alive(helium_pid, &helium_status)) {
			helium_pid = -1;
			return -2;
		}
		sleep_ms(200);
	}
	return -1;
}

static int wait_page_ready(struct ws *page, long timeout_ms, char *last, size_t last_len)
{
	char want[64], got[256];
	snprintf(want, sizeof want, "complete|http://127.0.0.1:%d/", opt.fixture_port);
	snprintf(last, last_len, "no-answer");
	long deadline = now_ms() + timeout_ms;
	while (now_ms() < deadline) {
		if (cdp_eval_string(page, "document.readyState+'|'+location.href", got, sizeof got) == 0) {
			snprintf(last, last_len, "%s", got);
			if (!strcmp(got, want))
				return 0;
		}
		sleep_ms(200);
	}
	return -1;
}

static void print_procs(const struct proc *ps, int n)
{
	for (int i = 0; i < n; i++)
		note("HELIUM_PROC pid=%d role=%s NoNewPrivs=%d Seccomp=%d Uid=%ld NSpid=%s", ps[i].pid, ps[i].role, ps[i].nnp,
			ps[i].seccomp, ps[i].uid, ps[i].nspid);
}

static void check_zygotes(const struct proc *ps, int n)
{
	int zygotes = 0, nnp = 0, namespaced = 0;
	for (int i = 0; i < n; i++) {
		if (strcmp(ps[i].role, "zygote"))
			continue;
		zygotes++;
		if (ps[i].nnp == 1)
			nnp++;
		if (ps[i].nspid_depth >= 2)
			namespaced++;
	}
	record("helium_zygote", zygotes >= 1 && nnp == zygotes && namespaced >= 1,
		"zygotes=%d,nnp=%d,in_pid_namespace=%d", zygotes, nnp, namespaced);
}

static void check_renderers(const struct proc *ps, int n)
{
	int renderers = 0, good = 0;
	for (int i = 0; i < n; i++) {
		if (strcmp(ps[i].role, "renderer"))
			continue;
		renderers++;
		if (ps[i].nnp == 1 && ps[i].seccomp == 2 && ps[i].uid > 0 && ps[i].nspid_depth >= 2)
			good++;
	}
	record("helium_renderer_sandboxed", renderers >= 1 && good == renderers,
		"renderers=%d,nnp1_seccomp2_nonroot_pidns=%d", renderers, good);
}

static ssize_t link_text(int pid, const char *what, char *out, size_t n)
{
	char path[96];
	snprintf(path, sizeof path, "/proc/%d/%s", pid, what);
	ssize_t r = readlink(path, out, n - 1);
	if (r >= 0)
		out[r] = 0;
	return r;
}

static int renderer_confined_externally(const struct proc *p, char *why, size_t n)
{
	static const char *const spaces[] = { "ns/user", "ns/pid", "ns/net" };
	char mine[128], theirs[128];
	for (size_t i = 0; i < 3; i++) {
		if (link_text(getpid(), spaces[i], mine, sizeof mine) < 0 || link_text(p->pid, spaces[i], theirs, sizeof theirs) < 0) {
			snprintf(why, n, "pid=%d,cannot-read-%s", p->pid, spaces[i]);
			return 0;
		}
		if (!strcmp(mine, theirs)) {
			snprintf(why, n, "pid=%d,shares-%s", p->pid, spaces[i]);
			return 0;
		}
	}
	if (link_text(p->pid, "root", theirs, sizeof theirs) < 0 || strncmp(theirs, "/proc/", 6) ||
	    strcmp(theirs + strlen(theirs) - 7, "/fdinfo")) {
		snprintf(why, n, "pid=%d,root=%s", p->pid, theirs);
		return 0;
	}
	return 1;
}

static int renderer_count(void)
{
	struct proc ps[MAX_PROCS];
	int n = scan_procs(ps, MAX_PROCS), c = 0;
	for (int i = 0; i < n; i++)
		if (!strcmp(ps[i].role, "renderer"))
			c++;
	return c;
}

static void check_confinement(struct ws *browser, struct ws *page)
{
	struct proc ps[MAX_PROCS];
	int n = scan_procs(ps, MAX_PROCS);
	char why[160] = "";
	int renderers = 0, confined = 0;
	for (int i = 0; i < n; i++) {
		if (strcmp(ps[i].role, "renderer"))
			continue;
		renderers++;
		if (renderer_confined_externally(&ps[i], why, sizeof why))
			confined++;
	}
	if (renderers < 1 || confined != renderers) {
		record("sandbox_confinement", 0, "stage=namespaces,renderers=%d,confined=%d,%s", renderers, confined, why);
		return;
	}

	char *r = cdp_call(browser, "Target.setDiscoverTargets", "{\"discover\":true}", 15000);
	int ok = cdp_ok(r);
	free(r);
	if (!ok) {
		record("sandbox_confinement", 0, "stage=discover");
		return;
	}
	int before = renderer_count();
	r = cdp_call(browser, "Target.createTarget", "{\"url\":\"chrome://crash\"}", 30000);
	ok = cdp_ok(r);
	free(r);
	if (!ok) {
		record("sandbox_confinement", 0, "stage=create-crash-target");
		return;
	}
	int crashed = 0;
	long deadline = now_ms() + 60000;
	while (!crashed) {
		char *ev = ws_recv(browser, deadline);
		if (!ev)
			break;
		if (strstr(ev, "\"method\":\"Target.targetCrashed\""))
			crashed = 1;
		free(ev);
	}
	char val[64] = "";
	int page_ok = cdp_eval_string(page, "String(window.__sum)", val, sizeof val) == 0 && atol(val) == FIXTURE_SUM;
	int alive = child_alive(helium_pid, NULL);
	int after_settled = 0;
	for (int i = 0; i < 100 && !after_settled; i++) {
		after_settled = renderer_count() <= before;
		if (!after_settled)
			sleep_ms(100);
	}
	record("sandbox_confinement", crashed && page_ok && alive && after_settled,
		"renderers=%d,own_namespaces_and_fdinfo_root=%d,crash_target_reported=%d,sibling_page_ok=%d,browser_alive=%d,dead_renderer_gone=%d",
		renderers, confined, crashed, page_ok, alive, after_settled);
}

static int run_cdp(void)
{
	int status = 0;
	char *ver = http_request("GET", "/json/version", &status);
	char browser_ver[128] = "", user_agent[256] = "", browser_url[256] = "";
	int ok = ver && status == 200 && json_string(ver, "Browser", browser_ver, sizeof browser_ver) == 0 &&
		json_string(ver, "User-Agent", user_agent, sizeof user_agent) == 0 &&
		json_string(ver, "webSocketDebuggerUrl", browser_url, sizeof browser_url) == 0 &&
		!strcmp(browser_ver, "Helium/0.20.3.1") && strstr(user_agent, "Chrome/156.");
	record("cdp_version", ok, "browser=%s", ok ? browser_ver : "unparsed");
	free(ver);
	if (!ok)
		return -1;

	char *tgt = http_request("PUT", "/json/new?about:blank", &status);
	char page_url[256] = "";
	if (!tgt || status != 200 || json_string(tgt, "webSocketDebuggerUrl", page_url, sizeof page_url) < 0) {
		free(tgt);
		record("cdp_navigate", 0, "stage=new-target,http=%d", status);
		return -1;
	}
	free(tgt);

	struct ws page = { -1, 0 }, browser = { -1, 0 };
	if (ws_open(&page, page_url) < 0 || ws_open(&browser, browser_url) < 0) {
		record("cdp_navigate", 0, "stage=websocket");
		return -1;
	}

	char params[256];
	snprintf(params, sizeof params, "{\"url\":\"http://127.0.0.1:%d/\"}", opt.fixture_port);
	long nav_start = now_ms();
	char *nav = cdp_call(&page, "Page.navigate", params, 30000);
	long nav_ms = now_ms() - nav_start;
	int sent = cdp_ok(nav) && !strstr(nav, "\"errorText\"");
	char nav_reply[200] = "none";
	if (nav)
		snprintf(nav_reply, sizeof nav_reply, "%s", nav);
	free(nav);
	char last[256] = "not-waited";
	int nav_ok = sent && wait_page_ready(&page, 60000, last, sizeof last) == 0;
	record("cdp_navigate", nav_ok, "url=http://127.0.0.1:%d/,navigate_ok=%d,navigate_ms=%ld,last_state=%s,reply=%s",
		opt.fixture_port, sent, nav_ms, last, nav_reply);
	if (!nav_ok) {
		emit_log_tail("helium.log", 8192);
		ws_close(&page);
		ws_close(&browser);
		return -1;
	}

	char val[64] = "";
	int eval_ok = cdp_eval_string(&page, "String(window.__sum)", val, sizeof val) == 0 && atol(val) == FIXTURE_SUM;
	record("cdp_evaluate", eval_ok, "value=%s,expected=%d", val, FIXTURE_SUM);

	int dom_ok = 0;
	char outer_detail[96] = "stage=getDocument";
	char *doc = cdp_call(&page, "DOM.getDocument", "{\"depth\":1}", 30000);
	long root = 0;
	if (cdp_ok(doc) && strstr(doc, "\"root\":{") && json_long(strstr(doc, "\"root\":{"), "nodeId", &root) == 0) {
		char q[128];
		snprintf(q, sizeof q, "{\"nodeId\":%ld,\"selector\":\"#out\"}", root);
		char *sel = cdp_call(&page, "DOM.querySelector", q, 30000);
		long node = 0;
		if (cdp_ok(sel) && json_long(sel, "nodeId", &node) == 0 && node > 0) {
			snprintf(q, sizeof q, "{\"nodeId\":%ld}", node);
			char *html = cdp_call(&page, "DOM.getOuterHTML", q, 30000);
			char want[64];
			snprintf(want, sizeof want, "sum-squares=%d", FIXTURE_SUM);
			dom_ok = cdp_ok(html) && strstr(html, FIXTURE_MARKER) && strstr(html, want);
			snprintf(outer_detail, sizeof outer_detail, "marker=%s,text=%s", dom_ok ? "found" : "missing", want);
			free(html);
		} else {
			snprintf(outer_detail, sizeof outer_detail, "stage=querySelector");
		}
		free(sel);
	}
	free(doc);
	record("cdp_dom", dom_ok, "%s", outer_detail);

	struct proc ps[MAX_PROCS];
	int n = scan_procs(ps, MAX_PROCS);
	print_procs(ps, n);
	check_zygotes(ps, n);
	check_renderers(ps, n);
	check_confinement(&browser, &page);

	char *cl = cdp_call(&browser, "Browser.close", "{}", 15000);
	free(cl);
	ws_close(&page);
	ws_close(&browser);
	return 0;
}

static void reap_available(void)
{
	int st;
	pid_t r;
	while ((r = waitpid(-1, &st, WNOHANG)) > 0) {
		if (r == fixture_pid)
			fixture_pid = -1;
	}
}

static void stop_all_and_check(int closed_by_cdp)
{
	int helium_status = -1, helium_exited = 0;
	long deadline = now_ms() + (closed_by_cdp ? 30000 : 0);
	while (helium_pid > 0) {
		int st;
		pid_t r = waitpid(helium_pid, &st, WNOHANG);
		if (r == helium_pid) {
			helium_exited = WIFEXITED(st);
			helium_status = helium_exited ? WEXITSTATUS(st) : 128 + WTERMSIG(st);
			helium_pid = -1;
			break;
		}
		if (now_ms() >= deadline)
			break;
		sleep_ms(100);
	}
	int forced = 0;
	if (helium_pid > 0) {
		kill(helium_pid, SIGKILL);
		forced = 1;
	}

	struct proc ps[MAX_PROCS];
	int survivors = 0;
	long grace = now_ms() + 15000;
	for (;;) {
		reap_available();
		survivors = scan_procs(ps, MAX_PROCS);
		if (survivors == 0 || now_ms() >= grace)
			break;
		sleep_ms(100);
	}
	if (survivors > 0) {
		kill_profile_procs(SIGKILL);
		sleep_ms(300);
	}

	if (fixture_pid > 0)
		kill(fixture_pid, SIGTERM);
	int children_left = 0;
	reap_children(5000, &children_left);
	if (children_left) {
		if (fixture_pid > 0)
			kill(fixture_pid, SIGKILL);
		reap_children(5000, &children_left);
	}
	record("clean_exit", closed_by_cdp && helium_exited && helium_status == 0 && !forced && survivors == 0 && !children_left,
		"helium_exit=%d,survivors_after_close=%d,children_left=%d", helium_status, survivors, children_left);
}


static void on_alarm(int sig)
{
	(void)sig;
	static const char msg[] = "guest_init: overall timeout\n";
	(void)!write(2, msg, sizeof msg - 1);
	kill_profile_procs(SIGKILL);
	if (fixture_pid > 0)
		kill(fixture_pid, SIGKILL);
	_exit(124);
}

int main(int argc, char **argv)
{
	parse_args(argc, argv);
	setvbuf(stdout, NULL, _IONBF, 0);
	if (getpid() != 1)
		prctl(PR_SET_CHILD_SUBREAPER, 1);
	signal(SIGPIPE, SIG_IGN);
	signal(SIGALRM, on_alarm);
	alarm((unsigned)opt.timeout_s);

	if (prepare_dirs() < 0)
		return 2;

	int closed = 0;

	char fixture_error[96] = "none";
	int f = start_fixture(fixture_error, sizeof fixture_error);
	record("fixture_server", f == 0, "port=%d,error=%s", opt.fixture_port, fixture_error);
	if (f != 0)
		goto finish;

	if (start_helium() < 0) {
		record("helium_browser_up", 0, "stage=spawn");
		goto finish;
	}
	int c = wait_cdp_port(120000);
	record("helium_browser_up", c == 0, "pid=%d,port=%d%s,status=0x%x", (int)helium_pid, opt.cdp_port,
		c == -2 ? ",browser-exited" : "", helium_status);
	if (c != 0) {
		emit_log_tail("helium.log", 8192);
		goto finish;
	}

	closed = run_cdp() == 0;

finish:
	stop_all_and_check(closed);
	for (size_t i = 0; i < NTESTS; i++)
		if (!done[i])
			record(TEST_NAMES[i], 0, "not-run");
	char line[128];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	emit(line, (size_t)len);
	return failed ? 1 : 0;
}
