// Copyright 2025 The gVisor Authors.
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

// Ported to plain C from gVisor test/syscalls/linux/socket_unix_peercred.cc at commit
// 975fb172241c7946a9f67b4f52e0349d9240589a.

#include "harness.h"

#include <stddef.h>
#include <sys/socket.h>
#include <sys/un.h>

static int unique_counter;

static struct sockaddr_un unique_abstract_addr(void)
{
	struct sockaddr_un addr;
	memset(&addr, 0, sizeof addr);
	addr.sun_family = AF_UNIX;
	snprintf(addr.sun_path + 1, sizeof addr.sun_path - 1, "external-peercred-%d-%d", (int)getpid(),
		unique_counter++);
	return addr;
}

static int cred_equals(struct ucred c, pid_t pid, uid_t uid, gid_t gid)
{
	return c.pid == pid && c.uid == uid && c.gid == gid;
}

// socket_unix_peercred.cc:UnixSocketPeerCredTest.GetPeerCred
static void get_peer_cred(int type)
{
	int sv[2];
	struct ucred cred;
	socklen_t len = sizeof cred;
	ASSERT(socketpair(AF_UNIX, type, 0, sv) == 0);
	ASSERT(getsockopt(sv[0], SOL_SOCKET, SO_PEERCRED, &cred, &len) == 0);
	EXPECT(cred_equals(cred, getpid(), getuid(), getgid()));
	close(sv[0]);
	close(sv[1]);
}

static void get_peer_cred_stream(void) { get_peer_cred(SOCK_STREAM); }
static void get_peer_cred_seqpacket(void) { get_peer_cred(SOCK_SEQPACKET); }

// socket_unix_peercred.cc:UnixSocketPeerCredTest.PeerCredSurvivesPeerExit
static void peer_cred_survives_peer_exit(void)
{
	int pipe_fd[2];
	struct sockaddr_un addr = unique_abstract_addr();
	int server, accepted;
	pid_t pid;
	char ok = 1;
	struct ucred cred;
	socklen_t len = sizeof cred;

	ASSERT(pipe(pipe_fd) == 0);
	server = socket(AF_UNIX, SOCK_STREAM, 0);
	ASSERT(server >= 0);
	ASSERT(bind(server, (struct sockaddr *)&addr, sizeof addr) == 0);
	ASSERT(listen(server, 5) == 0);

	pid = fork_tracked();
	ASSERT(pid >= 0);
	if (pid == 0) {
		char c = 0;
		int client;
		CHILD_CHECK(close(pipe_fd[1]) == 0);
		client = socket(AF_UNIX, SOCK_STREAM, 0);
		CHILD_CHECK(client >= 0);
		CHILD_CHECK(connect(client, (struct sockaddr *)&addr, sizeof addr) == 0);
		CHILD_CHECK(read(pipe_fd[0], &c, 1) == 1);
		_exit(0);
	}
	close(pipe_fd[0]);

	accepted = accept(server, NULL, NULL);
	ASSERT(accepted >= 0);
	ASSERT(write(pipe_fd[1], &ok, 1) == 1);
	close(pipe_fd[1]);
	EXPECT(wait_exit_code(pid) == 0);

	ASSERT(getsockopt(accepted, SOL_SOCKET, SO_PEERCRED, &cred, &len) == 0);
	EXPECT(cred_equals(cred, pid, getuid(), getgid()));
	close(accepted);
	close(server);
}

static const struct test_case cases[] = {
	{"gvisor_socket_unix_peercred_UnixSocketPeerCredTest_GetPeerCred_stream", get_peer_cred_stream},
	{"gvisor_socket_unix_peercred_UnixSocketPeerCredTest_GetPeerCred_seqpacket", get_peer_cred_seqpacket},
	{"gvisor_socket_unix_peercred_UnixSocketPeerCredTest_PeerCredSurvivesPeerExit",
		peer_cred_survives_peer_exit},
};

int main(void)
{
	return run_cases(cases, (int)(sizeof cases / sizeof cases[0]));
}
