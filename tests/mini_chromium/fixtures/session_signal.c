#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed, failed;

static void record(const char *name, int ok, const char *detail)
{
	printf("MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name, ok ? "PASS" : "FAIL", detail);
	fflush(stdout);
	if (ok)
		passed++;
	else
		failed++;
}

static int child_status(pid_t pid)
{
	int st = 0;
	return waitpid(pid, &st, 0) == pid ? st : -1;
}

static int exit_code(int st)
{
	return WIFEXITED(st) ? WEXITSTATUS(st) : 200 + (WIFSIGNALED(st) ? WTERMSIG(st) : 0);
}

static int child_setsid(void)
{
	pid_t self = getpid();
	if (setsid() != self)
		return 2;
	if (getsid(0) != self || getpgrp() != self || getpgid(0) != self)
		return 3;
	errno = 0;
	if (setsid() != -1 || errno != EPERM)
		return 4;
	errno = 0;
	if (setpgid(0, 0) != -1 || errno != EPERM)
		return 5;
	return 0;
}

static void check_setsid(void)
{
	char detail[64];
	pid_t parent_sid = getsid(0);
	pid_t pid = fork();
	if (pid == 0)
		_exit(child_setsid());
	int code = exit_code(child_status(pid));
	snprintf(detail, sizeof detail, "stage=%d,parent_sid_kept=%d", code, getsid(0) == parent_sid);
	record("session_setsid", code == 0 && getsid(0) == parent_sid, detail);
}

static void check_setpgid(void)
{
	char detail[96];
	int ready[2], hold[2];
	if (pipe(ready) || pipe(hold)) {
		record("session_setpgid", 0, "pipe-failed");
		return;
	}
	pid_t pid = fork();
	if (pid == 0) {
		char c;
		if (setpgid(0, 0) || getpgrp() != getpid())
			_exit(2);
		write(ready[1], "r", 1);
		read(hold[0], &c, 1);
		_exit(0);
	}
	char c;
	read(ready[0], &c, 1);
	int child_group = getpgid(pid) == pid;
	int same_session = getsid(pid) == getsid(0);
	errno = 0;
	int bad_group = setpgid(pid, 999999) == -1 && errno == EPERM;
	errno = 0;
	int not_child = setpgid(1, 0) == -1 && (errno == ESRCH || errno == EPERM);
	errno = 0;
	int negative = setpgid(-1, 0) == -1 && errno == EINVAL;
	errno = 0;
	int missing = getsid(999999) == -1 && errno == ESRCH;
	write(hold[1], "h", 1);
	int code = exit_code(child_status(pid));
	snprintf(detail, sizeof detail, "child_group=%d,same_session=%d,bad_group=%d,not_child=%d,negative=%d,missing=%d,code=%d",
		child_group, same_session, bad_group, not_child, negative, missing, code);
	record("session_setpgid", child_group && same_session && bad_group && not_child && negative && missing && code == 0,
		detail);
}

static void check_session_id_not_reused(void)
{
	char detail[96];
	int ready[2], hold[2];
	if (pipe(ready) || pipe(hold)) {
		record("session_id_not_reused", 0, "pipe-failed");
		return;
	}
	pid_t leader = fork();
	if (leader == 0) {
		if (setsid() != getpid())
			_exit(2);
		pid_t member = fork();
		if (member == 0) {
			char c;
			close(ready[0]);
			write(ready[1], "r", 1);
			read(hold[0], &c, 1);
			_exit(0);
		}
		_exit(member > 0 ? 0 : 3);
	}
	char c;
	close(ready[1]);
	int leader_code = exit_code(child_status(leader));
	read(ready[0], &c, 1);
	int reused = 0, setsid_failed = 0;
	for (int i = 0; i < 16; i++) {
		pid_t p = fork();
		if (p == 0)
			_exit(setsid() == getpid() ? 0 : 1);
		if (p == leader)
			reused++;
		if (exit_code(child_status(p)) != 0)
			setsid_failed++;
	}
	write(hold[1], "h", 1);
	snprintf(detail, sizeof detail, "leader_code=%d,reused=%d,setsid_failed=%d", leader_code, reused, setsid_failed);
	record("session_id_not_reused", leader_code == 0 && !reused && !setsid_failed, detail);
}

struct kernel_sigaction {
	void (*handler)(int);
	unsigned long flags;
	void (*restorer)(void);
	unsigned long mask;
};

static void on_usr1(int sig)
{
	(void)sig;
}

static int child_no_restorer_handler(void)
{
	struct kernel_sigaction ka = { on_usr1, 0, 0, 0 };
	if (syscall(SYS_rt_sigaction, SIGUSR1, &ka, 0, 8))
		return 2;
	kill(getpid(), SIGUSR1);
	return 3;
}

static void check_sigaction_without_restorer(void)
{
	char detail[96];
	struct kernel_sigaction dfl = { SIG_DFL, 0, 0, 0 }, ign = { SIG_IGN, 0, 0, 0 }, old;
	int set_dfl = syscall(SYS_rt_sigaction, SIGTERM, &dfl, 0, 8) == 0;
	int set_ign = syscall(SYS_rt_sigaction, SIGUSR2, &ign, &old, 8) == 0;
	int ignored = set_ign && kill(getpid(), SIGUSR2) == 0;
	syscall(SYS_rt_sigaction, SIGUSR2, &dfl, 0, 8);
	pid_t pid = fork();
	if (pid == 0)
		_exit(child_no_restorer_handler());
	int st = child_status(pid);
#if defined(__aarch64__)
	int expected_outcome = WIFEXITED(st) && WEXITSTATUS(st) == 3;
#else
	int expected_outcome = WIFSIGNALED(st) && WTERMSIG(st) == SIGSEGV;
#endif
	snprintf(detail, sizeof detail, "set_dfl=%d,set_ign=%d,ignored=%d,handler_status=0x%x", set_dfl, set_ign, ignored, st);
	record("sigaction_without_restorer", set_dfl && set_ign && ignored && expected_outcome, detail);
}

static volatile sig_atomic_t got_sig, got_code, got_tid;

static void on_info(int sig, siginfo_t *si, void *uc)
{
	(void)uc;
	got_sig = sig;
	got_code = si->si_code;
	got_tid = syscall(SYS_gettid);
}

static void catch_usr1(void)
{
	struct sigaction sa = { 0 };
	sa.sa_sigaction = on_info;
	sa.sa_flags = SA_SIGINFO;
	sigaction(SIGUSR1, &sa, 0);
	got_sig = got_code = got_tid = 0;
}

static int child_self_reraise(void)
{
	catch_usr1();
	siginfo_t si = { 0 };
	si.si_signo = SIGUSR1;
	si.si_code = SI_KERNEL;
	if (syscall(SYS_rt_tgsigqueueinfo, getpid(), syscall(SYS_gettid), SIGUSR1, &si))
		return 2;
	if (got_sig != SIGUSR1 || got_code != SI_KERNEL)
		return 3;
	catch_usr1();
	if (syscall(SYS_tkill, syscall(SYS_gettid), SIGUSR1))
		return 4;
	if (got_sig != SIGUSR1 || got_code != SI_TKILL)
		return 5;
	return 0;
}

static void check_self_reraise(void)
{
	char detail[64];
	pid_t pid = fork();
	if (pid == 0)
		_exit(child_self_reraise());
	int code = exit_code(child_status(pid));
	snprintf(detail, sizeof detail, "stage=%d", code);
	record("signal_self_reraise", code == 0, detail);
}

struct worker_pipes {
	int report, release;
};

static void *signalled_worker(void *arg)
{
	struct worker_pipes *w = arg;
	sigset_t mask;
	sigemptyset(&mask);
	sigaddset(&mask, SIGUSR1);
	pthread_sigmask(SIG_UNBLOCK, &mask, 0);
	pid_t tid = syscall(SYS_gettid);
	char c;
	write(w->report, &tid, sizeof tid);
	while (read(w->release, &c, 1) < 0 && errno == EINTR)
		;
	return (void *)(long)(got_sig == SIGUSR1 && got_tid == tid);
}

static int child_thread_target(int report, int release)
{
	struct worker_pipes w = { report, release };
	sigset_t mask;
	pthread_t th;
	void *hit;
	catch_usr1();
	sigemptyset(&mask);
	sigaddset(&mask, SIGUSR1);
	pthread_sigmask(SIG_BLOCK, &mask, 0);
	if (pthread_create(&th, 0, signalled_worker, &w))
		return 2;
	pthread_join(th, &hit);
	return hit ? 0 : 3;
}

static void check_thread_target(void)
{
	char detail[96];
	int report[2], release[2];
	if (pipe(report) || pipe(release)) {
		record("signal_thread_target", 0, "pipe-failed");
		return;
	}
	pid_t pid = fork();
	if (pid == 0)
		_exit(child_thread_target(report[1], release[0]));
	pid_t tid = 0;
	read(report[0], &tid, sizeof tid);
	int sent = tid > 0 && tid != pid && syscall(SYS_tkill, tid, SIGUSR1) == 0;
	write(release[1], "h", 1);
	int code = exit_code(child_status(pid));
	snprintf(detail, sizeof detail, "tid_distinct=%d,sent=%d,code=%d", tid > 0 && tid != pid, sent, code);
	record("signal_thread_target", sent && code == 0, detail);
}

static void check_cross_process(void)
{
	char detail[160];
	int report[2], release[2];
	if (pipe(report) || pipe(release)) {
		record("signal_cross_process", 0, "pipe-failed");
		return;
	}
	pid_t pid = fork();
	if (pid == 0) {
		catch_usr1();
		char c;
		write(report[1], "r", 1);
		while (read(release[0], &c, 1) < 0 && errno == EINTR)
			;
		_exit(got_sig == SIGUSR1 && got_code == SI_QUEUE ? 0 : 2);
	}
	char c;
	read(report[0], &c, 1);
	siginfo_t si = { 0 };
	si.si_signo = SIGUSR1;
	si.si_code = SI_KERNEL;
	errno = 0;
	int forged = syscall(SYS_rt_tgsigqueueinfo, pid, pid, SIGUSR1, &si) == -1 && errno == EPERM;
	errno = 0;
	int forged_proc = syscall(SYS_rt_sigqueueinfo, pid, SIGUSR1, &si) == -1 && errno == EPERM;
	si.si_code = SI_QUEUE;
	int queued = syscall(SYS_rt_tgsigqueueinfo, pid, pid, SIGUSR1, &si) == 0;
	errno = 0;
	int wrong_group = syscall(SYS_tgkill, getpid(), pid, 0) == -1 && errno == ESRCH;
	int tg_probe = syscall(SYS_tgkill, pid, pid, 0) == 0 ? 0 : errno;
	int t_probe = syscall(SYS_tkill, pid, 0) == 0 ? 0 : errno;
	write(release[1], "h", 1);
	int code = exit_code(child_status(pid));
	snprintf(detail, sizeof detail, "forged=%d,forged_proc=%d,queued=%d,wrong_group=%d,tgkill_errno=%d,tkill_errno=%d,code=%d",
		forged, forged_proc, queued, wrong_group, tg_probe, t_probe, code);
	record("signal_cross_process", forged && forged_proc && queued && wrong_group && !tg_probe && !t_probe && code == 0, detail);
}

static volatile int clear_tid_ready;

static void *park_with_unmapped_clear_tid(void *arg)
{
	void *page = arg;
	syscall(SYS_set_tid_address, page);
	munmap(page, 4096);
	clear_tid_ready = 1;
	for (;;)
		pause();
	return 0;
}

static int child_unmapped_clear_tid(void)
{
	void *page = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (page == MAP_FAILED)
		return 2;
	pthread_t th;
	if (pthread_create(&th, 0, park_with_unmapped_clear_tid, page))
		return 3;
	while (!clear_tid_ready)
		usleep(1000);
	return 0;
}

static void check_exit_unmapped_clear_tid(void)
{
	char detail[64];
	pid_t pid = fork();
	if (pid == 0)
		_exit(child_unmapped_clear_tid());
	int st = child_status(pid);
	snprintf(detail, sizeof detail, "status=0x%x", st);
	record("exit_unmapped_clear_tid", st == 0, detail);
}

struct robust_head {
	void *list;
	long futex_offset;
	void *pending;
};

static void *busy_robust_thread(void *arg)
{
	(void)arg;
	static __thread struct robust_head head;
	static __thread int lock_word;
	head.list = &head;
	head.futex_offset = (char *)&lock_word - (char *)&head;
	head.pending = 0;
	syscall(SYS_set_robust_list, &head, sizeof head);
	for (;;)
		sched_yield();
	return 0;
}

static int child_group_exit_many(void)
{
	pthread_t th;
	for (int i = 0; i < 12; i++)
		if (pthread_create(&th, 0, busy_robust_thread, 0))
			return 2;
	usleep(20000);
	return 0;
}

static void check_group_exit_many_threads(void)
{
	char detail[64];
	int bad = 0, last = 0;
	for (int i = 0; i < 20; i++) {
		pid_t pid = fork();
		if (pid == 0)
			_exit(child_group_exit_many());
		int st = child_status(pid);
		if (st) {
			bad++;
			last = st;
		}
	}
	snprintf(detail, sizeof detail, "bad=%d,last=0x%x", bad, last);
	record("group_exit_many_threads", bad == 0, detail);
}

static void check_sched_queries(void)
{
	char detail[128];
	int policy = -1;
	struct sched_param sp = { .sched_priority = -1 };
	int self = pthread_getschedparam(pthread_self(), &policy, &sp);
	int sched = (int)syscall(SYS_sched_getscheduler, 0);
	errno = 0;
	int missing = syscall(SYS_sched_getparam, 999999, &sp) == -1 && errno == ESRCH;
	errno = 0;
	int bad_policy = sched_get_priority_max(42) == -1 && errno == EINVAL;
	int ranges = sched_get_priority_max(SCHED_FIFO) == 99 && sched_get_priority_min(SCHED_FIFO) == 1 &&
		sched_get_priority_max(SCHED_OTHER) == 0 && sched_get_priority_min(SCHED_OTHER) == 0;
	snprintf(detail, sizeof detail, "self=%d,policy=%d,prio=%d,sched=%d,missing=%d,bad_policy=%d,ranges=%d", self,
		policy, sp.sched_priority, sched, missing, bad_policy, ranges);
	record("sched_queries", !self && policy == SCHED_OTHER && sp.sched_priority == 0 && sched == SCHED_OTHER && missing &&
		bad_policy && ranges, detail);
}

int main(void)
{
	check_setsid();
	check_setpgid();
	check_session_id_not_reused();
	check_sigaction_without_restorer();
	check_self_reraise();
	check_cross_process();
	check_thread_target();
	check_exit_unmapped_clear_tid();
	check_group_exit_many_threads();
	check_sched_queries();
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS", passed, failed);
	return failed ? 1 : 0;
}
