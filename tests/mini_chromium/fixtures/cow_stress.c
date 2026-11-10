#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <spawn.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum {
	PAGE = 4096,
	CHILDREN = 8,
	ANON_PAGES = 1024,
	FILE_PAGES = 32,
	DATA_PAGES = 128,
	BSS_PAGES = 128,
	HEAP_PAGES = 256,
	STACK_PAGES = 32,
	SHARED_PAGES = 64,
	NPROC_LIMIT = 6,
	ZOMBIES = 24,
};

static int passed, failed;

static void record(const char *name, int ok, const char *fmt, ...)
{
	char detail[256] = "", line[384];
	if (fmt) {
		va_list ap;
		va_start(ap, fmt);
		vsnprintf(detail, sizeof detail, fmt, ap);
		va_end(ap);
	}
	for (char *c = detail; *c; c++)
		if (*c == ' ')
			*c = ',';
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s%s%s\n", name, ok ? "PASS" : "FAIL",
		detail[0] ? " detail=" : "", detail);
	write(1, line, (size_t)len);
	if (ok)
		passed++;
	else
		failed++;
}

static unsigned char data_seg[DATA_PAGES * PAGE] = { 1 };
static unsigned char bss_seg[BSS_PAGES * PAGE];

struct region {
	const char *name;
	unsigned char *base;
	size_t pages;
};

enum { FILE_REGION = 1 };
static const unsigned char *file_original;

static int tracked(int region, size_t page)
{
	return region != FILE_REGION || page % 2 == 0;
}

static uint32_t pattern(int region, size_t page, uint32_t generation)
{
	return 0x9e3779b9u * (uint32_t)(region + 1) ^ (uint32_t)page * 0x85ebca6bu ^ generation * 0xc2b2ae35u;
}

static void fill_page(unsigned char *p, uint32_t v)
{
	uint32_t *w = (uint32_t *)p;
	for (size_t i = 0; i < PAGE / sizeof *w; i++)
		w[i] = v + (uint32_t)i;
}

static int page_is(const unsigned char *p, uint32_t v)
{
	const uint32_t *w = (const uint32_t *)p;
	for (size_t i = 0; i < PAGE / sizeof *w; i++)
		if (w[i] != v + (uint32_t)i)
			return 0;
	return 1;
}

static int read_full(int fd, void *buf, size_t len)
{
	unsigned char *b = buf;
	while (len) {
		ssize_t n = read(fd, b, len);
		if (n < 0 && errno == EINTR)
			continue;
		if (n <= 0)
			return 0;
		b += n;
		len -= (size_t)n;
	}
	return 1;
}

static int wait_child(pid_t pid, int *status)
{
	pid_t r;
	do
		r = waitpid(pid, status, 0);
	while (r < 0 && errno == EINTR);
	return r == pid;
}

static int exited_with(int status, int code)
{
	return WIFEXITED(status) && WEXITSTATUS(status) == code;
}

static uint32_t expected(int region, size_t page, int who, int parent_wrote, int child_wrote)
{
	if (who < 0)
		return pattern(region, page, parent_wrote && page % 3 == 0 ? 1 : 0);
	if (child_wrote && page % CHILDREN == (size_t)who)
		return pattern(region, page, 2 + (uint32_t)who);
	return pattern(region, page, 0);
}

static int check_regions(struct region *r, int nr, int who, int parent_wrote, int child_wrote)
{
	int bad = 0;
	for (int i = 0; i < nr; i++)
		for (size_t pg = 0; pg < r[i].pages; pg++) {
			const unsigned char *p = r[i].base + pg * PAGE;
			if (tracked(i, pg) ? !page_is(p, expected(i, pg, who, parent_wrote, child_wrote)) :
					     memcmp(p, file_original + pg * PAGE, PAGE) != 0)
				bad++;
		}
	return bad;
}

static void cow_child(struct region *r, int nr, int k, int ready_fd, int go_fd, int done_fd)
{
	int bad = check_regions(r, nr, k, 0, 0);
	write(ready_fd, "r", 1);
	char c;
	if (!read_full(go_fd, &c, 1))
		_exit(90);
	bad += check_regions(r, nr, k, 0, 0);
	for (int i = 0; i < nr; i++)
		for (size_t pg = k; pg < r[i].pages; pg += CHILDREN)
			if (tracked(i, pg))
				fill_page(r[i].base + pg * PAGE, pattern(i, pg, 2 + (uint32_t)k));
	sched_yield();
	bad += check_regions(r, nr, k, 0, 1);
	unsigned char result = bad > 255 ? 255 : (unsigned char)bad;
	write(done_fd, &result, 1);
	_exit(0);
}

static void check_cow(const char *self)
{
	unsigned char stack_region[STACK_PAGES * PAGE] __attribute__((aligned(PAGE)));
	unsigned char *anon = mmap(0, ANON_PAGES * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	unsigned char *heap = aligned_alloc(PAGE, HEAP_PAGES * PAGE);
	int fd = open(self, O_RDONLY | O_CLOEXEC);
	struct stat st;
	size_t file_pages = fd >= 0 && fstat(fd, &st) == 0 ? (size_t)st.st_size / PAGE : 0;
	if (file_pages > FILE_PAGES)
		file_pages = FILE_PAGES;
	unsigned char *file = file_pages ? mmap(0, file_pages * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0) : MAP_FAILED;
	if (anon == MAP_FAILED || !heap || file == MAP_FAILED || file_pages < 8) {
		record("cow_regions", 0, "setup errno=%d file_pages=%zu", errno, file_pages);
		return;
	}
	close(fd);
	struct region r[] = {
		{ "anon", anon, ANON_PAGES },
		[FILE_REGION] = { "file", file, file_pages },
		{ "data", data_seg, DATA_PAGES },
		{ "bss", bss_seg, BSS_PAGES },
		{ "heap", heap, HEAP_PAGES },
		{ "stack", stack_region, STACK_PAGES },
	};
	int nr = (int)(sizeof r / sizeof r[0]);
	unsigned char *file_copy = malloc(file_pages * PAGE);
	memcpy(file_copy, file, file_pages * PAGE);
	file_original = file_copy;
	for (int i = 0; i < nr; i++)
		for (size_t pg = 0; pg < r[i].pages; pg++)
			if (tracked(i, pg))
				fill_page(r[i].base + pg * PAGE, pattern(i, pg, 0));

	int ready[2], go[2], done[2];
	if (pipe(ready) || pipe(go) || pipe(done)) {
		record("cow_regions", 0, "pipe errno=%d", errno);
		return;
	}
	pid_t pids[CHILDREN];
	int forked = 0;
	for (int k = 0; k < CHILDREN; k++) {
		pids[k] = fork();
		if (pids[k] == 0) {
			close(ready[0]);
			close(go[1]);
			close(done[0]);
			cow_child(r, nr, k, ready[1], go[0], done[1]);
		}
		if (pids[k] > 0)
			forked++;
	}
	close(ready[1]);
	close(go[0]);
	close(done[1]);
	char c;
	int ready_seen = 0;
	while (ready_seen < forked && read_full(ready[0], &c, 1))
		ready_seen++;
	for (int i = 0; i < nr; i++)
		for (size_t pg = 0; pg < r[i].pages; pg += 3)
			if (tracked(i, pg))
				fill_page(r[i].base + pg * PAGE, pattern(i, pg, 1));
	for (int k = 0; k < forked; k++)
		write(go[1], "g", 1);
	int parent_bad = 0, rounds = 0;
	for (; rounds < 4; rounds++) {
		parent_bad += check_regions(r, nr, -1, 1, 0);
		sched_yield();
	}
	int child_bad = 0, reports = 0;
	unsigned char res;
	while (reports < forked && read_full(done[0], &res, 1)) {
		child_bad += res;
		reports++;
	}
	int exits_ok = 0;
	for (int k = 0; k < CHILDREN; k++) {
		int status = 0;
		if (pids[k] > 0 && wait_child(pids[k], &status) && exited_with(status, 0))
			exits_ok++;
	}
	parent_bad += check_regions(r, nr, -1, 1, 0);
	int file_untouched = 0;
	fd = open(self, O_RDONLY | O_CLOEXEC);
	if (fd >= 0) {
		unsigned char *again = mmap(0, file_pages * PAGE, PROT_READ, MAP_PRIVATE, fd, 0);
		file_untouched = again != MAP_FAILED && !memcmp(again, file_copy, file_pages * PAGE);
		if (again != MAP_FAILED)
			munmap(again, file_pages * PAGE);
		close(fd);
	}
	record("cow_regions", forked == CHILDREN && reports == CHILDREN && !child_bad && !parent_bad && exits_ok == CHILDREN &&
		file_untouched, "children=%d reports=%d child_bad=%d parent_bad=%d exits=%d file_untouched=%d",
		forked, reports, child_bad, parent_bad, exits_ok, file_untouched);
	close(ready[0]);
	close(go[1]);
	close(done[0]);
	free(file_copy);
	munmap(file, file_pages * PAGE);
	munmap(anon, ANON_PAGES * PAGE);
	free(heap);
}

static void check_cow_mprotect(void)
{
	unsigned char *p = mmap(0, 4 * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (p == MAP_FAILED) {
		record("cow_mprotect", 0, "mmap errno=%d", errno);
		return;
	}
	for (int i = 0; i < 4; i++)
		fill_page(p + i * PAGE, pattern(7, (size_t)i, 0));
	int done[2];
	if (pipe(done)) {
		record("cow_mprotect", 0, "pipe errno=%d", errno);
		return;
	}
	pid_t pid = fork();
	if (pid == 0) {
		int ok = !mprotect(p, 4 * PAGE, PROT_READ) && !mprotect(p, 4 * PAGE, PROT_READ | PROT_WRITE);
		for (int i = 0; i < 4; i++)
			fill_page(p + i * PAGE, pattern(7, (size_t)i, 9));
		for (int i = 0; i < 4; i++)
			ok = ok && page_is(p + i * PAGE, pattern(7, (size_t)i, 9));
		write(done[1], ok ? "y" : "n", 1);
		_exit(0);
	}
	char c = 0;
	read_full(done[0], &c, 1);
	int status = 0;
	int reaped = pid > 0 && wait_child(pid, &status) && exited_with(status, 0);
	int intact = 1;
	for (int i = 0; i < 4; i++)
		intact = intact && page_is(p + i * PAGE, pattern(7, (size_t)i, 0));
	if (!mprotect(p, 4 * PAGE, PROT_READ) && !mprotect(p, 4 * PAGE, PROT_READ | PROT_WRITE))
		fill_page(p, pattern(7, 0, 3));
	record("cow_mprotect", c == 'y' && reaped && intact && page_is(p, pattern(7, 0, 3)), "child=%c reaped=%d parent_intact=%d", c ? c : '0',
		reaped, intact);
	close(done[0]);
	close(done[1]);
	munmap(p, 4 * PAGE);
}

static void check_shared_anon(void)
{
	volatile uint32_t *s = mmap(0, SHARED_PAGES * PAGE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	if (s == MAP_FAILED) {
		record("cow_shared_anon", 0, "mmap errno=%d", errno);
		return;
	}
	for (int i = 0; i < SHARED_PAGES; i++)
		s[i * PAGE / 4] = 0;
	s[0] = 0x5eed;
	pid_t pids[CHILDREN];
	for (int k = 0; k < CHILDREN; k++) {
		pids[k] = fork();
		if (pids[k] == 0) {
			int ok = s[0] == 0x5eed;
			for (int i = k; i < SHARED_PAGES; i += CHILDREN)
				s[i * PAGE / 4 + 1] = 0xc0de0000u + (uint32_t)i;
			pid_t g = fork();
			if (g == 0) {
				s[k * PAGE / 4 + 2] = 0x9a9a0000u + (uint32_t)k;
				_exit(0);
			}
			int st = 0;
			ok = ok && g > 0 && wait_child(g, &st) && exited_with(st, 0) && s[k * PAGE / 4 + 2] == 0x9a9a0000u + (uint32_t)k;
			munmap((void *)s, SHARED_PAGES * PAGE);
			_exit(ok ? 0 : 1);
		}
	}
	int exits_ok = 0;
	for (int k = 0; k < CHILDREN; k++) {
		int status = 0;
		if (pids[k] > 0 && wait_child(pids[k], &status) && exited_with(status, 0))
			exits_ok++;
	}
	int seen = 0;
	for (int i = 0; i < SHARED_PAGES; i++)
		if (s[i * PAGE / 4 + 1] == 0xc0de0000u + (uint32_t)i)
			seen++;
	int grand = 0;
	for (int k = 0; k < CHILDREN; k++)
		if (s[k * PAGE / 4 + 2] == 0x9a9a0000u + (uint32_t)k)
			grand++;
	record("cow_shared_anon", exits_ok == CHILDREN && seen == SHARED_PAGES && grand == CHILDREN, "exits=%d seen=%d grandchildren=%d",
		exits_ok, seen, grand);
	munmap((void *)s, SHARED_PAGES * PAGE);
}

static int nproc_manager(void)
{
	if (unshare(CLONE_NEWUSER) && errno != ENOSYS && errno != EINVAL)
		return 10;
	struct rlimit rl = { NPROC_LIMIT, NPROC_LIMIT };
	if (setrlimit(RLIMIT_NPROC, &rl))
		return 11;
	struct rlimit got;
	if (getrlimit(RLIMIT_NPROC, &got) || got.rlim_cur != NPROC_LIMIT || got.rlim_max != NPROC_LIMIT)
		return 12;
	pid_t bomb[NPROC_LIMIT + 1];
	int forked = 0;
	for (;;) {
		pid_t pid = fork();
		if (pid == 0)
			_exit(3);
		if (pid < 0)
			break;
		if (forked == NPROC_LIMIT)
			return 13;
		bomb[forked++] = pid;
	}
	if (errno != EAGAIN || forked < 1)
		return 14;
	for (int i = 0; i < forked; i++) {
		siginfo_t si = { 0 };
		if (waitid(P_PID, (id_t)bomb[i], &si, WEXITED | WNOWAIT) || si.si_pid != bomb[i] || si.si_code != CLD_EXITED ||
		    si.si_status != 3)
			return 15;
	}
	errno = 0;
	if (fork() >= 0 || errno != EAGAIN)
		return 16;
	int status = 0;
	if (waitpid(bomb[0], &status, 0) != bomb[0] || !exited_with(status, 3))
		return 17;
	pid_t again = fork();
	if (again == 0)
		_exit(4);
	if (again < 0)
		return 18;
	if (!wait_child(again, &status) || !exited_with(status, 4))
		return 19;
	int reaped = 1;
	for (;;) {
		pid_t r = waitpid(-1, &status, 0);
		if (r < 0)
			break;
		if (!exited_with(status, 3))
			return 20;
		reaped++;
	}
	if (errno != ECHILD || reaped != forked)
		return 21;
	return 0;
}

static void check_nproc_zombies(void)
{
	pid_t pid = fork();
	if (pid == 0)
		_exit(nproc_manager());
	int status = 0;
	int reaped = pid > 0 && wait_child(pid, &status);
	record("lifecycle_nproc_zombies", reaped && exited_with(status, 0), "status=0x%x", status);
}

static int brk_split_worker(void)
{
	uintptr_t start = (uintptr_t)syscall(SYS_brk, 0);
	uintptr_t base = (start + PAGE - 1) & ~(uintptr_t)(PAGE - 1);
	if ((uintptr_t)syscall(SYS_brk, base + 4 * PAGE) != base + 4 * PAGE)
		return 10;
	if (mprotect((void *)base, PAGE, PROT_NONE))
		return 11;
	if ((uintptr_t)syscall(SYS_brk, base + 8 * PAGE) != base + 8 * PAGE)
		return 12;
	volatile unsigned char *top = (unsigned char *)(base + 7 * PAGE);
	*top = 0x5a;
	if (*top != 0x5a)
		return 13;
	if ((uintptr_t)syscall(SYS_brk, base + PAGE) != base + PAGE)
		return 14;
	if ((uintptr_t)syscall(SYS_brk, base + 3 * PAGE) != base + 3 * PAGE)
		return 15;
	volatile unsigned char *again = (unsigned char *)(base + 2 * PAGE);
	return *again == 0 ? 0 : 16;
}

static void check_brk_split(void)
{
	pid_t pid = fork();
	if (pid == 0)
		_exit(brk_split_worker());
	int status = 0;
	int reaped = pid > 0 && wait_child(pid, &status);
	record("lifecycle_brk_after_mprotect", reaped && exited_with(status, 0), "status=0x%x", status);
}

static void check_reparent(void)
{
	int report[2], hold[2];
	if (pipe(report) || pipe(hold)) {
		record("lifecycle_reparent", 0, "pipe errno=%d", errno);
		return;
	}
	pid_t reaper = getpid();
	pid_t c = fork();
	if (c == 0) {
		pid_t early = fork();
		if (early == 0)
			_exit(21);
		pid_t late = fork();
		if (late == 0) {
			char ch;
			close(hold[1]);
			read_full(hold[0], &ch, 1);
			pid_t ppid = getppid();
			write(report[1], &ppid, sizeof ppid);
			_exit(22);
		}
		siginfo_t si;
		waitid(P_PID, (id_t)early, &si, WEXITED | WNOWAIT);
		pid_t kids[2] = { early, late };
		write(report[1], kids, sizeof kids);
		_exit(20);
	}
	close(report[1]);
	pid_t kids[2] = { -1, -1 };
	int got_kids = read_full(report[0], kids, sizeof kids);
	int status = 0;
	int child_ok = c > 0 && wait_child(c, &status) && exited_with(status, 20);
	write(hold[1], "g", 1);
	pid_t seen_ppid = -1;
	int got_ppid = read_full(report[0], &seen_ppid, sizeof seen_ppid);
	int early_status = 0, late_status = 0;
	int early_ok = got_kids && wait_child(kids[0], &early_status) && exited_with(early_status, 21);
	int late_ok = got_kids && wait_child(kids[1], &late_status) && exited_with(late_status, 22);
	errno = 0;
	int echild = waitpid(-1, &status, WNOHANG) < 0 && errno == ECHILD;
	record("lifecycle_reparent", child_ok && got_ppid && seen_ppid == reaper && early_ok && late_ok && echild,
		"reaper=%d orphan_ppid=%d zombie_orphan=%d live_orphan=%d echild=%d", reaper, seen_ppid, early_ok, late_ok, echild);
	close(report[0]);
	close(hold[0]);
	close(hold[1]);
}

static volatile pid_t vfork_mark;

static void check_vfork(void)
{
	vfork_mark = 0;
	pid_t pid = vfork();
	if (pid == 0) {
		for (int i = 0; i < 200; i++)
			sched_yield();
		vfork_mark = getpid();
		_exit(7);
	}
	pid_t seen = vfork_mark;
	int status = 0;
	int reaped = pid > 0 && wait_child(pid, &status) && exited_with(status, 7);
	char *argv[] = { "/cow-stress-missing", 0 };
	pid_t spawned = -1;
	int rc = posix_spawn(&spawned, argv[0], 0, 0, argv, 0);
	errno = 0;
	int no_leftover = waitpid(-1, &status, WNOHANG) < 0 && errno == ECHILD;
	record("lifecycle_vfork", pid > 0 && seen == pid && reaped && (rc == ENOENT || rc == ENOSYS) && no_leftover,
		"pid=%d child_write=%d reaped=%d spawn_rc=%d no_leftover=%d", pid, seen, reaped, rc, no_leftover);
}

static long raw_clone(unsigned long flags)
{
	return syscall(SYS_clone, flags, 0, 0, 0, 0);
}

struct clone_args_v0 {
	uint64_t flags, pidfd, child_tid, parent_tid, exit_signal, stack, stack_size, tls;
};

static void check_clone_flags(void)
{
	int bad = 0;
	errno = 0;
	if (raw_clone(CLONE_SIGHAND | SIGCHLD) != -1 || errno != EINVAL)
		bad |= 1;
	errno = 0;
	if (raw_clone(CLONE_VM | CLONE_THREAD | SIGCHLD) != -1 || errno != EINVAL)
		bad |= 2;
	errno = 0;
	if (raw_clone(CLONE_NEWNS | CLONE_FS | SIGCHLD) != -1 || errno != EINVAL)
		bad |= 4;
	struct clone_args_v0 ca = { .flags = SIGCHLD, .exit_signal = 0 };
	errno = 0;
	if (syscall(SYS_clone3, &ca, sizeof ca) != -1 || errno != EINVAL)
		bad |= 8;
	ca.flags = 0;
	errno = 0;
	if (syscall(SYS_clone3, &ca, 32) != -1 || errno != EINVAL)
		bad |= 16;
	errno = 0;
	if (waitpid(-1, 0, 0x10000000) != -1 || errno != EINVAL)
		bad |= 32;
	siginfo_t si;
	errno = 0;
	if (waitid(P_ALL, 0, &si, WNOHANG) != -1 || errno != EINVAL)
		bad |= 64;
	errno = 0;
	if (waitid(P_PID, 0, &si, WEXITED) != -1 || errno != EINVAL)
		bad |= 128;
	errno = 0;
	if (waitpid(-1, 0, 0) != -1 || errno != ECHILD)
		bad |= 256;

	int fds[2];
	if (pipe(fds))
		bad |= 512;
	ca = (struct clone_args_v0){ .flags = 0, .exit_signal = SIGCHLD };
	long c = syscall(SYS_clone3, &ca, sizeof ca);
	if (c == 0) {
		close(fds[1]);
		_exit(dup2(fds[0], 200) == 200 ? 0 : 1);
	}
	int status = 0;
	if (c < 0 || !wait_child((pid_t)c, &status) || !exited_with(status, 0) || fcntl(200, F_GETFD) != -1)
		bad |= 1024;
	c = raw_clone(CLONE_FILES | SIGCHLD);
	if (c == 0)
		_exit(dup2(fds[0], 201) == 201 && close(fds[1]) == 0 ? 0 : 1);
	errno = 0;
	if (c < 0 || !wait_child((pid_t)c, &status) || !exited_with(status, 0) || fcntl(201, F_GETFD) < 0 ||
	    fcntl(fds[1], F_GETFD) != -1 || errno != EBADF)
		bad |= 2048;
	close(201);
	close(fds[0]);
	record("lifecycle_clone_flags", !bad, "bad=0x%x", bad);
}

int main(int argc, char **argv)
{
	(void)argc;
	if (getpid() != 1 && prctl(PR_SET_CHILD_SUBREAPER, 1)) {
		record("lifecycle_subreaper", 0, "errno=%d", errno);
		return 1;
	}
	check_brk_split();
	check_cow(argv[0]);
	check_cow_mprotect();
	check_shared_anon();
	check_nproc_zombies();
	check_reparent();
	check_vfork();
	check_clone_flags();
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed,
		failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
