#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <unistd.h>

enum { CHECK_DEADLINE_SECONDS = 40 };

#define _LINUX_CAPABILITY_VERSION_3 0x20080522

struct __user_cap_header_struct {
	uint32_t version;
	int pid;
};

struct __user_cap_data_struct {
	uint32_t effective;
	uint32_t permitted;
	uint32_t inheritable;
};

#ifndef CLONE_NEWUSER
#define CLONE_NEWUSER 0x10000000
#endif
#ifndef CLONE_NEWNS
#define CLONE_NEWNS 0x00020000
#endif
#ifndef CLONE_NEWUTS
#define CLONE_NEWUTS 0x04000000
#endif
#ifndef CLONE_NEWIPC
#define CLONE_NEWIPC 0x08000000
#endif
#ifndef CLONE_FS
#define CLONE_FS 0x00000200
#endif

#define CAP_SYS_CHROOT 18
#define CAP_FULL_LOW 0xffffffffu
#define CAP_FULL_HIGH 0x1ffu
#define USER_NS_OVERFLOW 65534

extern char **environ;

static int passed, failed;
static const char *volatile current_check = "";
static char self[4096];
static uid_t map_uid;
static gid_t map_gid;

static const uint64_t full_caps = (uint64_t)CAP_FULL_HIGH << 32 | CAP_FULL_LOW;

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
	while ((r = waitpid(pid, &status, 0)) < 0 && errno == EINTR)
		;
	return r == pid ? status : -1;
}

static int run_child(int (*fn)(void))
{
	pid_t pid = fork();
	if (pid < 0)
		return -1;
	if (pid == 0)
		_exit(fn());
	return wait_status(pid);
}

static int child_exit_code(int status)
{
	return WIFEXITED(status) ? WEXITSTATUS(status) : 1000 + status;
}

static ssize_t read_file(const char *path, char *buf, size_t len)
{
	int fd = open(path, O_RDONLY | O_CLOEXEC);
	if (fd < 0)
		return -errno;
	size_t got = 0;
	ssize_t n;
	while (got < len - 1 && (n = read(fd, buf + got, len - 1 - got)) > 0)
		got += (size_t)n;
	close(fd);
	buf[got] = 0;
	return (ssize_t)got;
}

static int write_file(const char *path, const char *text)
{
	int fd = open(path, O_WRONLY | O_CLOEXEC);
	if (fd < 0)
		return -errno;
	ssize_t n = write(fd, text, strlen(text));
	int err = n == (ssize_t)strlen(text) ? 0 : -errno;
	close(fd);
	return err;
}

static int status_field(const char *path, const char *field, char *out, size_t outlen)
{
	char text[4096];
	if (read_file(path, text, sizeof text) < 0)
		return -1;
	size_t flen = strlen(field);
	for (char *line = text; line && *line; line = strchr(line, '\n') ? strchr(line, '\n') + 1 : 0) {
		if (!strncmp(line, field, flen) && line[flen] == ':') {
			char *v = line + flen + 1;
			while (*v == ' ' || *v == '\t')
				v++;
			size_t len = strcspn(v, "\n");
			if (len >= outlen)
				len = outlen - 1;
			memcpy(out, v, len);
			out[len] = 0;
			return 0;
		}
	}
	return -1;
}

static int capget_v3(struct __user_cap_data_struct data[2])
{
	struct __user_cap_header_struct hdr = { _LINUX_CAPABILITY_VERSION_3, 0 };
	memset(data, 0, 2 * sizeof data[0]);
	return (int)syscall(SYS_capget, &hdr, data);
}

static int capset_v3(uint64_t eff, uint64_t prm, uint64_t inh)
{
	struct __user_cap_header_struct hdr = { _LINUX_CAPABILITY_VERSION_3, 0 };
	struct __user_cap_data_struct data[2] = {
		{ (uint32_t)eff, (uint32_t)prm, (uint32_t)inh },
		{ (uint32_t)(eff >> 32), (uint32_t)(prm >> 32), (uint32_t)(inh >> 32) },
	};
	return (int)syscall(SYS_capset, &hdr, data);
}

static int caps_are(uint64_t eff, uint64_t prm)
{
	struct __user_cap_data_struct d[2];
	if (capget_v3(d))
		return 0;
	uint64_t e = d[0].effective | (uint64_t)d[1].effective << 32;
	uint64_t p = d[0].permitted | (uint64_t)d[1].permitted << 32;
	return e == eff && p == prm;
}

static int have_cap(int cap)
{
	struct __user_cap_data_struct d[2];
	if (capget_v3(d))
		return 0;
	uint64_t e = d[0].effective | (uint64_t)d[1].effective << 32;
	return !!(e & (1ull << cap));
}

static int is_nanos(void)
{
	struct utsname u;
	return !uname(&u) && !strcmp(u.sysname, "Nanos");
}

static int map_is(const char *text, unsigned first, unsigned lower, unsigned count)
{
	unsigned a, b, c;
	return sscanf(text, "%u %u %u", &a, &b, &c) == 3 && a == first && b == lower && c == count;
}

static int write_identity_maps(uid_t uid, gid_t gid)
{
	char map[64];
	int r = write_file("/proc/self/setgroups", "deny\n");
	if (r)
		return r;
	snprintf(map, sizeof map, "%u %u 1\n", (unsigned)gid, (unsigned)gid);
	r = write_file("/proc/self/gid_map", map);
	if (r)
		return r;
	snprintf(map, sizeof map, "%u %u 1\n", (unsigned)uid, (unsigned)uid);
	return write_file("/proc/self/uid_map", map);
}

static int child_clone_userns(void)
{
	long pid = syscall(SYS_clone, CLONE_NEWUSER | SIGCHLD, 0, 0, 0, 0);
	if (pid < 0)
		return 2;
	if (pid == 0) {
		if (getuid() != USER_NS_OVERFLOW || geteuid() != USER_NS_OVERFLOW || getgid() != USER_NS_OVERFLOW ||
			getegid() != USER_NS_OVERFLOW)
			_exit(10);
		char v[128];
		if (status_field("/proc/self/status", "Uid", v, sizeof v) ||
			strcmp(v, "65534\t65534\t65534\t65534"))
			_exit(11);
		if (status_field("/proc/self/status", "Gid", v, sizeof v) ||
			strcmp(v, "65534\t65534\t65534\t65534"))
			_exit(12);
		if (!caps_are(full_caps, full_caps))
			_exit(13);
		if (write_identity_maps(map_uid, map_gid))
			_exit(14);
		if (getuid() != map_uid || geteuid() != map_uid || getgid() != map_gid || getegid() != map_gid)
			_exit(15);
		if (!caps_are(full_caps, full_caps))
			_exit(16);
		_exit(0);
	}
	return child_exit_code(wait_status((pid_t)pid));
}

static void check_clone_userns(void)
{
	map_uid = getuid();
	map_gid = getgid();
	int status = run_child(child_clone_userns);
	record("userns_clone_map", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int child_map_rules(void)
{
	uid_t uid = getuid();
	gid_t gid = getgid();
	char map[64], six[128];
	if (unshare(CLONE_NEWUSER))
		return 2;
	snprintf(map, sizeof map, "%u %u 1\n", (unsigned)gid, (unsigned)gid);
	int pre = write_file("/proc/self/gid_map", map);
	if (is_nanos() && pre != -EPERM)
		return 3;
	if (pre == 0) {
		if (write_file("/proc/self/setgroups", "deny\n") != -EPERM)
			return 4;
	} else if (pre == -EPERM) {
		if (write_file("/proc/self/setgroups", "deny\n"))
			return 5;
		if (write_file("/proc/self/gid_map", map))
			return 6;
	} else {
		return 7;
	}
	if (write_file("/proc/self/gid_map", map) != -EPERM)
		return 8;
	if (write_file("/proc/self/setgroups", "deny\n") != -EPERM)
		return 9;
	if (write_file("/proc/self/setgroups", "allow\n") != -EPERM)
		return 10;
	snprintf(map, sizeof map, "%u %u 1\n", (unsigned)uid, (unsigned)uid);
	if (write_file("/proc/self/uid_map", map))
		return 11;
	if (write_file("/proc/self/uid_map", map) != -EPERM)
		return 12;
	if (getuid() != uid || getgid() != gid)
		return 13;
	pid_t p = fork();
	if (p == 0) {
		if (unshare(CLONE_NEWUSER))
			_exit(20);
		if (write_file("/proc/self/uid_map", "1 2\n") != -EINVAL)
			_exit(21);
		if (write_file("/proc/self/uid_map", "0 0 0\n") != -EINVAL)
			_exit(22);
		if (write_file("/proc/self/uid_map", "0 0 1\n0 1 1\n0 2 1\n0 3 1\n0 4 1\n0 5 1\n") != -EINVAL)
			_exit(23);
		snprintf(map, sizeof map, "%u %u 1\n", (unsigned)uid, (unsigned)uid + 1);
		if (write_file("/proc/self/uid_map", map) != -EPERM)
			_exit(24);
		if (write_file("/proc/self/uid_map", "0 0 2\n1 2 2\n") != -EINVAL)
			_exit(25);
		snprintf(six, sizeof six, "%u %u 1\n", (unsigned)uid, (unsigned)uid);
		if (write_file("/proc/self/uid_map", six))
			_exit(26);
		if (getuid() != uid)
			_exit(27);
		_exit(0);
	}
	int code = child_exit_code(wait_status(p));
	return code ? 30 + code : 0;
}

static void check_map_rules(void)
{
	int status = run_child(child_map_rules);
	record("userns_map_rules", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int child_nested(void)
{
	uid_t uid = getuid();
	gid_t gid = getgid();
	if (unshare(CLONE_NEWUSER))
		return 2;
	if (write_identity_maps(uid, gid))
		return 3;
	if (!caps_are(full_caps, full_caps))
		return 4;
	if (capset_v3(0, 0, 0))
		return 5;
	if (!caps_are(0, 0))
		return 6;
	if (unshare(CLONE_NEWUSER))
		return 7;
	if (getuid() != USER_NS_OVERFLOW || getgid() != USER_NS_OVERFLOW)
		return 8;
	if (!caps_are(full_caps, full_caps))
		return 9;
	int r = write_identity_maps(uid, gid);
	if (is_nanos()) {
		if (r)
			return 10;
		if (getuid() != uid || getgid() != gid)
			return 11;
	} else if (r != 0 && r != -EPERM) {
		return 12;
	}
	return 0;
}

static void check_nested(void)
{
	int status = run_child(child_nested);
	record("userns_nested", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int child_caps_scope(void)
{
	if (capset_v3(0, 0, 0))
		return 2;
	if (!caps_are(0, 0))
		return 3;
	pid_t p = fork();
	if (p == 0) {
		if (unshare(CLONE_NEWUSER))
			_exit(10);
		if (!caps_are(full_caps, full_caps))
			_exit(11);
		if (getuid() != USER_NS_OVERFLOW)
			_exit(12);
		if (chroot("/"))
			_exit(13);
		_exit(0);
	}
	int code = child_exit_code(wait_status(p));
	if (code)
		return 20 + code;
	if (!caps_are(0, 0))
		return 4;
	return 0;
}

static void check_caps_scope(void)
{
	int status = run_child(child_caps_scope);
	record("userns_caps_scope", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int child_chroot_denied(void)
{
	if (capset_v3(0, 0, 0))
		return 2;
	errno = 0;
	if (chroot("/") != -1 || errno != EPERM)
		return 3;
	return 0;
}

static void check_chroot_denied(void)
{
	int status = run_child(child_chroot_denied);
	record("chroot_denied", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static ssize_t readlink_at_path(char *out, const char *path)
{
	char root[96];
	snprintf(root, sizeof root, "%s/root", path);
	ssize_t n = readlink(root, out, 95);
	if (n > 0)
		out[n] = 0;
	return n;
}

static int chroot_helper(int ready_fd, int go_fd)
{
	if (chroot("/proc/self/fdinfo/"))
		return 10;
	if (chdir("/"))
		return 11;
	struct stat a, b;
	if (stat("/", &a) || stat("/..", &b))
		return 12;
	if (a.st_dev != b.st_dev || a.st_ino != b.st_ino)
		return 13;
	if (stat("/../..", &b))
		return 14;
	if (a.st_dev != b.st_dev || a.st_ino != b.st_ino)
		return 15;
	if (write(ready_fd, "r", 1) != 1)
		return 16;
	char c;
	if (read(go_fd, &c, 1) != 1)
		return 17;
	return 0;
}

static int child_chroot_fs(void)
{
	if (!have_cap(CAP_SYS_CHROOT))
		return 1;
	int ready[2], go[2], obs[2], rr[2];
	if (pipe(ready) || pipe(go) || pipe(obs) || pipe(rr))
		return 2;
	pid_t observer = fork();
	if (observer == 0) {
		close(ready[0]);
		close(ready[1]);
		close(go[0]);
		close(go[1]);
		close(obs[1]);
		close(rr[0]);
		long helper;
		if (read(obs[0], &helper, sizeof helper) != sizeof helper)
			_exit(1);
		char rootpath[96] = "", base[64], want[96];
		snprintf(base, sizeof base, "/proc/%ld", helper);
		snprintf(want, sizeof want, "/proc/%ld/fdinfo", helper);
		ssize_t n = readlink_at_path(rootpath, base);
		struct stat rst;
		char ok = n > 0 && !strcmp(rootpath, want) && !stat(want, &rst) && S_ISDIR(rst.st_mode);
		_exit(write(rr[1], ok ? "1" : "0", 1) == 1 ? 0 : 2);
	}
	long helper = syscall(SYS_clone, CLONE_FS | SIGCHLD, 0, 0, 0, 0);
	if (helper == 0) {
		close(ready[0]);
		close(go[1]);
		close(obs[0]);
		close(obs[1]);
		close(rr[0]);
		close(rr[1]);
		_exit(chroot_helper(ready[1], go[0]));
	}
	if (helper < 0)
		return 3;
	close(ready[1]);
	close(go[0]);
	close(obs[0]);
	close(rr[1]);
	char c;
	if (read(ready[0], &c, 1) != 1)
		return 4;
	char cwd[64] = "";
	if (!getcwd(cwd, sizeof cwd) || strcmp(cwd, "/"))
		return 5;
	errno = 0;
	if (open("/etc/passwd", O_RDONLY | O_CLOEXEC) >= 0 || errno != ENOENT)
		return 6;
	errno = 0;
	if (open(self, O_RDONLY | O_CLOEXEC) >= 0 || errno != ENOENT)
		return 7;
	if (write(obs[1], &helper, sizeof helper) != sizeof helper)
		return 8;
	char root_ok = '0';
	if (read(rr[0], &root_ok, 1) != 1 || root_ok != '1')
		return 9;
	write(go[1], "x", 1);
	int status = wait_status((pid_t)helper);
	if (child_exit_code(status) != 0)
		return 10;
	status = wait_status(observer);
	if (child_exit_code(status) != 0)
		return 11;
	struct stat st;
	if (!stat("/proc", &st) && S_ISDIR(st.st_mode))
		return 12;
	close(ready[0]);
	close(obs[1]);
	close(rr[0]);
	close(go[1]);
	return 0;
}

static void check_chroot_fs(void)
{
	int status = run_child(child_chroot_fs);
	record("chroot_fs", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static volatile int park_released;

static void *park_thread(void *arg)
{
	(void)arg;
	while (!park_released)
		sched_yield();
	return 0;
}

static int child_unshare_threads(void)
{
	pthread_t t;
	if (pthread_create(&t, 0, park_thread, 0))
		return 2;
	errno = 0;
	int bad = unshare(CLONE_NEWUSER) == -1 && errno == EINVAL;
	park_released = 1;
	pthread_join(t, 0);
	if (!bad)
		return 3;
	int r = -1;
	for (int i = 0; i < 100; i++) {
		r = unshare(CLONE_NEWUSER);
		if (!r || errno != EINVAL)
			break;
		usleep(10000);
	}
	if (r)
		return 4;
	return 0;
}

static void check_unshare_threads(void)
{
	int status = run_child(child_unshare_threads);
	record("unshare_threads", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int child_unshare_flags(void)
{
	int nanos = is_nanos();
	errno = 0;
	int r = unshare(CLONE_NEWNS);
	if (nanos ? (r != -1 || errno != EINVAL) : (r != 0))
		return 2;
	errno = 0;
	r = unshare(CLONE_NEWUTS);
	if (nanos ? (r != -1 || errno != EINVAL) : (r != 0))
		return 3;
	errno = 0;
	r = unshare(CLONE_NEWIPC);
	if (nanos ? (r != -1 || errno != EINVAL) : (r != 0))
		return 4;
	if (nanos) {
		errno = 0;
		long c = syscall(SYS_clone, CLONE_NEWNS | SIGCHLD, 0, 0, 0, 0);
		if (c != -1 || errno != EINVAL)
			return 5;
	}
	errno = 0;
	long c = syscall(SYS_clone, CLONE_NEWUSER | CLONE_FS | SIGCHLD, 0, 0, 0, 0);
	if (c != -1 || errno != EINVAL)
		return 6;
	errno = 0;
	c = syscall(SYS_clone, CLONE_NEWUSER | CLONE_THREAD | SIGCHLD, 0, 0, 0, 0);
	if (c != -1 || errno != EINVAL)
		return 7;
	return 0;
}

static void check_unshare_flags(void)
{
	int status = run_child(child_unshare_flags);
	record("unshare_flags", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static const char *ns_names[] = { "user", "pid", "net", "ipc", "mnt", "uts" };

static int read_ns_links(char links[6][64])
{
	for (int i = 0; i < 6; i++) {
		char path[64];
		snprintf(path, sizeof path, "/proc/self/ns/%s", ns_names[i]);
		if (access(path, F_OK))
			return 1;
		ssize_t n = readlink(path, links[i], 63);
		if (n <= 0)
			return 2;
		links[i][n] = 0;
		if (strncmp(links[i], ns_names[i], strlen(ns_names[i])) ||
			links[i][strlen(ns_names[i])] != ':' || links[i][strlen(ns_names[i]) + 1] != '[')
			return 3;
	}
	return 0;
}

static int child_ns_links(void)
{
	char before[6][64], after[6][64];
	int r = read_ns_links(before);
	if (r)
		return 10 + r;
	if (unshare(CLONE_NEWUSER))
		return 20;
	r = read_ns_links(after);
	if (r)
		return 30 + r;
	if (!strcmp(before[0], after[0]))
		return 40;
	for (int i = 1; i < 6; i++)
		if (strcmp(before[i], after[i]))
			return 50 + i;
	return 0;
}

static void check_ns_links(void)
{
	int status = run_child(child_ns_links);
	record("proc_ns_links", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int child_id_maps(void)
{
	uid_t uid = getuid();
	gid_t gid = getgid();
	char text[512], want[128], v[128];
	if (unshare(CLONE_NEWUSER))
		return 2;
	if (getuid() != USER_NS_OVERFLOW || getgid() != USER_NS_OVERFLOW)
		return 3;
	if (read_file("/proc/self/uid_map", text, sizeof text) != 0 || text[0])
		return 4;
	if (read_file("/proc/self/gid_map", text, sizeof text) != 0 || text[0])
		return 5;
	if (read_file("/proc/self/setgroups", text, sizeof text) <= 0 ||
		(strncmp(text, "allow", 5) && strncmp(text, "deny", 4)))
		return 6;
	if (write_identity_maps(uid, gid))
		return 7;
	if (read_file("/proc/self/setgroups", text, sizeof text) <= 0 || strncmp(text, "deny", 4))
		return 12;
	if (read_file("/proc/self/uid_map", text, sizeof text) <= 0 || !map_is(text, uid, uid, 1))
		return 8;
	if (read_file("/proc/self/gid_map", text, sizeof text) <= 0 || !map_is(text, gid, gid, 1))
		return 9;
	snprintf(want, sizeof want, "%u\t%u\t%u\t%u", (unsigned)uid, (unsigned)uid, (unsigned)uid, (unsigned)uid);
	if (status_field("/proc/self/status", "Uid", v, sizeof v) || strcmp(v, want))
		return 10;
	snprintf(want, sizeof want, "%u\t%u\t%u\t%u", (unsigned)gid, (unsigned)gid, (unsigned)gid, (unsigned)gid);
	if (status_field("/proc/self/status", "Gid", v, sizeof v) || strcmp(v, want))
		return 11;
	return 0;
}

static void check_id_maps(void)
{
	int status = run_child(child_id_maps);
	record("proc_id_maps", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int role_no_caps(void)
{
	return caps_are(0, 0) && getuid() == 1000 ? 0 : 1;
}

static int child_exec_caps(void)
{
	uid_t uid = getuid();
	gid_t gid = getgid();
	char map[64];
	if (unshare(CLONE_NEWUSER))
		return 2;
	if (write_file("/proc/self/setgroups", "deny\n"))
		return 3;
	snprintf(map, sizeof map, "1000 %u 1\n", (unsigned)gid);
	if (write_file("/proc/self/gid_map", map))
		return 4;
	snprintf(map, sizeof map, "1000 %u 1\n", (unsigned)uid);
	if (write_file("/proc/self/uid_map", map))
		return 5;
	if (getuid() != 1000)
		return 6;
	char *argv[] = { self, "--role=no-caps", 0 };
	execve(self, argv, environ);
	return 7;
}

static void check_exec_caps(void)
{
	int status = run_child(child_exec_caps);
	record("exec_caps", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static void run_check(const char *name, void (*fn)(void))
{
	current_check = name;
	alarm(CHECK_DEADLINE_SECONDS);
	fn();
	alarm(0);
}

int main(int argc, char **argv)
{
	ssize_t n = readlink("/proc/self/exe", self, sizeof self - 1);
	if (n > 0)
		self[n] = 0;
	else
		snprintf(self, sizeof self, "%s", argv[0]);
	if (argc > 1 && !strcmp(argv[1], "--role=no-caps"))
		return role_no_caps();
	signal(SIGALRM, deadline);
	run_check("userns_clone_map", check_clone_userns);
	run_check("userns_map_rules", check_map_rules);
	run_check("userns_nested", check_nested);
	run_check("userns_caps_scope", check_caps_scope);
	run_check("chroot_denied", check_chroot_denied);
	run_check("chroot_fs", check_chroot_fs);
	run_check("unshare_threads", check_unshare_threads);
	run_check("unshare_flags", check_unshare_flags);
	run_check("proc_ns_links", check_ns_links);
	run_check("proc_id_maps", check_id_maps);
	run_check("exec_caps", check_exec_caps);
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n",
		failed ? "FAIL" : "PASS", passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
