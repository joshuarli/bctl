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

// Ported to plain C from gVisor test/syscalls/linux/fork.cc at commit
// 975fb172241c7946a9f67b4f52e0349d9240589a.

#include "harness.h"

// fork.cc:ForkTest.Pipe
static void pipe_inherited_across_fork(void)
{
	int pipes[2];
	pid_t child;
	char val = -1;
	ASSERT(pipe(pipes) == 0);
	child = fork_tracked();
	ASSERT(child >= 0);
	if (child == 0) {
		char one = 1;
		CHILD_CHECK(write(pipes[1], &one, 1) == 1);
		_exit(0);
	}
	EXPECT(read_full(pipes[0], &val, 1) == 1);
	EXPECT(val == 1);
	EXPECT(wait_exit_code(child) == 0);
	close(pipes[0]);
	close(pipes[1]);
}

static const struct test_case cases[] = {
	{"gvisor_fork_ForkTest_Pipe", pipe_inherited_across_fork},
};

int main(void)
{
	return run_cases(cases, (int)(sizeof cases / sizeof cases[0]));
}
