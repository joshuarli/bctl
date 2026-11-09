#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sched.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

#define ROUNDS 10000
#define PAGE 4096
#define SHARED_VA ((void *)0x200000000000ul)
#define KERNEL_VA ((volatile char *)0xffff800000000000ul)
#define PARENT_BYTE 'P'
#define CHILD_BYTE 'C'
#define FOREIGN_BYTE 'X'

struct child_report {
	int rounds_ok;
	int dumpable_after_clear;
	int proc_mem_rc;
	int proc_mem_errno;
	int vm_writev_rc;
	int vm_writev_errno;
	int kernel_read_faulted;
	int kernel_write_faulted;
	int pattern_ok_at_exit;
};

static int passed, failed;
static sigjmp_buf fault_jump;

static void record(const char *name, int ok, const char *detail)
{
	char line[256];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name,
		ok ? "PASS" : "FAIL", detail);
	write(1, line, (size_t)n);
	ok ? passed++ : failed++;
}

static void on_segv(int sig)
{
	(void)sig;
	siglongjmp(fault_jump, 1);
}

static int kernel_read_faults(void)
{
	if (sigsetjmp(fault_jump, 1))
		return 1;
	volatile char c = *KERNEL_VA;
	(void)c;
	return 0;
}

static int kernel_write_faults(void)
{
	if (sigsetjmp(fault_jump, 1))
		return 1;
	*KERNEL_VA = FOREIGN_BYTE;
	return 0;
}

static int page_is(const volatile char *p, char want)
{
	for (int i = 0; i < PAGE; i++)
		if (p[i] != want)
			return 0;
	return 1;
}

static int read_full(int fd, void *buf, size_t len)
{
	size_t done = 0;
	while (done < len) {
		ssize_t n = read(fd, (char *)buf + done, len - done);
		if (n < 0 && errno == EINTR)
			continue;
		if (n <= 0)
			return 0;
		done += (size_t)n;
	}
	return 1;
}

static long foreign_write(pid_t pid)
{
	char src[16];
	memset(src, FOREIGN_BYTE, sizeof src);
	struct iovec local = { src, sizeof src };
	struct iovec remote = { SHARED_VA, sizeof src };
	errno = 0;
	return syscall(SYS_process_vm_writev, pid, &local, 1, &remote, 1, 0);
}

static int denied(long rc, int err)
{
	return rc == -1 && (err == EPERM || err == ENOSYS);
}

static void child(int from_parent, int to_parent)
{
	struct child_report r = { 0 };
	volatile char *page = SHARED_VA;
	r.dumpable_after_clear = prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) == 0 ? prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) : -1;
	memset((void *)page, CHILD_BYTE, PAGE);

	int fd = open("/proc/self/mem", O_RDWR);
	r.proc_mem_errno = fd < 0 ? errno : 0;
	if (fd >= 0) {
		char same[16];
		memset(same, CHILD_BYTE, sizeof same);
		r.proc_mem_rc = (int)pwrite(fd, same, sizeof same, (off_t)(uintptr_t)SHARED_VA);
		close(fd);
	} else {
		r.proc_mem_rc = -1;
	}
	r.vm_writev_rc = (int)foreign_write(getppid());
	r.vm_writev_errno = errno;
	r.kernel_read_faulted = kernel_read_faults();
	r.kernel_write_faulted = kernel_write_faults();

	r.rounds_ok = 1;
	for (int i = 0; i < ROUNDS; i++) {
		char token;
		if (!read_full(from_parent, &token, 1)) {
			r.rounds_ok = 0;
			break;
		}
		if (!page_is(page, CHILD_BYTE))
			r.rounds_ok = 0;
		if (write(to_parent, &token, 1) != 1)
			r.rounds_ok = 0;
		sched_yield();
	}
	char go;
	if (!read_full(from_parent, &go, 1))
		r.rounds_ok = 0;
	r.pattern_ok_at_exit = page_is(page, CHILD_BYTE);
	write(to_parent, &r, sizeof r);
	_exit(0);
}

int main(void)
{
	char detail[128];
	struct sigaction sa = { 0 };
	sa.sa_handler = on_segv;
	sa.sa_flags = SA_NODEFER;
	sigaction(SIGSEGV, &sa, 0);

	volatile char *page = mmap(SHARED_VA, PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
	if (page != SHARED_VA) {
		record("vm_isolation_map", 0, "mmap-failed");
		goto out;
	}
	memset((void *)page, PARENT_BYTE, PAGE);

	int down[2], up[2];
	if (pipe(down) || pipe(up)) {
		record("vm_isolation_pipes", 0, "pipe-failed");
		goto out;
	}
	long pid = syscall(SYS_clone, SIGCHLD, 0, 0, 0, 0);
	if (pid < 0) {
		snprintf(detail, sizeof detail, "errno=%d", errno);
		record("vm_isolation_fork", 0, detail);
		goto out;
	}
	if (pid == 0) {
		close(down[1]);
		close(up[0]);
		child(down[0], up[1]);
	}
	close(down[0]);
	close(up[1]);

	int rounds_ok = 1, round = 0;
	for (; round < ROUNDS; round++) {
		char token = (char)round, echo;
		if (write(down[1], &token, 1) != 1 || !page_is(page, PARENT_BYTE) || !read_full(up[0], &echo, 1) ||
			echo != token) {
			rounds_ok = 0;
			break;
		}
		sched_yield();
	}
	snprintf(detail, sizeof detail, "rounds=%d", round);
	record("vm_isolation_parent_rounds", rounds_ok, detail);

	long rc = foreign_write((pid_t)pid);
	int err = errno;
	snprintf(detail, sizeof detail, "rc=%ld,errno=%d", rc, err);
	record("vm_isolation_parent_to_child_vm_writev", denied(rc, err), detail);

	char go = 'E';
	struct child_report r;
	int got = write(down[1], &go, 1) == 1 && read_full(up[0], &r, sizeof r);
	record("vm_isolation_child_report", got, got ? "received" : "missing");
	if (got) {
		snprintf(detail, sizeof detail, "rounds_ok=%d,exit_pattern=%d", r.rounds_ok, r.pattern_ok_at_exit);
		record("vm_isolation_child_rounds", r.rounds_ok && r.pattern_ok_at_exit, detail);
		snprintf(detail, sizeof detail, "value=%d", r.dumpable_after_clear);
		record("vm_isolation_child_dumpable", r.dumpable_after_clear == 0, detail);
		snprintf(detail, sizeof detail, "rc=%d,errno=%d", r.proc_mem_rc, r.proc_mem_errno);
		record("vm_isolation_child_proc_mem", r.proc_mem_errno == ENOENT || r.proc_mem_errno == EACCES || r.proc_mem_rc == 16, detail);
		snprintf(detail, sizeof detail, "rc=%d,errno=%d", r.vm_writev_rc, r.vm_writev_errno);
		record("vm_isolation_child_to_parent_vm_writev", denied(r.vm_writev_rc, r.vm_writev_errno), detail);
		snprintf(detail, sizeof detail, "read=%d,write=%d", r.kernel_read_faulted, r.kernel_write_faulted);
		record("vm_isolation_child_kernel_address", r.kernel_read_faulted && r.kernel_write_faulted, detail);
	}

	char extra;
	ssize_t n;
	do
		n = read(up[0], &extra, 1);
	while (n < 0 && errno == EINTR);
	snprintf(detail, sizeof detail, "read=%zd", n);
	record("vm_isolation_eof_after_child_exit", n == 0, detail);

	record("vm_isolation_parent_pattern", page_is(page, PARENT_BYTE), "after-child-exit");
	int kr = kernel_read_faults(), kw = kernel_write_faults();
	snprintf(detail, sizeof detail, "read=%d,write=%d", kr, kw);
	record("vm_isolation_parent_kernel_address", kr && kw, detail);
out:;
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
