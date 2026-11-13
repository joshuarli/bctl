#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/uio.h>
#include <unistd.h>

static int passed, failed;

static void record(const char *name, int ok, long got, long want)
{
	char line[160];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s detail=got=%ld,want=%ld\n", name,
		ok ? "PASS" : "FAIL", got, want);
	write(1, line, (size_t)n);
	ok ? passed++ : failed++;
}

static void console_case(const char *name, struct iovec *iov, int n, long want)
{
	long got = writev(1, iov, n);
	record(name, got == want, got, want);
}

static void pipe_case(const char *name, struct iovec *iov, int n, const char *want)
{
	int p[2];
	char buf[64] = "";
	long got = -1;
	if (!pipe(p)) {
		got = writev(p[1], iov, n);
		close(p[1]);
		long r = read(p[0], buf, sizeof buf - 1);
		buf[r > 0 ? r : 0] = 0;
		close(p[0]);
	}
	long want_len = (long)strlen(want);
	record(name, got == want_len && !strcmp(buf, want), got, want_len);
}

int main(void)
{
	struct iovec two[] = { { "WV1 ", 4 }, { "WV2\n", 4 } };
	struct iovec empty_last[] = { { "WV3\n", 4 }, { 0, 0 } };
	struct iovec empty_first[] = { { 0, 0 }, { "WV4\n", 4 } };
	struct iovec three[] = { { "a", 1 }, { "bc", 2 }, { "def", 3 } };
	struct iovec gap[] = { { "x", 1 }, { 0, 0 }, { "yz", 2 } };
	console_case("console_two_iovecs", two, 2, 8);
	console_case("console_empty_last", empty_last, 2, 4);
	console_case("console_empty_first", empty_first, 2, 4);
	pipe_case("pipe_three_iovecs", three, 3, "abcdef");
	pipe_case("pipe_empty_middle", gap, 3, "xyz");
	pipe_case("pipe_empty_last", empty_last, 2, "WV3\n");

	int p[2];
	char a[3] = "", b[4] = "";
	struct iovec in[] = { { a, 2 }, { b, 3 } };
	long got = -1;
	if (!pipe(p)) {
		write(p[1], "12345", 5);
		got = readv(p[0], in, 2);
		close(p[0]);
		close(p[1]);
	}
	record("pipe_readv_two_iovecs", got == 5 && !memcmp(a, "12", 2) && !memcmp(b, "345", 3), got, 5);

	printf("stdio flushes through writev\n");
	fflush(stdout);
	record("stdio_flush_returns", 1, 0, 0);
	char line[96];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	write(1, line, (size_t)n);
	return failed ? 1 : 0;
}
