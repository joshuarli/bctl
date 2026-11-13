#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int wait_host(const char *phase, const char *ack)
{
	printf("p9-mutation: %s\n", phase);
	fflush(stdout);
	while (access(ack, F_OK))
		usleep(1000);
	return 0;
}

int main(void)
{
	setvbuf(stdout, 0, _IONBF, 0);
	alarm(30);
	for (int i = 0; i < 128; i++) {
		char path[80], byte;
		snprintf(path, sizeof path, "/shared/distinct-%d", i);
		int fd = open(path, O_RDONLY);
		if (fd < 0 || read(fd, &byte, 1) != 1 || byte != 'd' || close(fd))
			return 2;
	}
	wait_host("closed", "/shared/closed-ack");
	int fd = open("/shared/victim", O_RDWR);
	if (fd < 0 || pwrite(fd, "changed", 7, 0) != 7)
		return 3;
	wait_host("open", "/shared/open-ack");
	DIR *dir = opendir("/shared/.");
	if (!dir)
		return 4;
	while (readdir(dir)) {}
	closedir(dir);
	char buf[8] = {0};
	int ok = pread(fd, buf, 7, 0) == 7 && !memcmp(buf, "changed", 7) && close(fd) == 0;
	printf("MINI_CHROMIUM_TEST name=p9_host_delete_open status=%s detail=errno=%d\n", ok ? "PASS" : "FAIL", errno);
	int unlinked = 1, replaced = 1;
	for (int i = 0; i < 32 && unlinked && replaced; i++) {
		int removed = open("/shared/unlinked", O_CREAT | O_RDWR, 0600);
		unlinked = removed >= 0 && write(removed, "old", 3) == 3 && unlink("/shared/unlinked") == 0 &&
			pread(removed, buf, 3, 0) == 3 && !memcmp(buf, "old", 3) && close(removed) == 0;
		int target = open("/shared/target", O_CREAT | O_TRUNC | O_RDWR, 0600);
		int source = open("/shared/source", O_CREAT | O_RDWR, 0600);
		replaced = target >= 0 && source >= 0 && write(target, "old", 3) == 3 &&
			write(source, "new", 3) == 3 && close(source) == 0 &&
			rename("/shared/source", "/shared/target") == 0 && pread(target, buf, 3, 0) == 3 &&
			!memcmp(buf, "old", 3) && close(target) == 0;
		int replacement = open("/shared/target", O_RDONLY);
		replaced = replaced && replacement >= 0 && read(replacement, buf, 3) == 3 &&
			!memcmp(buf, "new", 3) && close(replacement) == 0;
	}
	printf("MINI_CHROMIUM_TEST name=p9_guest_unlink_open status=%s detail=cycles32_errno%d\n", unlinked ? "PASS" : "FAIL", errno);
	printf("MINI_CHROMIUM_TEST name=p9_rename_replace_open status=%s detail=cycles32_errno%d\n", replaced ? "PASS" : "FAIL", errno);
	int passed = ok + unlinked + replaced;
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", passed == 3 ? "PASS" : "FAIL", passed, 3 - passed);
	return passed != 3;
}
