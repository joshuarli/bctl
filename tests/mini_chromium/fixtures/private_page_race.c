#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <unistd.h>

#define THREADS 4
#define PAGES 2048
#define PAGE 4096

static volatile unsigned char data[PAGES * PAGE] = { [0 ... PAGES * PAGE - 1] = 17 };
static pthread_barrier_t barrier;

static void *reader(void *arg)
{
	(void)arg;
	pthread_barrier_wait(&barrier);
	for (size_t i = 0; i < PAGES; i++)
		if (data[i * PAGE] != 17)
			return (void *)1;
	return 0;
}

int main(void)
{
	pthread_t threads[THREADS];
	int bad = 0, started = 0;
	if (!pthread_barrier_init(&barrier, 0, THREADS))
		for (; started < THREADS; started++)
			if (pthread_create(&threads[started], 0, reader, 0))
				break;
	if (started < THREADS)
		bad = 1;
	for (int i = 0; i < started; i++) {
		void *value;
		if (pthread_join(threads[i], &value) || value)
			bad = 1;
	}
	if (started < THREADS)
		_exit(1);
	printf("MINI_CHROMIUM_TEST name=private_page_race status=%s detail=threads=%d,pages=%d\n",
		bad ? "FAIL" : "PASS", THREADS, PAGES);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", bad ? "FAIL" : "PASS", !bad, bad);
	return bad;
}
