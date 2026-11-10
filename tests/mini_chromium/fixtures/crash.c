#include <string.h>
#include <unistd.h>

int main(void)
{
	static const char line[] = "MINI_CHROMIUM_TEST name=crash_reached status=PASS\n";
	write(1, line, strlen(line));
	volatile int *volatile fault = (volatile int *)16;
	*fault = 1;
	return 0;
}
