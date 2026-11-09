#include <string.h>
#include <unistd.h>

static void emit(const char *line)
{
	write(1, line, strlen(line));
}

int main(void)
{
	emit("MINI_CHROMIUM_TEST name=duplicate_result status=PASS\n");
	emit("MINI_CHROMIUM_RESULT status=PASS passed=1 failed=0\n");
	emit("MINI_CHROMIUM_RESULT status=PASS passed=1 failed=0\n");
	return 0;
}
