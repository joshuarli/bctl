#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define ROUNDS 300
#define SPAN (256 * 1024)

static int finish(const char *detail, int ok)
{
	char line[256];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=file_read_into_cow_pages_while_mapped status=%s detail=%s\n",
		ok ? "PASS" : "FAIL", detail);
	write(1, line, (size_t)n);
	n = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	write(1, line, (size_t)n);
	return ok ? 0 : 1;
}

static atomic_int stop;
static int file;
static size_t span;
static unsigned char reference[SPAN];

static void *reader(void *arg)
{
	(void)arg;
	long page = sysconf(_SC_PAGESIZE);
	unsigned sum = 0;
	while (!atomic_load(&stop)) {
		volatile unsigned char *map = mmap(0, span, PROT_READ, MAP_PRIVATE, file, 0);
		if (map == MAP_FAILED)
			return (void *)1;
		for (size_t at = 0; at < span; at += (size_t)page)
			sum += map[at];
		munmap((void *)map, span);
	}
	return (void *)(uintptr_t)(sum & 0);
}

static int child(unsigned char *cow)
{
	pthread_t t;
	if (pthread_create(&t, 0, reader, 0))
		return 10;
	int rv = 0;
	for (size_t at = 0; at + span <= SPAN && !rv; at += span) {
		if (pread(file, cow + at, span, 0) != (ssize_t)span)
			rv = 11;
		else if (memcmp(cow + at, reference, span))
			rv = 12;
	}
	atomic_store(&stop, 1);
	void *mapped;
	pthread_join(t, &mapped);
	return rv ? rv : mapped ? 13 : 0;
}

int main(int argc, char **argv)
{
	(void)argc;
	char detail[128];
	file = open(argv[0], O_RDONLY);
	if (file < 0) {
		snprintf(detail, sizeof detail, "open-errno=%d", errno);
		return finish(detail, 0);
	}
	ssize_t got = pread(file, reference, SPAN, 0);
	if (got <= 0) {
		snprintf(detail, sizeof detail, "pread-errno=%d", errno);
		return finish(detail, 0);
	}
	span = (size_t)got;
	unsigned char *cow = mmap(0, SPAN, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (cow == MAP_FAILED) {
		snprintf(detail, sizeof detail, "mmap-errno=%d", errno);
		return finish(detail, 0);
	}
	for (int round = 0; round < ROUNDS; round++) {
		memset(cow, 0xee, SPAN);
		pid_t pid = fork();
		if (pid == 0)
			_exit(child(cow));
		int status;
		if (pid < 0 || waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status)) {
			snprintf(detail, sizeof detail, "round=%d child-status=0x%x", round, pid < 0 ? errno : status);
			return finish(detail, 0);
		}
	}
	snprintf(detail, sizeof detail, "rounds=%d span=%zu", ROUNDS, span);
	return finish(detail, 1);
}
