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

// Ported to plain C from gVisor test/syscalls/linux/lseek.cc at commit
// 975fb172241c7946a9f67b4f52e0349d9240589a.

#include "harness.h"

// lseek.cc:LseekTest.EtcPasswdDup
// The scratch file replaces /etc/passwd so the case does not depend on a guest filesystem layout.
static void dup_shares_offset(void)
{
	int fd1 = open_scratch_file(O_RDONLY, 4096);
	int fd2, fd3;
	ASSERT(fd1 >= 0);
	fd2 = dup(fd1);
	ASSERT(fd2 >= 0);

	ASSERT(lseek(fd1, 0, SEEK_CUR) == 0);
	ASSERT(lseek(fd2, 0, SEEK_CUR) == 0);
	ASSERT(lseek(fd1, 1000, SEEK_CUR) >= 0);
	ASSERT(lseek(fd2, 0, SEEK_CUR) == 1000);

	fd3 = dup(fd1);
	ASSERT(fd3 >= 0);
	ASSERT(lseek(fd3, 0, SEEK_CUR) == 1000);
	close(fd3);
	close(fd2);
	close(fd1);
}

static const struct test_case cases[] = {
	{"gvisor_lseek_LseekTest_EtcPasswdDup", dup_shares_offset},
};

int main(void)
{
	return run_cases(cases, (int)(sizeof cases / sizeof cases[0]));
}
