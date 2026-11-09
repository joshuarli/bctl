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

// Ported to plain C from gVisor test/syscalls/linux/dup.cc at commit
// 975fb172241c7946a9f67b4f52e0349d9240589a.

#include "harness.h"

#include <sys/eventfd.h>

// dup.cc:DupTest.DupClearsCloExec
static void dup_clears_cloexec(void)
{
	int fd = eventfd(0, EFD_CLOEXEC);
	int nfd;
	ASSERT(fd >= 0);
	EXPECT(fcntl(fd, F_GETFD) == FD_CLOEXEC);
	nfd = dup(fd);
	ASSERT(nfd >= 0);
	EXPECT(fd != nfd);
	EXPECT(same_file(fd, nfd));
	EXPECT(fcntl(nfd, F_GETFD) == 0);
	close(nfd);
	close(fd);
}

// dup.cc:DupTest.Dup3
static void dup3_cloexec(void)
{
	int fd = open_scratch_file(O_RDONLY, 0);
	int nfd, again;
	ASSERT(fd >= 0);
	nfd = dup(fd);
	ASSERT(nfd >= 0);
	EXPECT(fd != nfd);
	EXPECT(same_file(fd, nfd));

	again = dup3(fd, nfd, 0);
	EXPECT(again == nfd);
	EXPECT(same_file(fd, again));
	EXPECT(fcntl(again, F_GETFD) == 0);

	again = dup3(fd, nfd, O_CLOEXEC);
	EXPECT(again == nfd);
	EXPECT(same_file(fd, again));
	EXPECT(fcntl(again, F_GETFD) == FD_CLOEXEC);
	close(nfd);
	close(fd);
}

static const struct test_case cases[] = {
	{"gvisor_dup_DupTest_DupClearsCloExec", dup_clears_cloexec},
	{"gvisor_dup_DupTest_Dup3", dup3_cloexec},
};

int main(void)
{
	return run_cases(cases, (int)(sizeof cases / sizeof cases[0]));
}
