// Copyright 2018 The gVisor Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// Ported to plain C from gVisor test/syscalls/linux/wait.cc at commit
// 975fb172241c7946a9f67b4f52e0349d9240589a.

#include "harness.h"

// wait.cc:WaitTest.SignalExit
// The child blocks on a pipe instead of sleeping for ten seconds.
static void signal_exit(void)
{
	int hold[2];
	pid_t child;
	int status = 0;
	ASSERT(pipe(hold) == 0);
	child = fork_tracked();
	ASSERT(child >= 0);
	if (child == 0) {
		char c;
		close(hold[1]);
		read(hold[0], &c, 1);
		_exit(0);
	}
	close(hold[0]);
	EXPECT(kill(child, SIGKILL) == 0);
	EXPECT(wait_tracked(child, &status) == child);
	EXPECT(!WIFEXITED(status));
	EXPECT(WIFSIGNALED(status));
	EXPECT(WTERMSIG(status) == SIGKILL);
	close(hold[1]);
}

// wait.cc:WaitTest.WaitidWNOWAIT
static void waitid_wnowait(void)
{
	pid_t child = fork_tracked();
	siginfo_t info;
	int rc;
	ASSERT(child >= 0);
	if (child == 0)
		_exit(42);

	memset(&info, 0, sizeof info);
	ASSERT(waitid(P_PID, (id_t)child, &info, WEXITED | WNOWAIT) == 0);
	EXPECT(info.si_pid == child);
	EXPECT(info.si_signo == SIGCHLD);
	EXPECT(info.si_code == CLD_EXITED);
	EXPECT(info.si_status == 42);

	memset(&info, 0, sizeof info);
	ASSERT(waitid(P_PID, (id_t)child, &info, WEXITED) == 0);
	untrack_child(child);
	EXPECT(info.si_pid == child);
	EXPECT(info.si_signo == SIGCHLD);
	EXPECT(info.si_code == CLD_EXITED);
	EXPECT(info.si_status == 42);

	rc = waitid(P_PID, (id_t)child, &info, WEXITED);
	EXPECT(rc == -1 && errno == ECHILD);
}

static const struct test_case cases[] = {
	{"gvisor_wait_WaitTest_SignalExit", signal_exit},
	{"gvisor_wait_WaitTest_WaitidWNOWAIT", waitid_wnowait},
};

int main(void)
{
	return run_cases(cases, (int)(sizeof cases / sizeof cases[0]));
}
