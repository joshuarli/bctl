#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>

static void *where(void *arg)
{
	long cpu = (long)arg;
	cpu_set_t set;
	CPU_ZERO(&set);
	CPU_SET((size_t)cpu, &set);
	if (sched_setaffinity(0, sizeof(set), &set))
		return (void *)-1L;
	long seen = -1;
	for (int i = 0; i < 1000 && seen != cpu; i++) {
		sched_yield();
		seen = sched_getcpu();
	}
	return (void *)seen;
}

int main(int argc, char **argv)
{
	long want = argc > 1 ? atol(argv[1]) : 0;
	cpu_set_t set;
	long online = sched_getaffinity(0, sizeof(set), &set) ? -1 : CPU_COUNT(&set);
	long ran = 0;
	for (long cpu = 0; cpu < online; cpu++) {
		pthread_t t;
		void *seen;
		if (pthread_create(&t, 0, where, (void *)cpu) || pthread_join(t, &seen))
			break;
		if ((long)seen == cpu)
			ran++;
	}
	int ok = want > 0 && online == want && ran == want;
	printf("MINI_CHROMIUM_TEST name=cpus_online status=%s detail=want=%ld,online=%ld,ran=%ld\n", ok ? "PASS" : "FAIL", want, online, ran);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
