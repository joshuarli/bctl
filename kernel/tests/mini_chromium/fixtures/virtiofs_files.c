#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* Expects a virtio-fs share mounted at /shared that holds from_host ("hello from the host\n") and an
 * empty directory host_dir. Leaves /shared/from_guest truncated to TRUNCATED bytes of the pattern. */

#define SHARE "/shared"
#define BIG (1200 * 1024)
#define TRUNCATED 1000

static int passed, failed;

static void report(const char *name, int ok, const char *detail)
{
	printf("MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name, ok ? "PASS" : "FAIL", detail);
	fflush(stdout);
	if (ok)
		passed++;
	else
		failed++;
}

static unsigned char pattern(size_t i)
{
	return (unsigned char)(i * 7 + i / 4096);
}

static void host_file_read(void)
{
	char buf[64] = {0}, detail[96];
	int fd = open(SHARE "/from_host", O_RDONLY);
	ssize_t n = fd < 0 ? -1 : read(fd, buf, sizeof buf - 1);
	snprintf(detail, sizeof detail, "read=%zd errno=%d", n, errno);
	report("virtiofs_host_file_read", n == 20 && !strcmp(buf, "hello from the host\n"), detail);
	if (fd >= 0)
		close(fd);
}

static void listing(void)
{
	DIR *d = opendir(SHARE);
	int file = 0, dir = 0;
	struct dirent *e;
	while (d && (e = readdir(d))) {
		file |= !strcmp(e->d_name, "from_host");
		dir |= !strcmp(e->d_name, "host_dir") && e->d_type == DT_DIR;
	}
	if (d)
		closedir(d);
	char detail[64];
	snprintf(detail, sizeof detail, "from_host=%d host_dir=%d", file, dir);
	report("virtiofs_listing", file && dir, detail);
}

static void write_read_back(void)
{
	char detail[96];
	unsigned char *out = malloc(BIG), *in = malloc(BIG);
	for (size_t i = 0; i < BIG; i++)
		out[i] = pattern(i);
	int fd = open(SHARE "/from_guest", O_RDWR | O_CREAT | O_TRUNC, 0644);
	ssize_t w = fd < 0 ? -1 : write(fd, out, BIG);
	int synced = fd >= 0 && fsync(fd) == 0;
	if (fd >= 0)
		close(fd);
	fd = open(SHARE "/from_guest", O_RDONLY);
	ssize_t r = 0, n;
	while (fd >= 0 && r < BIG && (n = read(fd, in + r, BIG - r)) > 0)
		r += n;
	struct stat st;
	int stated = fd >= 0 && fstat(fd, &st) == 0;
	snprintf(detail, sizeof detail, "write=%zd read=%zd fsync=%d size=%lld errno=%d", w, r, synced,
		 stated ? (long long)st.st_size : -1LL, errno);
	report("virtiofs_write_read_back",
	       w == BIG && r == BIG && synced && stated && st.st_size == BIG && !memcmp(out, in, BIG), detail);
	if (fd >= 0)
		close(fd);
	free(out);
	free(in);
}

static void tree_operations(void)
{
	char detail[96];
	int mk = mkdir(SHARE "/gdir", 0755);
	int fd = open(SHARE "/gdir/a", O_WRONLY | O_CREAT, 0644);
	if (fd >= 0)
		close(fd);
	int mv = rename(SHARE "/gdir/a", SHARE "/gdir/b");
	struct stat st;
	int a_gone = stat(SHARE "/gdir/a", &st) != 0 && errno == ENOENT;
	int b_there = stat(SHARE "/gdir/b", &st) == 0;
	int rm = unlink(SHARE "/gdir/b");
	int rd = rmdir(SHARE "/gdir");
	int dir_gone = stat(SHARE "/gdir", &st) != 0;
	snprintf(detail, sizeof detail, "mkdir=%d create=%d rename=%d a_gone=%d b=%d unlink=%d rmdir=%d gone=%d", mk,
		 fd >= 0, mv, a_gone, b_there, rm, rd, dir_gone);
	report("virtiofs_tree_operations", !mk && fd >= 0 && !mv && a_gone && b_there && !rm && !rd && dir_gone, detail);
}

static void truncate_file(void)
{
	char detail[64];
	int t = truncate(SHARE "/from_guest", TRUNCATED);
	struct stat st;
	int s = stat(SHARE "/from_guest", &st);
	snprintf(detail, sizeof detail, "truncate=%d size=%lld", t, s ? -1LL : (long long)st.st_size);
	report("virtiofs_truncate", !t && !s && st.st_size == TRUNCATED, detail);
}

int main(void)
{
	host_file_read();
	listing();
	write_read_back();
	tree_operations();
	truncate_file();
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	return failed ? 1 : 0;
}
