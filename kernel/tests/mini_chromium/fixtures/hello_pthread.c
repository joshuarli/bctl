#include <pthread.h>
#include <stdio.h>

static void *square(void *arg)
{
	long n = (long)arg;
	return (void *)(n * n);
}

int main(void)
{
	pthread_t threads[4];
	void *results[4];
	long sum = 0;
	for (long i = 0; i < 4; i++)
		if (pthread_create(&threads[i], 0, square, (void *)(i + 1)))
			return 1;
	for (int i = 0; i < 4; i++) {
		pthread_join(threads[i], &results[i]);
		sum += (long)results[i];
	}
	int ok = sum == 30;
	printf("MINI_CHROMIUM_TEST name=hello_pthread status=%s detail=sum=%ld\n", ok ? "PASS" : "FAIL", sum);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
