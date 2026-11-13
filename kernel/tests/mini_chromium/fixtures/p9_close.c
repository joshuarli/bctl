#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void)
{
	alarm(30);
	int ok = 1;
	int i = 0;
	const char *operation = "read";
	for (; i < 200 && ok; i++) {
		operation = "read";
		char buf[32] = {0};
		int fd = open("/shared/from_host", O_RDONLY);
		ok = fd >= 0 && read(fd, buf, sizeof buf) == 20 && !memcmp(buf, "hello from the host\n", 20);
		if (fd >= 0)
			ok &= close(fd) == 0;
		if (!ok)
			break;
		operation = "stat";
		struct stat st;
		ok &= stat("/shared/from_host", &st) == 0 && st.st_size == 20;
		if (!ok)
			break;
		operation = "readdir";
		DIR *dir = opendir("/shared");
		int found = 0;
		struct dirent *e;
		while (dir && (e = readdir(dir)))
			found |= !strcmp(e->d_name, "from_host");
		if (dir)
			closedir(dir);
		ok &= found;
	}
	printf("MINI_CHROMIUM_TEST name=p9_close_reopen status=%s detail=iteration=%d,operation=%s,errno=%d\n",
	       ok ? "PASS" : "FAIL", i, operation, errno);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
