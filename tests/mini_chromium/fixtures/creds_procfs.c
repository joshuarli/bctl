#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum { CHECK_DEADLINE_SECONDS = 20, DROP_UID = 1000, DROP_GID = 1000 };

#define _LINUX_CAPABILITY_VERSION_1 0x19980330
#define _LINUX_CAPABILITY_VERSION_2 0x20071026
#define _LINUX_CAPABILITY_VERSION_3 0x20080522
#define CAP_SETUID 7

struct __user_cap_header_struct {
	uint32_t version;
	int pid;
};

struct __user_cap_data_struct {
	uint32_t effective;
	uint32_t permitted;
	uint32_t inheritable;
};

#define PR_SET_PTRACER_FLAG 0x59616d61
#define PR_SET_PTRACER_ANY_FLAG ((unsigned long)-1)
#define CAP_FULL_LOW 0xffffffffu
#define CAP_FULL_HIGH 0x1ffu

extern char **environ;

static int passed, failed;
static const char *volatile current_check = "";
static char self[4096];

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

static int self_field_is(const char *field, const char *want)
{
	char v[256];
	return !status_field("/proc/self/status", field, v, sizeof v) && !strcmp(v, want);
}

static int capget_v3(pid_t pid, struct __user_cap_data_struct data[2])
{
	struct __user_cap_header_struct hdr = { _LINUX_CAPABILITY_VERSION_3, pid };
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
	if (capget_v3(0, d))
		return 0;
	uint64_t e = d[0].effective | (uint64_t)d[1].effective << 32;
	uint64_t p = d[0].permitted | (uint64_t)d[1].permitted << 32;
	return e == eff && p == prm;
}

static int resuid_is(uid_t r, uid_t e, uid_t s)
{
	uid_t gr, ge, gs;
	return !getresuid(&gr, &ge, &gs) && gr == r && ge == e && gs == s;
}

static const uint64_t full_caps = (uint64_t)CAP_FULL_HIGH << 32 | CAP_FULL_LOW;

static int drop_to_unprivileged(void)
{
	if (geteuid() != 0)
		return 0;
	return setgroups(0, 0) || setgid(DROP_GID) || setuid(DROP_UID) ? -1 : 0;
}

static int child_root_drop(void)
{
	if (!caps_are(full_caps, full_caps) || !self_field_is("CapEff", "000001ffffffffff") ||
		!self_field_is("CapBnd", "000001ffffffffff"))
		return 2;
	gid_t groups[2] = { 20, 10 };
	if (setgroups(2, groups) || getgroups(0, 0) != 2 || !self_field_is("Groups", "10 20 "))
		return 3;
	if (setgroups(0, 0) || setgid(DROP_GID) || setuid(DROP_UID))
		return 4;
	gid_t gr, ge, gs;
	if (!resuid_is(DROP_UID, DROP_UID, DROP_UID) || getresgid(&gr, &ge, &gs) || gr != DROP_GID || ge != DROP_GID ||
		gs != DROP_GID)
		return 5;
	if (!caps_are(0, 0) || !self_field_is("CapEff", "0000000000000000") || !self_field_is("CapPrm", "0000000000000000"))
		return 6;
	if (!self_field_is("Uid", "1000\t1000\t1000\t1000") || !self_field_is("Gid", "1000\t1000\t1000\t1000"))
		return 7;
	return 0;
}

static void check_root_drop(void)
{
	int status = run_child(child_root_drop);
	record("root_drop", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int child_setresuid(void)
{
	if (setresuid(DROP_UID, DROP_UID + 1, DROP_UID + 2) || !resuid_is(DROP_UID, DROP_UID + 1, DROP_UID + 2))
		return 2;
	if (!caps_are(0, 0))
		return 3;
	if (setresuid(DROP_UID + 2, DROP_UID, DROP_UID + 1) || !resuid_is(DROP_UID + 2, DROP_UID, DROP_UID + 1))
		return 4;
	errno = 0;
	if (setresuid(-1, DROP_UID + 3, -1) != -1 || errno != EPERM)
		return 5;
	if (setreuid(DROP_UID, -1) || !resuid_is(DROP_UID, DROP_UID, DROP_UID))
		return 6;
	errno = 0;
	if (setuid(DROP_UID + 1) != -1 || errno != EPERM)
		return 7;
	return 0;
}

static int child_euid_caps(void)
{
	if (setresuid(DROP_UID, 0, 0) || !caps_are(full_caps, full_caps))
		return 2;
	if (seteuid(DROP_UID) || !caps_are(0, full_caps))
		return 3;
	if (seteuid(0) || !caps_are(full_caps, full_caps))
		return 4;
	if (setuid(DROP_UID) || !resuid_is(DROP_UID, DROP_UID, DROP_UID) || !caps_are(0, 0))
		return 5;
	return 0;
}

static int child_capset_drop(void)
{
	uint64_t without_setuid = full_caps & ~(1ull << CAP_SETUID);
	if (capset_v3(without_setuid, without_setuid, 0) || !caps_are(without_setuid, without_setuid))
		return 2;
	errno = 0;
	if (setuid(DROP_UID) != -1 || errno != EPERM)
		return 3;
	errno = 0;
	if (capset_v3(full_caps, full_caps, 0) != -1 || errno != EPERM)
		return 4;
	return 0;
}

static void check_root_transitions(void)
{
	int a = child_exit_code(run_child(child_setresuid));
	int b = child_exit_code(run_child(child_euid_caps));
	int c = child_exit_code(run_child(child_capset_drop));
	record("setresuid_permutations", a == 0 && b == 0, "setresuid=%d euid_caps=%d", a, b);
	record("capset_drop", c == 0, "stage=%d", c);
}

static int child_unprivileged(void)
{
	if (drop_to_unprivileged() || geteuid() == 0)
		return 2;
	uid_t uid = getuid();
	errno = 0;
	if (setuid(0) != -1 || errno != EPERM || getuid() != uid)
		return 3;
	errno = 0;
	if (setresuid(0, -1, -1) != -1 || errno != EPERM)
		return 4;
	if (setresuid(-1, uid, -1))
		return 5;
	gid_t g = 1;
	errno = 0;
	if (setgroups(1, &g) != -1 || errno != EPERM)
		return 6;
	errno = 0;
	if (capset_v3(1ull << CAP_SETUID, 1ull << CAP_SETUID, 0) != -1 || errno != EPERM)
		return 7;
	if (capset_v3(0, 0, 0))
		return 8;
	if (!self_field_is("CapEff", "0000000000000000"))
		return 9;
	return 0;
}

static void check_unprivileged(void)
{
	int status = run_child(child_unprivileged);
	record("unprivileged_denied", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static void check_capget_versions(void)
{
	struct __user_cap_header_struct hdr = { _LINUX_CAPABILITY_VERSION_1, 0 };
	struct __user_cap_data_struct d[2];
	memset(d, 0x5a, sizeof d);
	int v1 = syscall(SYS_capget, &hdr, d) == 0 && d[1].effective == 0x5a5a5a5a;
	hdr.version = _LINUX_CAPABILITY_VERSION_2;
	int v2 = syscall(SYS_capget, &hdr, d) == 0 && d[1].effective != 0x5a5a5a5a;
	hdr.version = 0x12345678;
	errno = 0;
	int bad = syscall(SYS_capget, &hdr, d) == -1 && errno == EINVAL && hdr.version == _LINUX_CAPABILITY_VERSION_3;
	hdr.version = 0x12345678;
	int probe = syscall(SYS_capget, &hdr, 0) == 0 && hdr.version == _LINUX_CAPABILITY_VERSION_3;
	hdr.version = 0x12345678;
	errno = 0;
	int bad_set = syscall(SYS_capset, &hdr, d) == -1 && errno == EINVAL && hdr.version == _LINUX_CAPABILITY_VERSION_3;
	int hold[2];
	pipe(hold);
	pid_t other = fork();
	if (other == 0) {
		char c;
		read(hold[0], &c, 1);
		_exit(0);
	}
	struct __user_cap_data_struct mine[2], theirs[2];
	int other_get = capget_v3(0, mine) == 0 && capget_v3(other, theirs) == 0 && !memcmp(mine, theirs, sizeof mine);
	hdr.version = _LINUX_CAPABILITY_VERSION_3;
	hdr.pid = other;
	errno = 0;
	int other_set = syscall(SYS_capset, &hdr, mine) == -1 && errno == EPERM;
	write(hold[1], "x", 1);
	wait_status(other);
	close(hold[0]);
	close(hold[1]);
	errno = 0;
	int esrch = capget_v3(0x3ffffff0, theirs) == -1 && errno == ESRCH;
	record("capget_versions", v1 && v2 && bad && probe && bad_set && other_get && esrch && other_set,
		"v1=%d v2=%d bad=%d probe=%d bad_set=%d other_get=%d esrch=%d other_set=%d", v1, v2, bad, probe, bad_set,
		other_get, esrch, other_set);
}

static int child_nnp(void)
{
	if (prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 0 || !self_field_is("NoNewPrivs", "0"))
		return 2;
	errno = 0;
	if (prctl(PR_SET_NO_NEW_PRIVS, 1, 1, 0, 0) != -1 || errno != EINVAL)
		return 3;
	if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) || prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1)
		return 4;
	errno = 0;
	if (prctl(PR_SET_NO_NEW_PRIVS, 0, 0, 0, 0) != -1 || errno != EINVAL || prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1)
		return 5;
	errno = 0;
	if (prctl(PR_GET_NO_NEW_PRIVS, 1, 0, 0, 0) != -1 || errno != EINVAL)
		return 6;
	if (!self_field_is("NoNewPrivs", "1"))
		return 7;
	pid_t pid = fork();
	if (pid == 0)
		_exit(prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1 ? 0 : 1);
	if (child_exit_code(wait_status(pid)))
		return 8;
	pid = fork();
	if (pid == 0) {
		char *argv[] = { self, "--role=nnp", 0 };
		execve(self, argv, environ);
		_exit(100);
	}
	int code = child_exit_code(wait_status(pid));
	return code ? 10 + code : 0;
}

static void check_nnp(void)
{
	int status = run_child(child_nnp);
	record("no_new_privs", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int child_prctl(void)
{
	if (prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) != 1 || prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) ||
		prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) != 0)
		return 2;
	errno = 0;
	if (prctl(PR_SET_DUMPABLE, 2, 0, 0, 0) != -1 || errno != EINVAL || prctl(PR_SET_DUMPABLE, 1, 0, 0, 0))
		return 3;
	if (prctl(PR_SET_PTRACER_FLAG, 0, 0, 0, 0) || prctl(PR_SET_PTRACER_FLAG, PR_SET_PTRACER_ANY_FLAG, 0, 0, 0) ||
		prctl(PR_SET_PTRACER_FLAG, getppid(), 0, 0, 0))
		return 4;
	errno = 0;
	if (prctl(PR_SET_PTRACER_FLAG, 0x3ffffff0, 0, 0, 0) != -1 || errno != EINVAL)
		return 5;
	errno = 0;
	if (prctl(0x7fff1234, 0, 0, 0, 0) != -1 || errno != EINVAL)
		return 6;
	int sig = -1;
	if (prctl(PR_SET_PDEATHSIG, SIGUSR1, 0, 0, 0) || prctl(PR_GET_PDEATHSIG, &sig, 0, 0, 0) || sig != SIGUSR1)
		return 7;
	errno = 0;
	if (prctl(PR_SET_PDEATHSIG, 99, 0, 0, 0) != -1 || errno != EINVAL)
		return 8;
	if (prctl(PR_CAPBSET_READ, CAP_SETUID, 0, 0, 0) != 1)
		return 9;
	errno = 0;
	if (prctl(PR_CAPBSET_READ, 64, 0, 0, 0) != -1 || errno != EINVAL)
		return 10;
	char name[16] = "";
	if (prctl(PR_SET_NAME, "cp-prctl", 0, 0, 0) || prctl(PR_GET_NAME, name, 0, 0, 0) || strcmp(name, "cp-prctl") ||
		!self_field_is("Name", "cp-prctl"))
		return 11;
	return 0;
}

static void check_prctl(void)
{
	int status = run_child(child_prctl);
	record("prctl_table", child_exit_code(status) == 0, "stage=%d", child_exit_code(status));
}

static int dir_has(const char *path, const char *name)
{
	DIR *d = opendir(path);
	if (!d)
		return -1;
	int found = 0;
	struct dirent *e;
	while ((e = readdir(d)))
		if (!strcmp(e->d_name, name))
			found = 1;
	closedir(d);
	return found;
}

static void check_proc_status(void)
{
	char want[64], v[256];
	prctl(PR_SET_NAME, "cp-main", 0, 0, 0);
	int name = self_field_is("Name", "cp-main");
	snprintf(want, sizeof want, "%d", getpid());
	int pid = self_field_is("Pid", want) && self_field_is("Tgid", want) && self_field_is("NSpid", want);
	snprintf(want, sizeof want, "%d", getppid());
	int ppid = self_field_is("PPid", want);
	snprintf(want, sizeof want, "%u\t%u\t%u\t%u", getuid(), geteuid(), geteuid(), geteuid());
	int uid = self_field_is("Uid", want);
	int state = !status_field("/proc/self/status", "State", v, sizeof v) && v[0] == 'R';
	int misc = self_field_is("Threads", "1") && self_field_is("NoNewPrivs", "0") && self_field_is("Seccomp", "0") &&
		!status_field("/proc/self/status", "CapAmb", v, sizeof v) && strlen(v) == 16;
	char link[64] = "";
	ssize_t n = readlink("/proc/self", link, sizeof link - 1);
	snprintf(want, sizeof want, "%d", getpid());
	int self_link = n > 0 && !strcmp(link, want);
	char exe[4096] = "";
	n = readlink("/proc/self/exe", exe, sizeof exe - 1);
	int exe_ok = n > 0 && exe[0] == '/' && strstr(exe, "creds_procfs");
	for (int i = 0; exe_ok && i < 256; i++) {
		struct stat st;
		exe_ok = !stat("/proc/self/exe", &st) && S_ISREG(st.st_mode);
	}

	int ready[2], hold[2];
	pipe(ready);
	pipe(hold);
	pid_t child = fork();
	if (child == 0) {
		prctl(PR_SET_NAME, "cp-child", 0, 0, 0);
		prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
		write(ready[1], "r", 1);
		char c;
		read(hold[0], &c, 1);
		_exit(0);
	}
	char c;
	read(ready[0], &c, 1);
	char path[64];
	snprintf(path, sizeof path, "/proc/%d/status", child);
	int cname = !status_field(path, "Name", v, sizeof v) && !strcmp(v, "cp-child");
	snprintf(want, sizeof want, "%d", getpid());
	int cppid = !status_field(path, "PPid", v, sizeof v) && !strcmp(v, want);
	int cnnp = !status_field(path, "NoNewPrivs", v, sizeof v) && !strcmp(v, "1");
	char mine[4096], theirs[4096];
	snprintf(path, sizeof path, "/proc/%d/cmdline", child);
	ssize_t ml = read_file("/proc/self/cmdline", mine, sizeof mine);
	ssize_t tl = read_file(path, theirs, sizeof theirs);
	int cmdline = ml > 0 && ml == tl && !memcmp(mine, theirs, (size_t)ml);
	snprintf(want, sizeof want, "%d", child);
	int listed = dir_has("/proc", want) == 1 && dir_has("/proc", "self") == 1;
	write(hold[1], "x", 1);
	wait_status(child);
	snprintf(path, sizeof path, "/proc/%d/status", child);
	int gone = open(path, O_RDONLY) < 0 && errno == ENOENT && dir_has("/proc", want) == 0;
	close(ready[0]);
	close(ready[1]);
	close(hold[0]);
	close(hold[1]);
	record("proc_status_self", name && pid && ppid && uid && state && misc && self_link && exe_ok,
		"name=%d pid=%d ppid=%d uid=%d state=%d misc=%d self=%d exe=%d", name, pid, ppid, uid, state, misc, self_link,
		exe_ok);
	record("proc_status_child", cname && cppid && cnnp && cmdline, "name=%d ppid=%d nnp=%d cmdline=%d", cname, cppid,
		cnnp, cmdline);
	record("proc_readdir_live", listed && gone, "listed=%d gone=%d", listed, gone);
}

static volatile int park_released;

static void *parked(void *arg)
{
	(void)arg;
	while (!park_released) {
		struct timespec ts = { 0, 1000 * 1000 };
		nanosleep(&ts, 0);
	}
	return 0;
}

static unsigned long task_nlink(void)
{
	struct stat st;
	return stat("/proc/self/task/", &st) ? 0 : (unsigned long)st.st_nlink;
}

static int task_entries(void)
{
	DIR *d = opendir("/proc/self/task");
	if (!d)
		return -1;
	int n = 0;
	struct dirent *e;
	while ((e = readdir(d)))
		if (e->d_name[0] != '.')
			n++;
	closedir(d);
	return n;
}

static void check_task_nlink(void)
{
	unsigned long one = task_nlink();
	int proc_fd = open("/proc", O_DIRECTORY | O_RDONLY | O_CLOEXEC);
	struct stat st;
	int at_ok = proc_fd >= 0 && !fstatat(proc_fd, "self/task/", &st, 0) && st.st_nlink == 3;
	pthread_t t[2];
	for (int i = 0; i < 2; i++)
		pthread_create(&t[i], 0, parked, 0);
	unsigned long three = task_nlink();
	int entries = task_entries();
	int threads = self_field_is("Threads", "3");
	park_released = 1;
	for (int i = 0; i < 2; i++)
		pthread_join(t[i], 0);
	unsigned long after = 0;
	for (int i = 0; i < 200 && (after = task_nlink()) != 3; i++) {
		struct timespec ts = { 0, 5 * 1000 * 1000 };
		nanosleep(&ts, 0);
	}
	if (proc_fd >= 0)
		close(proc_fd);
	record("task_nlink", one == 3 && at_ok && three == 5 && entries == 3 && threads && after == 3,
		"one=%lu at=%d three=%lu entries=%d threads=%d after=%lu", one, at_ok, three, entries, threads, after);
}

static void check_fd_listing(void)
{
	int dfd = open("/", O_DIRECTORY | O_RDONLY | O_CLOEXEC);
	int p[2];
	pipe(p);
	int fd_dir = open("/proc/self/fd", O_DIRECTORY | O_RDONLY | O_CLOEXEC);
	char name[16];
	struct stat st;
	snprintf(name, sizeof name, "%d", dfd);
	int dir_follow = !fstatat(fd_dir, name, &st, 0) && S_ISDIR(st.st_mode);
	int dir_link = !fstatat(fd_dir, name, &st, AT_SYMLINK_NOFOLLOW) && S_ISLNK(st.st_mode);
	snprintf(name, sizeof name, "%d", p[0]);
	int pipe_follow = !fstatat(fd_dir, name, &st, 0) && !S_ISDIR(st.st_mode);
	int listed = 0, types = 1;
	DIR *d = fdopendir(dup(fd_dir));
	struct dirent *e;
	while (d && (e = readdir(d))) {
		if (e->d_name[0] == '.')
			continue;
		int n = atoi(e->d_name);
		if (n == dfd || n == p[0] || n == p[1])
			listed++;
		if (e->d_type != DT_LNK)
			types = 0;
	}
	if (d)
		closedir(d);
	char path[64], link[128] = "";
	snprintf(path, sizeof path, "/proc/self/fd/%d", p[0]);
	ssize_t n = readlink(path, link, sizeof link - 1);
	int pipe_name = n > 0 && !strncmp(link, "pipe:[", 6);
	snprintf(path, sizeof path, "/proc/self/fd/%d", dfd);
	memset(link, 0, sizeof link);
	n = readlink(path, link, sizeof link - 1);
	int dir_name = n == 1 && link[0] == '/';
	char info[256];
	snprintf(path, sizeof path, "/proc/self/fdinfo/%d", dfd);
	char flags[64] = "";
	int fdinfo = !status_field(path, "flags", flags, sizeof flags) && (strtoul(flags, 0, 8) & O_CLOEXEC) &&
		read_file(path, info, sizeof info) > 0 && !strncmp(info, "pos:\t0\n", 7);
	close(fd_dir);
	close(dfd);
	close(p[0]);
	close(p[1]);
	record("fd_listing", dir_follow && dir_link && pipe_follow && listed == 3 && types && pipe_name && dir_name && fdinfo,
		"dir=%d link=%d pipe=%d listed=%d types=%d pipe_name=%d dir_name=%d fdinfo=%d", dir_follow, dir_link, pipe_follow,
		listed, types, pipe_name, dir_name, fdinfo);
}

static void check_memfd_reopen(void)
{
	int fd = memfd_create("cp-reopen", MFD_CLOEXEC);
	if (fd < 0 || ftruncate(fd, 4096) || pwrite(fd, "hello", 5, 0) != 5) {
		record("memfd_reopen", 0, "setup errno=%d", errno);
		return;
	}
	char path[64];
	snprintf(path, sizeof path, "/proc/self/fd/%d", fd);
	int ro = open(path, O_RDONLY | O_CLOEXEC);
	char buf[8] = "";
	int same = ro >= 0 && pread(ro, buf, 5, 0) == 5 && !memcmp(buf, "hello", 5);
	struct stat a, b;
	int ino = ro >= 0 && !fstat(fd, &a) && !fstat(ro, &b) && a.st_ino == b.st_ino;
	errno = 0;
	void *w = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, ro, 0);
	int denied = w == MAP_FAILED && errno == EACCES;
	unsigned char *r = mmap(0, 4096, PROT_READ, MAP_SHARED, ro, 0);
	int mapped = r != MAP_FAILED;
	pwrite(fd, "world", 5, 0);
	int sees = mapped && !memcmp(r, "world", 5) && pread(ro, buf, 5, 0) == 5 && !memcmp(buf, "world", 5);
	errno = 0;
	int ro_write = write(ro, "x", 1) == -1 && errno == EBADF;
	int rw = open(path, O_RDWR | O_CLOEXEC);
	void *rw_map = rw >= 0 ? mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, rw, 0) : MAP_FAILED;
	int writable = rw_map != MAP_FAILED;
	if (writable) {
		memcpy(rw_map, "again", 5);
		writable = !memcmp(r, "again", 5);
		munmap(rw_map, 4096);
	}
	if (w != MAP_FAILED)
		munmap(w, 4096);
	if (mapped)
		munmap(r, 4096);
	if (rw >= 0)
		close(rw);
	if (ro >= 0)
		close(ro);
	close(fd);
	record("memfd_reopen", same && ino && denied && mapped && sees && ro_write && writable,
		"same=%d ino=%d denied=%d mapped=%d sees=%d ro_write=%d rw=%d", same, ino, denied, mapped, sees, ro_write,
		writable);
}

static void run_check(const char *name, void (*fn)(void))
{
	current_check = name;
	alarm(CHECK_DEADLINE_SECONDS);
	fn();
	alarm(0);
}

static int role_nnp(void)
{
	return prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1 && self_field_is("NoNewPrivs", "1") ? 0 : 1;
}

static int role_exe_link(const char *want)
{
	char link[sizeof self] = "";
	ssize_t n = readlink("/proc/self/exe", link, sizeof link - 1);
	if (n <= 0)
		return 2;
	link[n] = 0;
	return strcmp(link, want) ? 3 : 0;
}

static void check_reexec_proc_self_exe(void)
{
	pid_t pid = fork();
	if (pid == 0) {
		char *argv[] = { "/proc/self/exe", "--role=exe-link", self, 0 };
		execv("/proc/self/exe", argv);
		_exit(9);
	}
	int status = 0;
	waitpid(pid, &status, 0);
	record("reexec_proc_self_exe", WIFEXITED(status) && WEXITSTATUS(status) == 0, "status=0x%x", status);
}

int main(int argc, char **argv)
{
	ssize_t n = readlink("/proc/self/exe", self, sizeof self - 1);
	if (n > 0)
		self[n] = 0;
	else
		snprintf(self, sizeof self, "%s", argv[0]);
	if (argc > 1 && !strcmp(argv[1], "--role=nnp"))
		return role_nnp();
	if (argc > 2 && !strcmp(argv[1], "--role=exe-link"))
		return role_exe_link(argv[2]);
	signal(SIGALRM, deadline);
	if (geteuid() == 0) {
		run_check("root_drop", check_root_drop);
		run_check("root_transitions", check_root_transitions);
	}
	run_check("unprivileged_denied", check_unprivileged);
	run_check("capget_versions", check_capget_versions);
	run_check("no_new_privs", check_nnp);
	run_check("prctl_table", check_prctl);
	run_check("proc_status", check_proc_status);
	run_check("task_nlink", check_task_nlink);
	run_check("fd_listing", check_fd_listing);
	run_check("memfd_reopen", check_memfd_reopen);
	run_check("reexec_proc_self_exe", check_reexec_proc_self_exe);
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
