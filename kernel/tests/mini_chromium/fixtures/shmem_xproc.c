#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <setjmp.h>
#include <stddef.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define FUTEX_WAIT 0
#define FUTEX_WAKE 1
#define FUTEX_CMP_REQUEUE 4
#define FUTEX_WAIT_BITSET 9
#define FUTEX_WAKE_BITSET 10
#define FUTEX_PRIVATE_FLAG 128
#define FUTEX_CLOCK_REALTIME 256
#define FUTEX_WAIT_PRIVATE (FUTEX_WAIT | FUTEX_PRIVATE_FLAG)
#define FUTEX_WAKE_PRIVATE (FUTEX_WAKE | FUTEX_PRIVATE_FLAG)
#define FUTEX_WAITERS 0x80000000u
#define FUTEX_OWNER_DIED 0x40000000u

#ifndef MFD_NOEXEC_SEAL
#define MFD_NOEXEC_SEAL 0x0008U
#endif

#define PAGE 4096
#define WAIT_MS 5000

static int passed, failed;

static void record(const char *name, int ok, const char *detail)
{
	char line[320];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name,
		ok ? "PASS" : "FAIL", detail);
	write(1, line, (size_t)n);
	ok ? passed++ : failed++;
}

static long futex(volatile uint32_t *addr, int op, uint32_t val, const struct timespec *ts, volatile uint32_t *addr2,
	uint32_t val3)
{
	return syscall(SYS_futex, addr, op, val, ts, addr2, val3);
}

static long elapsed_ms(const struct timespec *start)
{
	struct timespec now;
	clock_gettime(CLOCK_MONOTONIC, &now);
	return (now.tv_sec - start->tv_sec) * 1000 + (now.tv_nsec - start->tv_nsec) / 1000000;
}

static void pause_ms(long ms)
{
	struct timespec ts = { ms / 1000, (ms % 1000) * 1000000 };
	nanosleep(&ts, 0);
}

static int reap(pid_t pid, long timeout_ms)
{
	struct timespec start;
	clock_gettime(CLOCK_MONOTONIC, &start);
	int status;
	while (elapsed_ms(&start) < timeout_ms) {
		pid_t r = waitpid(pid, &status, WNOHANG);
		if (r == pid)
			return WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
		if (r < 0)
			return -2;
		pause_ms(5);
	}
	kill(pid, SIGKILL);
	waitpid(pid, &status, 0);
	return -1;
}

static void signal_byte(int fd)
{
	if (write(fd, "x", 1) != 1)
		_exit(90);
}

static int await_byte(int fd)
{
	char c;
	return read(fd, &c, 1) == 1;
}

static long wake_until(volatile uint32_t *word, int op, uint32_t val3, long want)
{
	struct timespec start;
	clock_gettime(CLOCK_MONOTONIC, &start);
	long got = 0;
	while (elapsed_ms(&start) < WAIT_MS) {
		long r = futex(word, op, INT_MAX, 0, 0, val3);
		if (r < 0)
			return r;
		got += r;
		if (got >= want)
			break;
		pause_ms(2);
	}
	return got;
}

static int wait_once(volatile uint32_t *word, int op, uint32_t val3)
{
	struct timespec ts = { WAIT_MS / 1000, 0 };
	if (op == FUTEX_WAIT_BITSET) {
		struct timespec now;
		clock_gettime(CLOCK_MONOTONIC, &now);
		ts.tv_sec += now.tv_sec;
		ts.tv_nsec = now.tv_nsec;
	}
	return futex(word, op, 0, &ts, 0, val3) == 0;
}

struct region {
	const char *name;
	volatile uint8_t *parent;
	int memfd;
};

static volatile uint8_t *child_view(struct region *r, volatile uint8_t *avoid)
{
	void *view;
	if (r->memfd >= 0) {
		view = mmap(0, 2 * PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, r->memfd, 0);
	} else {
		void *target = mmap(0, 4 * PAGE, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (target == MAP_FAILED)
			return 0;
		munmap(target, 4 * PAGE);
		view = mremap((void *)r->parent, 2 * PAGE, 2 * PAGE, MREMAP_MAYMOVE | MREMAP_FIXED, (uint8_t *)target + PAGE);
	}
	if (view == MAP_FAILED || view == (void *)avoid)
		return 0;
	return view;
}

static void check_mremap_sole_owner(void)
{
	char detail[96];
	int bad = 0, round;
	for (round = 0; round < 64 && !bad; round++) {
		volatile uint32_t *a = mmap(0, 2 * PAGE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
		void *hole = mmap(0, 4 * PAGE, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (a == MAP_FAILED || hole == MAP_FAILED) {
			bad = 1;
			break;
		}
		munmap(hole, 4 * PAGE);
		a[0] = 0x5eed0000u + round;
		a[PAGE / 4] = ~(0x5eed0000u + round);
		volatile uint32_t *b = mremap((void *)a, 2 * PAGE, 2 * PAGE, MREMAP_MAYMOVE | MREMAP_FIXED, (uint8_t *)hole + PAGE);
		if (b == MAP_FAILED) {
			bad = 2;
			break;
		}
		pid_t pid = fork();
		if (pid == 0) {
			if (b[0] != 0x5eed0000u + round || b[PAGE / 4] != ~(0x5eed0000u + round))
				_exit(3);
			b[1] = round;
			_exit(0);
		}
		int status = reap(pid, WAIT_MS);
		if (status != 0 || b[1] != (uint32_t)round)
			bad = 3;
		munmap((void *)b, 2 * PAGE);
	}
	snprintf(detail, sizeof detail, "rounds=%d,bad=%d", round, bad);
	record("shmem_xproc_anon_mremap_sole_owner", !bad, detail);
}

static void check_cross_writes(struct region *r)
{
	int to_child[2], to_parent[2];
	char name[64], detail[128];
	snprintf(name, sizeof name, "shmem_xproc_%s_writes", r->name);
	if (pipe(to_child) || pipe(to_parent)) {
		record(name, 0, "pipe-failed");
		return;
	}
	r->parent[0] = 0;
	r->parent[PAGE] = 0;
	pid_t pid = fork();
	if (pid == 0) {
		volatile uint8_t *v = child_view(r, r->parent);
		if (!v)
			_exit(2);
		v[0] = 'c';
		signal_byte(to_parent[1]);
		if (!await_byte(to_child[0]))
			_exit(3);
		_exit(v[PAGE] == 'p' ? 0 : 4);
	}
	int parent_saw = await_byte(to_parent[0]) && r->parent[0] == 'c';
	r->parent[PAGE] = 'p';
	signal_byte(to_child[1]);
	int status = reap(pid, WAIT_MS);
	snprintf(detail, sizeof detail, "parent_saw=%d,child_status=%d", parent_saw, status);
	record(name, parent_saw && status == 0, detail);
	close(to_child[0]);
	close(to_child[1]);
	close(to_parent[0]);
	close(to_parent[1]);
}

static void check_wait_wake(struct region *r)
{
	char name[64], detail[128];
	snprintf(name, sizeof name, "shmem_xproc_%s_futex_wake", r->name);
	volatile uint32_t *word = (volatile uint32_t *)(r->parent + 64);
	*word = 0;
	pid_t pid = fork();
	if (pid == 0) {
		volatile uint8_t *v = child_view(r, r->parent);
		if (!v)
			_exit(2);
		_exit(wait_once((volatile uint32_t *)(v + 64), FUTEX_WAIT, 0) ? 0 : 3);
	}
	long woken = wake_until(word, FUTEX_WAKE, 0, 1);
	int status = reap(pid, WAIT_MS);
	snprintf(detail, sizeof detail, "woken=%ld,child_status=%d", woken, status);
	record(name, woken == 1 && status == 0, detail);
}

static void check_cmp_requeue(struct region *r)
{
	char name[64], detail[160];
	snprintf(name, sizeof name, "shmem_xproc_%s_cmp_requeue", r->name);
	volatile uint32_t *from = (volatile uint32_t *)(r->parent + 128);
	volatile uint32_t *to = (volatile uint32_t *)(r->parent + PAGE + 128);
	*from = 0;
	*to = 0;
	pid_t pid = fork();
	if (pid == 0) {
		volatile uint8_t *v = child_view(r, r->parent);
		if (!v)
			_exit(2);
		_exit(wait_once((volatile uint32_t *)(v + 128), FUTEX_WAIT, 0) ? 0 : 3);
	}
	struct timespec start;
	clock_gettime(CLOCK_MONOTONIC, &start);
	long requeued = 0;
	while (!requeued && elapsed_ms(&start) < WAIT_MS) {
		requeued = futex(from, FUTEX_CMP_REQUEUE, 0, (void *)1, to, 0);
		if (requeued < 0)
			break;
		if (!requeued)
			pause_ms(2);
	}
	errno = 0;
	long mismatch = futex(from, FUTEX_CMP_REQUEUE, 0, (void *)1, to, 7);
	int eagain = mismatch < 0 && errno == EAGAIN;
	long woken_from = futex(from, FUTEX_WAKE, INT_MAX, 0, 0, 0);
	long woken_to = wake_until(to, FUTEX_WAKE, 0, 1);
	int status = reap(pid, WAIT_MS);
	snprintf(detail, sizeof detail, "requeued=%ld,eagain=%d,woken_from=%ld,woken_to=%ld,child_status=%d", requeued,
		eagain, woken_from, woken_to, status);
	record(name, requeued == 1 && eagain && woken_from == 0 && woken_to == 1 && status == 0, detail);
}

static void check_wake_bitset(struct region *r)
{
	char name[64], detail[160];
	snprintf(name, sizeof name, "shmem_xproc_%s_wake_bitset", r->name);
	volatile uint32_t *word = (volatile uint32_t *)(r->parent + 192);
	*word = 0;
	pid_t pid[2];
	for (int i = 0; i < 2; i++) {
		pid[i] = fork();
		if (pid[i] == 0) {
			volatile uint8_t *v = child_view(r, r->parent);
			if (!v)
				_exit(2);
			_exit(wait_once((volatile uint32_t *)(v + 192), FUTEX_WAIT_BITSET, 1u << i) ? 0 : 3);
		}
	}
	long second = wake_until(word, FUTEX_WAKE_BITSET, 2, 1);
	int status1 = reap(pid[1], WAIT_MS);
	int status;
	int first_slept = waitpid(pid[0], &status, WNOHANG) == 0;
	long first = wake_until(word, FUTEX_WAKE_BITSET, 1, 1);
	int status0 = reap(pid[0], WAIT_MS);
	errno = 0;
	int wait_zero = futex(word, FUTEX_WAIT_BITSET, 0, 0, 0, 0) < 0 && errno == EINVAL;
	errno = 0;
	int wake_zero = futex(word, FUTEX_WAKE_BITSET, 1, 0, 0, 0) < 0 && errno == EINVAL;
	snprintf(detail, sizeof detail, "second=%ld,first_slept=%d,first=%ld,status=%d,%d,zero_einval=%d,%d", second,
		first_slept, first, status0, status1, wait_zero, wake_zero);
	record(name, second == 1 && first_slept && first == 1 && status0 == 0 && status1 == 0 && wait_zero && wake_zero,
		detail);
}

static void check_clock_realtime(void)
{
	char detail[96];
	volatile uint32_t word = 0;
	struct timespec past, rel = { 0, 1000000 };
	clock_gettime(CLOCK_REALTIME, &past);
	past.tv_sec -= 1;
	errno = 0;
	int bitset_timedout = futex(&word, FUTEX_WAIT_BITSET | FUTEX_CLOCK_REALTIME, 0, &past, 0, ~0u) < 0 &&
		errno == ETIMEDOUT;
	struct timespec before_boot = { 1, 0 };
	errno = 0;
	bitset_timedout = bitset_timedout &&
		futex(&word, FUTEX_WAIT_BITSET | FUTEX_CLOCK_REALTIME, 0, &before_boot, 0, ~0u) < 0 && errno == ETIMEDOUT;
	errno = 0;
	int wait_enosys = futex(&word, FUTEX_WAIT | FUTEX_CLOCK_REALTIME, 0, &rel, 0, 0) < 0 && errno == ENOSYS;
	errno = 0;
	int wake_enosys = futex(&word, FUTEX_WAKE | FUTEX_CLOCK_REALTIME, 1, 0, 0, 0) < 0 && errno == ENOSYS;
	snprintf(detail, sizeof detail, "bitset_timedout=%d,wait_enosys=%d,wake_enosys=%d", bitset_timedout, wait_enosys,
		wake_enosys);
	record("shmem_xproc_futex_clock_realtime", bitset_timedout && wait_enosys && wake_enosys, detail);
}

static void check_private_flag(struct region *r)
{
	char name[64], detail[160];
	snprintf(name, sizeof name, "shmem_xproc_%s_private_stays_private", r->name);
	volatile uint32_t *word = (volatile uint32_t *)(r->parent + 320);
	*word = 0;
	int ready[2];
	if (pipe(ready)) {
		record(name, 0, "pipe-failed");
		return;
	}
	pid_t pid = fork();
	if (pid == 0) {
		volatile uint8_t *v = child_view(r, r->parent);
		if (!v)
			_exit(2);
		struct timespec ts = { 0, 400 * 1000 * 1000 };
		signal_byte(ready[1]);
		long rc = futex((volatile uint32_t *)(v + 320), FUTEX_WAIT_PRIVATE, 0, &ts, 0, 0);
		_exit(rc < 0 && errno == ETIMEDOUT ? 0 : 3);
	}
	await_byte(ready[0]);
	long woken = 0;
	struct timespec start;
	clock_gettime(CLOCK_MONOTONIC, &start);
	while (elapsed_ms(&start) < 250) {
		woken += futex(word, FUTEX_WAKE, INT_MAX, 0, 0, 0);
		woken += futex(word, FUTEX_WAKE_PRIVATE, INT_MAX, 0, 0, 0);
		pause_ms(5);
	}
	int status = reap(pid, WAIT_MS);
	snprintf(detail, sizeof detail, "woken=%ld,child_status=%d", woken, status);
	record(name, woken == 0 && status == 0, detail);
	close(ready[0]);
	close(ready[1]);
}

static void check_robust_owner_death(volatile uint8_t *shared)
{
	char detail[128];
	pthread_mutex_t *m = (pthread_mutex_t *)(shared + 1024);
	pthread_mutexattr_t a;
	pthread_mutexattr_init(&a);
	pthread_mutexattr_setpshared(&a, PTHREAD_PROCESS_SHARED);
	pthread_mutexattr_setrobust(&a, PTHREAD_MUTEX_ROBUST);
	pthread_mutex_init(m, &a);
	int locked[2], release[2];
	if (pipe(locked) || pipe(release)) {
		record("shmem_xproc_robust_owner_died", 0, "pipe-failed");
		return;
	}
	pid_t pid = fork();
	if (pid == 0) {
		if (pthread_mutex_lock(m))
			_exit(2);
		signal_byte(locked[1]);
		await_byte(release[0]);
		pause_ms(100);
		_exit(0);
	}
	await_byte(locked[0]);
	signal_byte(release[1]);
	int rc = pthread_mutex_lock(m);
	int consistent = rc == EOWNERDEAD && !pthread_mutex_consistent(m) && !pthread_mutex_unlock(m);
	int status = reap(pid, WAIT_MS);
	snprintf(detail, sizeof detail, "lock_rc=%d,consistent=%d,child_status=%d", rc, consistent, status);
	record("shmem_xproc_robust_owner_died", consistent && status == 0, detail);
	close(locked[0]);
	close(locked[1]);
	close(release[0]);
	close(release[1]);
}

struct robust_entry {
	struct robust_entry *next;
	uint32_t word;
};

struct robust_head {
	struct robust_entry *list;
	long futex_offset;
	struct robust_entry *pending;
};

static struct robust_head *robust_setup(volatile uint8_t *shared, int cyclic)
{
	struct robust_head *h = (struct robust_head *)(shared + 2048);
	struct robust_entry *e = (struct robust_entry *)(shared + 2048 + 64);
	h->futex_offset = offsetof(struct robust_entry, word);
	h->pending = 0;
	h->list = &e[0];
	e[0].next = &e[1];
	e[1].next = cyclic ? &e[0] : (struct robust_entry *)h;
	return h;
}

static void check_robust_entries(volatile uint8_t *shared)
{
	char detail[128];
	struct robust_entry *e = (struct robust_entry *)(shared + 2048 + 64);
	pid_t pid = fork();
	if (pid == 0) {
		struct robust_head *h = robust_setup(shared, 0);
		e[0].word = (uint32_t)syscall(SYS_gettid);
		e[1].word = 0x7ffff | FUTEX_WAITERS;
		if (syscall(SYS_set_robust_list, h, sizeof *h))
			_exit(2);
		_exit(0);
	}
	int status = reap(pid, WAIT_MS);
	uint32_t owned = e[0].word, foreign = e[1].word;
	snprintf(detail, sizeof detail, "owned=0x%x,foreign=0x%x,child_status=%d", owned, foreign, status);
	record("shmem_xproc_robust_owner_only", status == 0 && owned == FUTEX_OWNER_DIED &&
		foreign == (0x7ffff | FUTEX_WAITERS), detail);
}

static void check_robust_cycle(volatile uint8_t *shared)
{
	char detail[96];
	pid_t pid = fork();
	if (pid == 0) {
		struct robust_head *h = robust_setup(shared, 1);
		struct robust_entry *e = h->list;
		e[0].word = e[1].word = (uint32_t)syscall(SYS_gettid);
		if (syscall(SYS_set_robust_list, h, sizeof *h))
			_exit(2);
		_exit(0);
	}
	int status = reap(pid, WAIT_MS);
	snprintf(detail, sizeof detail, "child_status=%d", status);
	record("shmem_xproc_robust_cycle_exits", status == 0, detail);
}

static int expect_errno(long rc, int want)
{
	return rc < 0 && errno == want;
}

static void check_seals(void)
{
	char detail[256];
	int fd = memfd_create("shmem-xproc-seals", MFD_ALLOW_SEALING);
	int to_child[2], to_parent[2];
	if (fd < 0 || ftruncate(fd, 2 * PAGE) || pipe(to_child) || pipe(to_parent)) {
		record("shmem_xproc_seals", 0, "setup-failed");
		return;
	}
	volatile uint32_t *report = mmap(0, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	if (report == MAP_FAILED) {
		record("shmem_xproc_seals", 0, "mmap-failed");
		return;
	}
	memset((void *)report, 0, PAGE);
	pid_t pid = fork();
	if (pid == 0) {
		volatile uint8_t *w = mmap(0, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
		if (w == MAP_FAILED)
			_exit(2);
		w[0] = 'w';
		signal_byte(to_parent[1]);
		await_byte(to_child[0]);
		errno = 0;
		report[0] = expect_errno(ftruncate(fd, PAGE), EPERM);
		errno = 0;
		report[1] = expect_errno(ftruncate(fd, 4 * PAGE), EPERM);
		munmap((void *)w, PAGE);
		signal_byte(to_parent[1]);
		await_byte(to_child[0]);
		errno = 0;
		report[2] = mmap(0, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) == MAP_FAILED && errno == EPERM;
		errno = 0;
		report[3] = expect_errno(pwrite(fd, "x", 1, 0), EPERM);
		volatile uint8_t *r = mmap(0, PAGE, PROT_READ, MAP_SHARED, fd, 0);
		report[4] = r != MAP_FAILED && r[0] == 'w';
		errno = 0;
		report[5] = r != MAP_FAILED && expect_errno(mprotect((void *)r, PAGE, PROT_READ | PROT_WRITE), EACCES);
		_exit(0);
	}
	await_byte(to_parent[0]);
	int added = !fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK | F_SEAL_GROW);
	errno = 0;
	int busy = expect_errno(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY);
	signal_byte(to_child[1]);
	await_byte(to_parent[0]);
	int sealed = !fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
	int seals = fcntl(fd, F_GET_SEALS);
	signal_byte(to_child[1]);
	int status = reap(pid, WAIT_MS);
	snprintf(detail, sizeof detail,
		"added=%d,busy=%d,sealed=%d,seals=0x%x,child_shrink=%u,child_grow=%u,child_wmap=%u,child_write=%u,"
		"child_rmap=%u,child_mprotect=%u,child_status=%d",
		added, busy, sealed, seals, report[0], report[1], report[2], report[3], report[4], report[5], status);
	record("shmem_xproc_seals", added && busy && sealed && seals == (F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE) &&
		report[0] && report[1] && report[2] && report[3] && report[4] && report[5] && status == 0, detail);
	munmap((void *)report, PAGE);
	close(fd);
	close(to_child[0]);
	close(to_child[1]);
	close(to_parent[0]);
	close(to_parent[1]);
}

static sigjmp_buf bus_jump;

static void on_bus(int sig)
{
	(void)sig;
	siglongjmp(bus_jump, 1);
}

static void check_truncate_unmaps(void)
{
	char detail[96];
	int fd = memfd_create("shmem-xproc-truncate", 0);
	int to_child[2], to_parent[2];
	if (fd < 0 || ftruncate(fd, 2 * PAGE) || pipe(to_child) || pipe(to_parent)) {
		record("shmem_xproc_truncate_unmaps", 0, "setup-failed");
		return;
	}
	pid_t pid = fork();
	if (pid == 0) {
		volatile uint8_t *v = mmap(0, 2 * PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
		if (v == MAP_FAILED)
			_exit(2);
		v[PAGE] = 't';
		signal(SIGBUS, on_bus);
		signal_byte(to_parent[1]);
		await_byte(to_child[0]);
		if (sigsetjmp(bus_jump, 1))
			_exit(0);
		_exit(v[PAGE] == 't' ? 3 : 4);
	}
	await_byte(to_parent[0]);
	int truncated = !ftruncate(fd, PAGE);
	signal_byte(to_child[1]);
	int status = reap(pid, WAIT_MS);
	snprintf(detail, sizeof detail, "truncated=%d,child_status=%d", truncated, status);
	record("shmem_xproc_truncate_unmaps", truncated && status == 0, detail);
	close(fd);
	close(to_child[0]);
	close(to_child[1]);
	close(to_parent[0]);
	close(to_parent[1]);
}

static void check_memfd_flags(void)
{
	char detail[160], name[300];
	errno = 0;
	int unknown = memfd_create("x", 0x100) < 0 && errno == EINVAL;
	errno = 0;
	int all = memfd_create("x", ~0u) < 0 && errno == EINVAL;
	int noexec = memfd_create("x", MFD_NOEXEC_SEAL);
	int noexec_seals = noexec >= 0 ? fcntl(noexec, F_GET_SEALS) : -1;
	int cloexec = memfd_create("x", MFD_CLOEXEC);
	int cloexec_flag = cloexec >= 0 ? fcntl(cloexec, F_GETFD) : -1;
	int plain = memfd_create("x", 0);
	int plain_seals = plain >= 0 ? fcntl(plain, F_GET_SEALS) : -1;
	int plain_flag = plain >= 0 ? fcntl(plain, F_GETFD) : -1;
	memset(name, 'n', sizeof name);
	name[249] = 0;
	int longest = memfd_create(name, 0);
	name[249] = 'n';
	name[250] = 0;
	errno = 0;
	int too_long = memfd_create(name, 0) < 0 && errno == EINVAL;
	snprintf(detail, sizeof detail,
		"unknown_einval=%d,all_einval=%d,noexec_seals=0x%x,cloexec_fd=0x%x,plain_fd=0x%x,plain_seals=0x%x,name249=%d,"
		"name250_einval=%d",
		unknown, all, noexec_seals, cloexec_flag, plain_flag, plain_seals, longest >= 0, too_long);
	record("shmem_xproc_memfd_flags", unknown && all && noexec_seals == 0x20 && cloexec_flag == FD_CLOEXEC && plain_flag == 0 &&
		plain_seals == F_SEAL_SEAL && longest >= 0 && too_long, detail);
	int fds[] = { noexec, cloexec, plain, longest };
	for (int f = 0; f < 4; f++)
		if (fds[f] >= 0)
			close(fds[f]);
}

int main(void)
{
	int memfd = memfd_create("shmem-xproc", MFD_ALLOW_SEALING);
	void *mem = memfd >= 0 ? mmap(0, 2 * PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, memfd, 0) : MAP_FAILED;
	void *alias = memfd >= 0 ? mmap(0, 2 * PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, memfd, 0) : MAP_FAILED;
	void *anon = mmap(0, 2 * PAGE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	if (memfd < 0 || ftruncate(memfd, 2 * PAGE) || mem == MAP_FAILED || alias == MAP_FAILED || anon == MAP_FAILED) {
		record("shmem_xproc_setup", 0, "memfd-or-mmap-failed");
		goto out;
	}
	((volatile uint8_t *)mem)[PAGE + 7] = 'a';
	record("shmem_xproc_memfd_alias", mem != alias && ((volatile uint8_t *)alias)[PAGE + 7] == 'a', "");
	struct region regions[] = {
		{ "memfd", mem, memfd },
		{ "anon", anon, -1 },
	};
	for (int i = 0; i < 2; i++) {
		check_cross_writes(&regions[i]);
		check_wait_wake(&regions[i]);
		check_cmp_requeue(&regions[i]);
		check_wake_bitset(&regions[i]);
		check_private_flag(&regions[i]);
	}
	check_clock_realtime();
	check_mremap_sole_owner();
	check_robust_owner_death(anon);
	check_robust_entries(anon);
	check_robust_cycle(anon);
	check_seals();
	check_truncate_unmaps();
	check_memfd_flags();
out:;
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
