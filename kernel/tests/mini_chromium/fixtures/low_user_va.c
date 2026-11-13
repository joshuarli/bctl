#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static sigjmp_buf fault_jump;

static void on_fault(int sig)
{
	(void)sig;
	siglongjmp(fault_jump, 1);
}

int main(void)
{
	static const uintptr_t probes[] = { 0x10000, 0x100000, 0x1ff000, 0x387000, 0x3ff000, 0x400000 };
	volatile int passed = 0, failed = 0;
	signal(SIGSEGV, on_fault);
	for (size_t i = 0; i < sizeof probes / sizeof probes[0]; i++) {
		char line[160];
		void *volatile p = mmap((void *)probes[i], 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
		volatile int mapped = p == (void *)probes[i];
		volatile int ok = 0;
		if (mapped && !sigsetjmp(fault_jump, 1)) {
			*(volatile uint64_t *)p = 0x1234;
			ok = *(volatile uint64_t *)p == 0x1234;
		}
		int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=low_user_va_%lx status=%s detail=mapped=%d\n",
			(unsigned long)probes[i], ok ? "PASS" : "FAIL", mapped);
		write(1, line, (size_t)n);
		ok ? passed++ : failed++;
		if (mapped)
			munmap(p, 4096);
	}
	char line[96];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	write(1, line, (size_t)n);
	return failed ? 1 : 0;
}
