#define _GNU_SOURCE
#include <limits.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define FUTEX_WAIT_PRIVATE 128
#define FUTEX_WAKE_PRIVATE 129
#define FUTEX_CMP_REQUEUE_PRIVATE 132
#define WAITERS 6
#define ROUNDS 30000

static atomic_int a, b, done;

static long futex(atomic_int *uaddr, int op, int val, long val2, atomic_int *uaddr2, int val3)
{
	return syscall(SYS_futex, uaddr, op, val, val2, uaddr2, val3);
}

/* Waits on a; a requeue moves it to b, where it may be woken at once. */
static void *waiter(void *unused)
{
	(void)unused;
	struct timespec limit = {0, 20 * 1000 * 1000};
	while (!atomic_load(&done)) {
		int seen = atomic_load(&a);
		futex(&a, FUTEX_WAIT_PRIVATE, seen, (long)&limit, 0, 0);
	}
	return 0;
}

/* Wakes b continuously so requeued waiters wake while the requeue is still finishing. */
static void *waker(void *unused)
{
	(void)unused;
	while (!atomic_load(&done))
		futex(&b, FUTEX_WAKE_PRIVATE, INT_MAX, 0, 0, 0);
	return 0;
}

int main(void)
{
	alarm(80);
	pthread_t threads[WAITERS + 1];
	int started = 0, ok = 1;
	for (; started < WAITERS; started++)
		if (pthread_create(&threads[started], 0, waiter, 0))
			ok = 0;
	if (pthread_create(&threads[started], 0, waker, 0))
		ok = 0;
	else
		started++;
	long requeued = 0;
	for (int i = 0; ok && i < ROUNDS; i++) {
		int value = atomic_fetch_add(&a, 1) + 1;
		long r = futex(&a, FUTEX_CMP_REQUEUE_PRIVATE, 0, INT_MAX, &b, value);
		if (r < 0)
			continue;
		requeued += r;
		if (!(i % 64))
			usleep(100);
	}
	atomic_store(&done, 1);
	futex(&a, FUTEX_WAKE_PRIVATE, INT_MAX, 0, 0, 0);
	futex(&b, FUTEX_WAKE_PRIVATE, INT_MAX, 0, 0, 0);
	for (int i = 0; i < started; i++)
		pthread_join(threads[i], 0);
	ok = ok && requeued > 0;
	printf("MINI_CHROMIUM_TEST name=futex_requeue_race status=%s detail=requeued%ld\n", ok ? "PASS" : "FAIL", requeued);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
