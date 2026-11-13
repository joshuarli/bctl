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

// Ported to plain C from gVisor test/syscalls/linux/socket_unix_cmsg.cc at commit
// 975fb172241c7946a9f67b4f52e0349d9240589a.

#include "harness.h"

#include <sys/mman.h>
#include <sys/socket.h>

#define SEND_LEN 20

static int socket_pair(int type, int sv[2])
{
	return socketpair(AF_UNIX, type, 0, sv);
}

static ssize_t send_with_control(int sock, void *control, size_t controllen, const char *buf, size_t len)
{
	struct msghdr msg;
	struct iovec iov;
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = controllen;
	iov.iov_base = (void *)buf;
	iov.iov_len = len;
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	return sendmsg(sock, &msg, 0);
}

static ssize_t send_fds(int sock, const int *fds, int count, const char *buf, size_t len)
{
	char control[CMSG_SPACE(3 * sizeof(int))];
	struct msghdr msg;
	struct cmsghdr *cmsg;
	struct iovec iov;
	memset(control, 0, sizeof control);
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = CMSG_SPACE(count * sizeof(int));
	cmsg = CMSG_FIRSTHDR(&msg);
	cmsg->cmsg_len = CMSG_LEN(count * sizeof(int));
	cmsg->cmsg_level = SOL_SOCKET;
	cmsg->cmsg_type = SCM_RIGHTS;
	memcpy(CMSG_DATA(cmsg), fds, count * sizeof(int));
	iov.iov_base = (void *)buf;
	iov.iov_len = len;
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	return sendmsg(sock, &msg, 0);
}

static int recv_fds(int sock, int *fds, int count, char *buf, size_t len)
{
	char control[CMSG_SPACE(3 * sizeof(int))];
	struct msghdr msg;
	struct cmsghdr *cmsg;
	struct iovec iov;
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = CMSG_SPACE(count * sizeof(int));
	iov.iov_base = buf;
	iov.iov_len = len;
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	if (recvmsg(sock, &msg, 0) != (ssize_t)len)
		return 0;
	cmsg = CMSG_FIRSTHDR(&msg);
	if (!cmsg || cmsg->cmsg_len != CMSG_LEN(count * sizeof(int)) || cmsg->cmsg_level != SOL_SOCKET ||
		cmsg->cmsg_type != SCM_RIGHTS)
		return 0;
	memcpy(fds, CMSG_DATA(cmsg), count * sizeof(int));
	return 1;
}

static int transfer_test(int fd1, int fd2)
{
	char buf1[SEND_LEN], buf2[SEND_LEN];
	fill_pattern(buf1, sizeof buf1, 1);
	if (write(fd1, buf1, sizeof buf1) != sizeof buf1)
		return 0;
	if (read(fd2, buf2, sizeof buf2) != sizeof buf2 || memcmp(buf1, buf2, sizeof buf1))
		return 0;
	fill_pattern(buf1, sizeof buf1, 2);
	if (write(fd2, buf1, sizeof buf1) != sizeof buf1)
		return 0;
	if (read(fd1, buf2, sizeof buf2) != sizeof buf2 || memcmp(buf1, buf2, sizeof buf1))
		return 0;
	return 1;
}

// socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicFDPass
static void basic_fd_pass(int type)
{
	int sv[2], pair[2], fd = -1;
	char sent[SEND_LEN], received[SEND_LEN];
	ASSERT(socket_pair(type, sv) == 0);
	ASSERT(socket_pair(SOCK_SEQPACKET, pair) == 0);
	fill_pattern(sent, sizeof sent, 3);
	ASSERT(send_fds(sv[0], &pair[1], 1, sent, sizeof sent) == (ssize_t)sizeof sent);
	ASSERT(recv_fds(sv[1], &fd, 1, received, sizeof received));
	EXPECT(memcmp(sent, received, sizeof sent) == 0);
	EXPECT(transfer_test(fd, pair[0]));
	close(fd);
	close(pair[0]);
	close(pair[1]);
	close(sv[0]);
	close(sv[1]);
}

// socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicThreeFDPass
static void basic_three_fd_pass(int type)
{
	int sv[2], pair1[2], pair2[2], pair3[2], received_fds[3] = {-1, -1, -1};
	int sent_fds[3];
	char sent[SEND_LEN], received[SEND_LEN];
	ASSERT(socket_pair(type, sv) == 0);
	ASSERT(socket_pair(SOCK_SEQPACKET, pair1) == 0);
	ASSERT(socket_pair(SOCK_SEQPACKET, pair2) == 0);
	ASSERT(socket_pair(SOCK_SEQPACKET, pair3) == 0);
	sent_fds[0] = pair1[1];
	sent_fds[1] = pair2[1];
	sent_fds[2] = pair3[1];
	fill_pattern(sent, sizeof sent, 4);
	ASSERT(send_fds(sv[0], sent_fds, 3, sent, sizeof sent) == (ssize_t)sizeof sent);
	ASSERT(recv_fds(sv[1], received_fds, 3, received, sizeof received));
	EXPECT(memcmp(sent, received, sizeof sent) == 0);
	EXPECT(transfer_test(received_fds[0], pair1[0]));
	EXPECT(transfer_test(received_fds[1], pair2[0]));
	EXPECT(transfer_test(received_fds[2], pair3[0]));
	for (int i = 0; i < 3; i++)
		close(received_fds[i]);
	close(pair1[0]);
	close(pair1[1]);
	close(pair2[0]);
	close(pair2[1]);
	close(pair3[0]);
	close(pair3[1]);
	close(sv[0]);
	close(sv[1]);
}

// socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BadFDPass
static void bad_fd_pass(int type)
{
	int sv[2];
	int bad_fd = -1;
	char sent[SEND_LEN];
	char control[CMSG_SPACE(sizeof(int))];
	struct msghdr msg;
	struct cmsghdr *cmsg;
	ASSERT(socket_pair(type, sv) == 0);
	fill_pattern(sent, sizeof sent, 5);
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = sizeof control;
	cmsg = CMSG_FIRSTHDR(&msg);
	cmsg->cmsg_len = CMSG_LEN(sizeof(bad_fd));
	cmsg->cmsg_level = SOL_SOCKET;
	cmsg->cmsg_type = SCM_RIGHTS;
	memcpy(CMSG_DATA(cmsg), &bad_fd, sizeof bad_fd);
	EXPECT(send_with_control(sv[0], control, sizeof control, sent, sizeof sent) == -1 && errno == EBADF);
	close(sv[0]);
	close(sv[1]);
}

// socket_unix_cmsg.cc:UnixSocketPairCmsgTest.ShortCmsg
static void short_cmsg(int type)
{
	int sv[2];
	int fd = -1;
	char sent[SEND_LEN];
	char control[CMSG_SPACE(sizeof(int))];
	struct msghdr msg;
	struct cmsghdr *cmsg;
	ASSERT(socket_pair(type, sv) == 0);
	fill_pattern(sent, sizeof sent, 6);
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = sizeof control;
	cmsg = CMSG_FIRSTHDR(&msg);
	cmsg->cmsg_len = 1;
	cmsg->cmsg_level = SOL_SOCKET;
	cmsg->cmsg_type = SCM_RIGHTS;
	memcpy(CMSG_DATA(cmsg), &fd, sizeof fd);
	EXPECT(send_with_control(sv[0], control, sizeof control, sent, sizeof sent) == -1 && errno == EINVAL);
	close(sv[0]);
	close(sv[1]);
}

// socket_unix_cmsg.cc:UnixSocketPairCmsgTest.FDPassRecvEFAULT
static void fd_pass_recv_efault(int type)
{
	int sv[2], pipe_fds[2];
	char sent[2] = {'a', 'b'};
	void *inaccessible;
	char received = 0;
	struct iovec iov[2];
	char control[CMSG_SPACE(sizeof(int))];
	struct msghdr msg;
	char c;
	ssize_t eof;
	ASSERT(socket_pair(type, sv) == 0);
	ASSERT(pipe2(pipe_fds, O_NONBLOCK) == 0);
	ASSERT(send_fds(sv[0], &pipe_fds[1], 1, sent, sizeof sent) == (ssize_t)sizeof sent);
	ASSERT(close(pipe_fds[1]) == 0);

	inaccessible = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	ASSERT(inaccessible != MAP_FAILED);
	iov[0].iov_base = &received;
	iov[0].iov_len = 1;
	iov[1].iov_base = inaccessible;
	iov[1].iov_len = 1;
	memset(control, 0, sizeof control);
	memset(&msg, 0, sizeof msg);
	msg.msg_iov = iov;
	msg.msg_iovlen = 2;
	msg.msg_control = control;
	msg.msg_controllen = sizeof control;
	EXPECT(recvmsg(sv[1], &msg, 0) == -1 && errno == EFAULT);

	close(sv[0]);
	close(sv[1]);
	eof = read(pipe_fds[0], &c, 1);
	EXPECT(eof == 0);
	munmap(inaccessible, 4096);
	close(pipe_fds[0]);
}

// socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicFDPassNoSpaceMsgCtrunc
static void basic_fd_pass_no_space_msg_ctrunc(int type)
{
	int sv[2], pair[2];
	char sent[SEND_LEN], received[SEND_LEN];
	char control[CMSG_SPACE(0)];
	struct msghdr msg;
	struct iovec iov;
	ASSERT(socket_pair(type, sv) == 0);
	ASSERT(socket_pair(SOCK_SEQPACKET, pair) == 0);
	fill_pattern(sent, sizeof sent, 7);
	ASSERT(send_fds(sv[0], &pair[1], 1, sent, sizeof sent) == (ssize_t)sizeof sent);
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = sizeof control;
	iov.iov_base = received;
	iov.iov_len = sizeof received;
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	EXPECT(recvmsg(sv[1], &msg, 0) == (ssize_t)sizeof received);
	EXPECT(msg.msg_controllen == 0);
	EXPECT(msg.msg_flags == MSG_CTRUNC);
	close(pair[0]);
	close(pair[1]);
	close(sv[0]);
	close(sv[1]);
}

// socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicThreeFDPassTruncationMsgCtrunc
static void basic_three_fd_pass_truncation_msg_ctrunc(int type)
{
	int sv[2], pair1[2], pair2[2], pair3[2], sent_fds[3];
	char sent[SEND_LEN], received[SEND_LEN];
	char control[CMSG_SPACE(2 * sizeof(int))];
	struct msghdr msg;
	struct iovec iov;
	struct cmsghdr *cmsg;
	ASSERT(socket_pair(type, sv) == 0);
	ASSERT(socket_pair(SOCK_SEQPACKET, pair1) == 0);
	ASSERT(socket_pair(SOCK_SEQPACKET, pair2) == 0);
	ASSERT(socket_pair(SOCK_SEQPACKET, pair3) == 0);
	sent_fds[0] = pair1[1];
	sent_fds[1] = pair2[1];
	sent_fds[2] = pair3[1];
	fill_pattern(sent, sizeof sent, 8);
	ASSERT(send_fds(sv[0], sent_fds, 3, sent, sizeof sent) == (ssize_t)sizeof sent);
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = sizeof control;
	iov.iov_base = received;
	iov.iov_len = sizeof received;
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	ASSERT(recvmsg(sv[1], &msg, 0) == (ssize_t)sizeof received);
	EXPECT(msg.msg_flags == MSG_CTRUNC);
	cmsg = CMSG_FIRSTHDR(&msg);
	ASSERT(cmsg != NULL);
	EXPECT(cmsg->cmsg_len == CMSG_LEN(2 * sizeof(int)));
	EXPECT(cmsg->cmsg_level == SOL_SOCKET);
	EXPECT(cmsg->cmsg_type == SCM_RIGHTS);
	{
		int got[2];
		memcpy(got, CMSG_DATA(cmsg), sizeof got);
		close(got[0]);
		close(got[1]);
	}
	close(pair1[0]);
	close(pair1[1]);
	close(pair2[0]);
	close(pair2[1]);
	close(pair3[0]);
	close(pair3[1]);
	close(sv[0]);
	close(sv[1]);
}

// socket_unix_cmsg.cc:UnixSocketPairCmsgTest.CloexecRecvFDPass
static void cloexec_recv_fd_pass(int type)
{
	int sv[2], pair[2], fd = -1;
	char sent[SEND_LEN], received[SEND_LEN];
	char control[CMSG_SPACE(sizeof(int))];
	struct msghdr msg;
	struct iovec iov;
	struct cmsghdr *cmsg;
	ASSERT(socket_pair(type, sv) == 0);
	ASSERT(socket_pair(SOCK_SEQPACKET, pair) == 0);
	fill_pattern(sent, sizeof sent, 9);
	ASSERT(send_fds(sv[0], &pair[1], 1, sent, sizeof sent) == (ssize_t)sizeof sent);
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = sizeof control;
	iov.iov_base = received;
	iov.iov_len = sizeof received;
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	ASSERT(recvmsg(sv[1], &msg, MSG_CMSG_CLOEXEC) == (ssize_t)sizeof received);
	cmsg = CMSG_FIRSTHDR(&msg);
	ASSERT(cmsg != NULL);
	ASSERT(cmsg->cmsg_len == CMSG_LEN(sizeof(int)));
	ASSERT(cmsg->cmsg_level == SOL_SOCKET);
	ASSERT(cmsg->cmsg_type == SCM_RIGHTS);
	memcpy(&fd, CMSG_DATA(cmsg), sizeof(int));
	EXPECT(fcntl(fd, F_GETFD) == FD_CLOEXEC);
	close(fd);
	close(pair[0]);
	close(pair[1]);
	close(sv[0]);
	close(sv[1]);
}

// socket_unix_cmsg.cc:UnixSocketPairCmsgTest.BasicCredPass
static void basic_cred_pass(int type)
{
	int sv[2], one = 1;
	char sent[SEND_LEN], received[SEND_LEN];
	struct ucred sent_creds, got;
	char control[CMSG_SPACE(sizeof(struct ucred))];
	struct msghdr msg;
	struct cmsghdr *cmsg;
	struct iovec iov;
	ASSERT(socket_pair(type, sv) == 0);
	fill_pattern(sent, sizeof sent, 10);
	sent_creds.pid = getpid();
	sent_creds.uid = getuid();
	sent_creds.gid = getgid();

	memset(control, 0, sizeof control);
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = sizeof control;
	cmsg = CMSG_FIRSTHDR(&msg);
	cmsg->cmsg_level = SOL_SOCKET;
	cmsg->cmsg_type = SCM_CREDENTIALS;
	cmsg->cmsg_len = CMSG_LEN(sizeof(struct ucred));
	memcpy(CMSG_DATA(cmsg), &sent_creds, sizeof sent_creds);
	ASSERT(send_with_control(sv[0], control, sizeof control, sent, sizeof sent) == (ssize_t)sizeof sent);

	ASSERT(setsockopt(sv[1], SOL_SOCKET, SO_PASSCRED, &one, sizeof one) == 0);

	memset(control, 0, sizeof control);
	memset(&msg, 0, sizeof msg);
	msg.msg_control = control;
	msg.msg_controllen = sizeof control;
	iov.iov_base = received;
	iov.iov_len = sizeof received;
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	ASSERT(recvmsg(sv[1], &msg, 0) == (ssize_t)sizeof received);
	cmsg = CMSG_FIRSTHDR(&msg);
	ASSERT(cmsg != NULL);
	ASSERT(cmsg->cmsg_len == CMSG_LEN(sizeof(struct ucred)));
	ASSERT(cmsg->cmsg_level == SOL_SOCKET);
	ASSERT(cmsg->cmsg_type == SCM_CREDENTIALS);
	memcpy(&got, CMSG_DATA(cmsg), sizeof got);
	EXPECT(memcmp(sent, received, sizeof sent) == 0);
	EXPECT(sent_creds.pid == got.pid);
	EXPECT(sent_creds.uid == got.uid);
	EXPECT(sent_creds.gid == got.gid);
	close(sv[0]);
	close(sv[1]);
}

#define BOTH_TYPES(fn)                                       \
	static void fn##_stream(void) { fn(SOCK_STREAM); }       \
	static void fn##_seqpacket(void) { fn(SOCK_SEQPACKET); }

BOTH_TYPES(basic_fd_pass)
BOTH_TYPES(basic_three_fd_pass)
BOTH_TYPES(bad_fd_pass)
BOTH_TYPES(short_cmsg)
BOTH_TYPES(fd_pass_recv_efault)
BOTH_TYPES(basic_fd_pass_no_space_msg_ctrunc)
BOTH_TYPES(basic_three_fd_pass_truncation_msg_ctrunc)
BOTH_TYPES(cloexec_recv_fd_pass)
BOTH_TYPES(basic_cred_pass)

#define PREFIX "gvisor_socket_unix_cmsg_UnixSocketPairCmsgTest_"
#define CASES(test, fn)                                      \
	{PREFIX test "_stream", fn##_stream},                \
	{PREFIX test "_seqpacket", fn##_seqpacket}

static const struct test_case cases[] = {
	CASES("BasicFDPass", basic_fd_pass),
	CASES("BasicThreeFDPass", basic_three_fd_pass),
	CASES("BadFDPass", bad_fd_pass),
	CASES("ShortCmsg", short_cmsg),
	CASES("FDPassRecvEFAULT", fd_pass_recv_efault),
	CASES("BasicFDPassNoSpaceMsgCtrunc", basic_fd_pass_no_space_msg_ctrunc),
	CASES("BasicThreeFDPassTruncationMsgCtrunc", basic_three_fd_pass_truncation_msg_ctrunc),
	CASES("CloexecRecvFDPass", cloexec_recv_fd_pass),
	CASES("BasicCredPass", basic_cred_pass),
};

int main(void)
{
	return run_cases(cases, (int)(sizeof cases / sizeof cases[0]));
}
