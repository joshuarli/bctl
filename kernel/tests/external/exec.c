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

// Ported to plain C from gVisor test/syscalls/linux/exec.cc at commit
// 975fb172241c7946a9f67b4f52e0349d9240589a.

#include "harness.h"

#include <sys/eventfd.h>

static const char *self_path;
static const char assert_closed_flag[] = "--assert-closed-workload";

// exec_assert_closed_workload.cc:main
static int assert_closed_workload(const char *fd_arg)
{
	char *end;
	struct stat s;
	long fd = strtol(fd_arg, &end, 10);
	if (*end || end == fd_arg) {
		dprintf(2, "fd: %s could not be parsed\n", fd_arg);
		return 1;
	}
	if (fstat((int)fd, &s) == 0) {
		dprintf(2, "fd: %s should not be valid\n", fd_arg);
		return 2;
	}
	if (errno != EBADF) {
		dprintf(2, "fstat fd: %s got errno: %d wanted: %d\n", fd_arg, errno, EBADF);
		return 1;
	}
	return 0;
}

// exec.cc:CheckExecHelper (stderr captured through a pipe, stdin and stdout on /dev/null)
static void check_exec(int fd_to_probe, int want_exit, int *ok)
{
	int pipe_fds[2];
	pid_t child;
	char fd_text[16];
	char buf[128];
	ssize_t n;
	*ok = 0;
	if (pipe2(pipe_fds, O_CLOEXEC) != 0)
		return;
	snprintf(fd_text, sizeof fd_text, "%d", fd_to_probe);
	child = fork_tracked();
	if (child < 0) {
		close(pipe_fds[0]);
		close(pipe_fds[1]);
		return;
	}
	if (child == 0) {
		char *argv[] = {(char *)self_path, (char *)assert_closed_flag, fd_text, NULL};
		char *envp[] = {NULL};
		int devnull = open("/dev/null", O_RDWR | O_CLOEXEC);
		if (devnull < 0 || dup2(devnull, 0) < 0 || dup2(devnull, 1) < 0 || dup2(pipe_fds[1], 2) < 0)
			_exit(errno);
		execve(self_path, argv, envp);
		_exit(126);
	}
	close(pipe_fds[1]);
	while ((n = read(pipe_fds[0], buf, sizeof buf)) > 0 || (n < 0 && errno == EINTR))
		;
	close(pipe_fds[0]);
	*ok = wait_exit_code(child) == want_exit;
}

// exec.cc:ExecTest.CloexecNormalFile
static void cloexec_normal_file(void)
{
	int closed_on_exec = open_scratch_file(O_RDONLY | O_CLOEXEC, 1);
	int open_on_exec = open_scratch_file(O_RDONLY, 1);
	int ok;
	ASSERT(closed_on_exec >= 0);
	ASSERT(open_on_exec >= 0);
	check_exec(closed_on_exec, 0, &ok);
	EXPECT(ok);
	check_exec(open_on_exec, 2, &ok);
	EXPECT(ok);
	close(open_on_exec);
	close(closed_on_exec);
}

static const struct test_case cases[] = {
	{"gvisor_exec_ExecTest_CloexecNormalFile", cloexec_normal_file},
};

int main(int argc, char **argv)
{
	if (argc == 3 && strcmp(argv[1], assert_closed_flag) == 0)
		return assert_closed_workload(argv[2]);
	self_path = argv[0];
	return run_cases(cases, (int)(sizeof cases / sizeof cases[0]));
}
