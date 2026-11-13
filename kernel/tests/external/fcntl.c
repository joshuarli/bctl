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

// Ported to plain C from gVisor test/syscalls/linux/fcntl.cc at commit
// 975fb172241c7946a9f67b4f52e0349d9240589a.

#include "harness.h"

#include <sys/eventfd.h>
#include <sys/socket.h>

// fcntl.cc:FcntlTest.IndependentDescriptorFlags
static void independent_descriptor_flags(void)
{
	int fd = eventfd(0, 0);
	int newfd;
	ASSERT(fd >= 0);
	ASSERT(fcntl(fd, F_GETFD) == 0);
	newfd = dup(fd);
	ASSERT(newfd >= 0);
	ASSERT(fcntl(newfd, F_GETFD) == 0);
	ASSERT(fcntl(fd, F_SETFD, FD_CLOEXEC) == 0);
	ASSERT(fcntl(fd, F_GETFD) == FD_CLOEXEC);
	ASSERT(fcntl(newfd, F_GETFD) == 0);
	close(newfd);
	close(fd);
}

// fcntl.cc:FcntlTest.DupAfterO_ASYNC
static void dup_after_o_async(void)
{
	int s1 = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
	int before, fd2, after;
	ASSERT(s1 >= 0);
	before = fcntl(s1, F_GETFL);
	ASSERT(before >= 0);
	ASSERT(fcntl(s1, F_SETFL, before | O_ASYNC) == 0);
	fd2 = dup(s1);
	ASSERT(fd2 >= 0);
	after = fcntl(fd2, F_GETFL);
	ASSERT(after >= 0);
	EXPECT((after & O_ASYNC) == O_ASYNC);
	close(fd2);
	close(s1);
}

static const struct test_case cases[] = {
	{"gvisor_fcntl_FcntlTest_IndependentDescriptorFlags", independent_descriptor_flags},
	{"gvisor_fcntl_FcntlTest_DupAfterO_ASYNC", dup_after_o_async},
};

int main(void)
{
	return run_cases(cases, (int)(sizeof cases / sizeof cases[0]));
}
