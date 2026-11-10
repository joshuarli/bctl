#include <sys/syscall.h>

typedef unsigned long u64;
typedef long s64;

#if defined(__aarch64__)
#define AUDIT_ARCH_NATIVE 0xc00000b7U
#elif defined(__x86_64__)
#define AUDIT_ARCH_NATIVE 0xc000003eU
#else
#error unsupported architecture
#endif

enum {
	PROT_READ = 1,
	PROT_WRITE = 2,
	MAP_PRIVATE = 2,
	MAP_SHARED = 1,
	MAP_ANONYMOUS = 0x20,
	SIGSEGV = 11,
	SIGKILL = 9,
	SIGCHLD = 17,
	ENOSYS = 38,
	EACCES = 13,
	EINVAL = 22,
	ECHILD = 10,
	PR_SET_NO_NEW_PRIVS = 38,
	PR_GET_NO_NEW_PRIVS = 39,
};

static s64 sys6(s64 n, u64 a, u64 b, u64 c, u64 d, u64 e, u64 f)
{
#if defined(__aarch64__)
	register u64 x0 __asm__("x0") = a;
	register u64 x1 __asm__("x1") = b;
	register u64 x2 __asm__("x2") = c;
	register u64 x3 __asm__("x3") = d;
	register u64 x4 __asm__("x4") = e;
	register u64 x5 __asm__("x5") = f;
	register s64 x8 __asm__("x8") = n;
	__asm__ volatile("svc 0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8) : "memory");
	return (s64)x0;
#elif defined(__x86_64__)
	s64 r;
	register u64 r10 __asm__("r10") = d;
	register u64 r8 __asm__("r8") = e;
	register u64 r9 __asm__("r9") = f;
	__asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9) : "rcx", "r11", "memory");
	return r;
#else
#error unsupported architecture
#endif
}

#define sys3(n, a, b, c) sys6((n), (u64)(a), (u64)(b), (u64)(c), 0, 0, 0)
#define sys4(n, a, b, c, d) sys6((n), (u64)(a), (u64)(b), (u64)(c), (u64)(d), 0, 0)

static s64 fork_raw(void)
{
#if defined(__aarch64__)
	return sys6(SYS_clone, SIGCHLD, 0, 0, 0, 0, 0);
#else
	return sys3(SYS_fork, 0, 0, 0);
#endif
}

static u64 slen(const char *s)
{
	u64 n = 0;
	while (s[n])
		n++;
	return n;
}

static void out(const char *s)
{
	sys3(SYS_write, 1, s, slen(s));
}

static void out_num(s64 v)
{
	char buf[24];
	int i = 23;
	int neg = v < 0;
	u64 u = neg ? (u64)-v : (u64)v;
	buf[i] = 0;
	do {
		buf[--i] = (char)('0' + u % 10);
		u /= 10;
	} while (u);
	if (neg)
		buf[--i] = '-';
	out(buf + i);
}

static int passed;
static int failed;

static void record(const char *name, int ok, const char *key, s64 value)
{
	out("MINI_CHROMIUM_TEST name=");
	out(name);
	out(ok ? " status=PASS" : " status=FAIL");
	if (key) {
		out(" detail=");
		out(key);
		out("=");
		out_num(value);
	}
	out("\n");
	if (ok)
		passed++;
	else
		failed++;
}

static int exited(int status, int code)
{
	return (status & 0x7f) == 0 && ((status >> 8) & 0xff) == code;
}

static int signaled(int status, int sig)
{
	return (status & 0x7f) == sig;
}

static s64 waitchild(s64 pid, int *status)
{
	return sys4(SYS_wait4, pid, status, 0, 0);
}

static char *self;
static char **envp_saved;

static void check_fork_wait(void)
{
	s64 parent = sys3(SYS_getpid, 0, 0, 0);
	s64 pid = fork_raw();
	if (pid == 0)
		sys3(SYS_exit_group, sys3(SYS_getppid, 0, 0, 0) == parent ? 42 : 43, 0, 0);
	int status = 0;
	s64 w = pid > 0 ? waitchild(pid, &status) : pid;
	record("raw_fork_wait", pid > 0 && w == pid && exited(status, 42), "fork", pid);
	s64 again = waitchild(-1, &status);
	record("raw_wait_echild", again == -ECHILD, "wait4", again);
}

static void check_exec(void)
{
	s64 pid = fork_raw();
	if (pid == 0) {
		char *argv[] = { self, "exit7", 0 };
		sys3(SYS_execve, self, argv, envp_saved);
		sys3(SYS_exit_group, 100, 0, 0);
	}
	int status = 0;
	s64 w = pid > 0 ? waitchild(pid, &status) : pid;
	record("raw_exec", pid > 0 && w == pid && exited(status, 7), "fork", pid);
}

static void check_cow_isolation(void)
{
	volatile u64 *page = (volatile u64 *)sys6(SYS_mmap, 0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, (u64)-1, 0);
	if ((s64)page < 0) {
		record("raw_cow", 0, "mmap", (s64)page);
		return;
	}
	*page = 0x1111;
	s64 pid = fork_raw();
	if (pid == 0) {
		int ok = *page == 0x1111;
		*page = 0x2222;
		for (int i = 0; i < 100; i++)
			sys3(SYS_sched_yield, 0, 0, 0);
		sys3(SYS_exit_group, ok && *page == 0x2222 ? 0 : 1, 0, 0);
	}
	int status = 0;
	if (pid > 0)
		waitchild(pid, &status);
	record("raw_cow", pid > 0 && exited(status, 0) && *page == 0x1111, "fork", pid);
}

static void check_mprotect(void)
{
	s64 pid = fork_raw();
	if (pid == 0) {
		volatile char *p = (volatile char *)sys6(SYS_mmap, 0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, (u64)-1, 0);
		p[0] = 1;
		sys3(SYS_mprotect, p, 4096, PROT_READ);
		p[0] = 2;
		sys3(SYS_exit_group, 0, 0, 0);
	}
	int status = 0;
	if (pid > 0)
		waitchild(pid, &status);
	record("raw_mprotect_denied", pid > 0 && signaled(status, SIGSEGV), "fork", pid);
}

struct sock_filter {
	unsigned short code;
	unsigned char jt;
	unsigned char jf;
	unsigned int k;
};

struct sock_fprog {
	unsigned short len;
	struct sock_filter *filter;
};

static void check_seccomp(void)
{
	s64 pid = fork_raw();
	if (pid == 0) {
		struct sock_filter filter[] = {
			{ 0x20, 0, 0, 4 },
			{ 0x15, 0, 3, AUDIT_ARCH_NATIVE },
			{ 0x20, 0, 0, 0 },
			{ 0x15, 0, 1, SYS_getppid },
			{ 0x06, 0, 0, 0x00050000U | EACCES },
			{ 0x06, 0, 0, 0x7fff0000U },
		};
		struct sock_fprog prog = { 6, filter };
		struct sock_filter bad[] = { { 0x05, 0, 0, 40 }, { 0x06, 0, 0, 0x7fff0000U } };
		struct sock_fprog badprog = { 2, bad };
		if (sys6(SYS_prctl, PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0, 0))
			sys3(SYS_exit_group, 2, 0, 0);
		if (sys3(SYS_seccomp, 1, 0, &badprog) != -EINVAL)
			sys3(SYS_exit_group, 3, 0, 0);
		if (sys3(SYS_seccomp, 1, 0, &prog))
			sys3(SYS_exit_group, 4, 0, 0);
		if (sys3(SYS_getppid, 0, 0, 0) != -EACCES)
			sys3(SYS_exit_group, 5, 0, 0);
		if (sys3(SYS_getpid, 0, 0, 0) <= 0)
			sys3(SYS_exit_group, 6, 0, 0);
		if (sys6(SYS_prctl, PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0, 0) != 1)
			sys3(SYS_exit_group, 7, 0, 0);
		sys3(SYS_exit_group, 0, 0, 0);
	}
	int status = 0;
	if (pid > 0)
		waitchild(pid, &status);
	record("raw_seccomp", pid > 0 && exited(status, 0), "status", status);
}

static int streq(const char *a, const char *b)
{
	while (*a && *a == *b)
		a++, b++;
	return *a == *b;
}

__attribute__((used)) void raw_main(u64 *sp)
{
	int argc = (int)sp[0];
	char **argv = (char **)(sp + 1);
	envp_saved = argv + argc + 1;
	self = argv[0][0] == '/' ? argv[0] : "/mini_chromium_raw";
	if (argc > 1 && streq(argv[1], "exit7"))
		sys3(SYS_exit_group, 7, 0, 0);
	check_fork_wait();
	check_exec();
	check_cow_isolation();
	check_mprotect();
	check_seccomp();
	out(failed ? "MINI_CHROMIUM_RESULT status=FAIL passed=" : "MINI_CHROMIUM_RESULT status=PASS passed=");
	out_num(passed);
	out(" failed=");
	out_num(failed);
	out("\n");
	sys3(SYS_exit_group, failed ? 1 : 0, 0, 0);
}

#if defined(__aarch64__)
__asm__(".globl _start\n_start:\n\tmov x0, sp\n\tbl raw_main\n\tbrk 0\n");
#else
__asm__(".globl _start\n_start:\n\tmov %rsp, %rdi\n\tand $-16, %rsp\n\tcall raw_main\n\thlt\n");
#endif
