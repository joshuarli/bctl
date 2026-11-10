#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define PAGE 4096
#define PAGES 8

static int finish(const char *detail, int ok)
{
	char line[256];
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=write_from_unaligned_file_mapping status=%s detail=%s\n",
		ok ? "PASS" : "FAIL", detail);
	write(1, line, (size_t)n);
	n = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	write(1, line, (size_t)n);
	return ok ? 0 : 1;
}

int main(void)
{
	char detail[128];
	static unsigned char page[PAGE];
	int src = open("/tmp/mapped-source", O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (src < 0) {
		snprintf(detail, sizeof detail, "open-source-errno=%d", errno);
		return finish(detail, 0);
	}
	for (int i = 0; i < PAGES; i++) {
		memset(page, 'a' + i, sizeof page);
		if (write(src, page, sizeof page) != (ssize_t)sizeof page) {
			snprintf(detail, sizeof detail, "fill-errno=%d", errno);
			return finish(detail, 0);
		}
	}
	unsigned char *map = mmap(0, PAGE * PAGES, PROT_READ, MAP_SHARED, src, 0);
	if (map == MAP_FAILED) {
		snprintf(detail, sizeof detail, "mmap-errno=%d", errno);
		return finish(detail, 0);
	}
	int dst = open("/tmp/mapped-copy", O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (dst < 0) {
		snprintf(detail, sizeof detail, "open-copy-errno=%d", errno);
		return finish(detail, 0);
	}
	size_t offset = PAGE / 2, length = PAGE * (PAGES - 2);
	ssize_t written = write(dst, map + offset, length);
	if (written != (ssize_t)length) {
		snprintf(detail, sizeof detail, "write=%zd errno=%d", written, errno);
		return finish(detail, 0);
	}
	static unsigned char back[PAGE * PAGES];
	if (pread(dst, back, length, 0) != (ssize_t)length || memcmp(back, map + offset, length)) {
		snprintf(detail, sizeof detail, "copy-differs");
		return finish(detail, 0);
	}
	snprintf(detail, sizeof detail, "offset=%zu,length=%zu", offset, length);
	return finish(detail, 1);
}
