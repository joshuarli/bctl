#define _GNU_SOURCE
#pragma GCC diagnostic ignored "-Wsign-compare"
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#define KMAX 4

static int passed, failed;

static void record(const char *name, int ok, const char *fmt, ...)
{
	char detail[256], line[512];
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(detail, sizeof detail, fmt, ap);
	va_end(ap);
	int n = snprintf(line, sizeof line, "MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", name, ok ? "PASS" : "FAIL",
		detail);
	write(1, line, (size_t)n);
	ok ? passed++ : failed++;
}

struct walk {
	int ncred;
	int nrights;
	int nfds;
	int fds[KMAX];
	struct ucred cred;
	int has_cred;
	size_t sum_space;
	int bad_level;
	int bad_type;
	int bad;
};

static void walk_headers(struct msghdr *msg, struct walk *w)
{
	memset(w, 0, sizeof *w);
	w->bad_level = -1;
	w->bad_type = -1;
	for (struct cmsghdr *cm = CMSG_FIRSTHDR(msg); cm; cm = CMSG_NXTHDR(msg, cm)) {
		if (!(cm->cmsg_level == SOL_SOCKET && (cm->cmsg_type == SCM_RIGHTS || cm->cmsg_type == SCM_CREDENTIALS))) {
			w->bad = 1;
			w->bad_level = cm->cmsg_level;
			w->bad_type = cm->cmsg_type;
			continue;
		}
		size_t data_len = cm->cmsg_len - CMSG_LEN(0);
		w->sum_space += CMSG_SPACE(data_len);
		if (cm->cmsg_type == SCM_CREDENTIALS) {
			w->ncred++;
			if (data_len == sizeof(struct ucred)) {
				memcpy(&w->cred, CMSG_DATA(cm), sizeof w->cred);
				w->has_cred = 1;
			}
		} else {
			w->nrights++;
			if (data_len % sizeof(int) == 0) {
				int cnt = (int)(data_len / sizeof(int));
				for (int i = 0; i < cnt && w->nfds < KMAX; i++)
					memcpy(&w->fds[w->nfds++], CMSG_DATA(cm) + sizeof(int) * (size_t)i, sizeof(int));
			}
		}
	}
}

static int make_pair(int sv[2])
{
	if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv))
		return -1;
	int one = 1;
	if (setsockopt(sv[1], SOL_SOCKET, SO_PASSCRED, &one, sizeof one)) {
		close(sv[0]);
		close(sv[1]);
		return -1;
	}
	return 0;
}

static ssize_t recv_crashpad(int sock, char *data, size_t len, struct msghdr *msg, struct walk *w)
{
	char *control = malloc(CMSG_SPACE(sizeof(struct ucred)) + CMSG_SPACE(sizeof(int) * KMAX));
	if (!control)
		return -1;
	memset(control, 0xAB, CMSG_SPACE(sizeof(struct ucred)) + CMSG_SPACE(sizeof(int) * KMAX));
	struct iovec iov = { data, len };
	memset(msg, 0, sizeof *msg);
	msg->msg_iov = &iov;
	msg->msg_iovlen = 1;
	msg->msg_control = control;
	msg->msg_controllen = CMSG_SPACE(sizeof(struct ucred)) + CMSG_SPACE(sizeof(int) * KMAX);
	ssize_t n = recvmsg(sock, msg, 0);
	int e = errno;
	if (n > 0)
		walk_headers(msg, w);
	else
		memset(w, 0, sizeof *w);
	free(control);
	if (n < 0) {
		errno = e;
	}
	return n;
}

static int walk_ok(const struct walk *w, struct msghdr *msg, size_t exp_controllen)
{
	if (w->bad)
		return 0;
	if (msg->msg_controllen != w->sum_space)
		return 0;
	if (msg->msg_controllen != exp_controllen)
		return 0;
	if (msg->msg_flags & MSG_CTRUNC)
		return 0;
	if (msg->msg_flags & MSG_TRUNC)
		return 0;
	return 1;
}

static void check_plain(void)
{
	int sv[2];
	if (make_pair(sv)) {
		record("crashpad_cmsg_plain", 0, "socketpair errno=%d", errno);
		return;
	}
	ssize_t sent = send(sv[0], "p", 1, MSG_NOSIGNAL);
	char data[16] = { 0 };
	struct msghdr msg;
	struct walk w;
	ssize_t n = recv_crashpad(sv[1], data, sizeof data, &msg, &w);
	int ok = sent == 1 && n == 1 && data[0] == 'p' && w.ncred == 1 && w.nrights == 0 && w.has_cred &&
		w.cred.pid == getpid() && w.cred.uid == getuid() && w.cred.gid == getgid() &&
		walk_ok(&w, &msg, CMSG_SPACE(sizeof(struct ucred)));
	close(sv[0]);
	close(sv[1]);
	if (w.bad)
		record("crashpad_cmsg_plain", 0, "unhandled cmsg %d, %d controllen=%zu sum=%zu", w.bad_level, w.bad_type,
			(size_t)msg.msg_controllen, w.sum_space);
	else if (!walk_ok(&w, &msg, CMSG_SPACE(sizeof(struct ucred))))
		record("crashpad_cmsg_plain", 0, "controllen=%zu sum=%zu ncred=%d nrights=%d flags=%x",
			(size_t)msg.msg_controllen, w.sum_space, w.ncred, w.nrights, msg.msg_flags);
	else
		record("crashpad_cmsg_plain", ok, "n=%zd ncred=%d nrights=%d controllen=%zu", n, w.ncred, w.nrights, (size_t)msg.msg_controllen);
}

static void check_rights(void)
{
	int sv[2];
	if (make_pair(sv)) {
		record("crashpad_cmsg_rights", 0, "socketpair errno=%d", errno);
		return;
	}
	int p[2];
	pipe2(p, O_CLOEXEC);
	char scbuf[CMSG_SPACE(sizeof(int) * 1)];
	memset(scbuf, 0, sizeof scbuf);
	struct iovec siov = { (void *)"r", 1 };
	struct msghdr smsg = { .msg_iov = &siov, .msg_iovlen = 1, .msg_control = scbuf, .msg_controllen = sizeof scbuf };
	struct cmsghdr *scm = CMSG_FIRSTHDR(&smsg);
	scm->cmsg_level = SOL_SOCKET;
	scm->cmsg_type = SCM_RIGHTS;
	scm->cmsg_len = CMSG_LEN(sizeof(int) * 1);
	memcpy(CMSG_DATA(scm), &p[1], sizeof(int));
	ssize_t sent = sendmsg(sv[0], &smsg, MSG_NOSIGNAL);
	char data[16] = { 0 };
	struct msghdr msg;
	struct walk w;
	ssize_t n = recv_crashpad(sv[1], data, sizeof data, &msg, &w);
	int ok = sent == 1 && n == 1 && data[0] == 'r' && w.ncred == 1 && w.nrights == 1 && w.nfds == 1 && w.has_cred &&
		w.cred.pid == getpid() && w.cred.uid == getuid() && w.cred.gid == getgid() &&
		walk_ok(&w, &msg, CMSG_SPACE(sizeof(struct ucred)) + CMSG_SPACE(sizeof(int)));
	if (ok && w.nfds == 1) {
		ok = write(w.fds[0], "hi", 2) == 2;
	}
	for (int i = 0; i < w.nfds; i++)
		close(w.fds[i]);
	close(p[0]);
	close(p[1]);
	close(sv[0]);
	close(sv[1]);
	if (w.bad)
		record("crashpad_cmsg_rights", 0, "unhandled cmsg %d, %d controllen=%zu sum=%zu", w.bad_level, w.bad_type,
			(size_t)msg.msg_controllen, w.sum_space);
	else if (!walk_ok(&w, &msg, CMSG_SPACE(sizeof(struct ucred)) + CMSG_SPACE(sizeof(int))))
		record("crashpad_cmsg_rights", 0, "controllen=%zu sum=%zu ncred=%d nrights=%d nfds=%d flags=%x",
			(size_t)msg.msg_controllen, w.sum_space, w.ncred, w.nrights, w.nfds, msg.msg_flags);
	else
		record("crashpad_cmsg_rights", ok, "n=%zd ncred=%d nrights=%d nfds=%d controllen=%zu", n, w.ncred, w.nrights, w.nfds, (size_t)msg.msg_controllen);
}

static void check_explicit(void)
{
	int sv[2];
	if (make_pair(sv)) {
		record("crashpad_cmsg_explicit", 0, "socketpair errno=%d", errno);
		return;
	}
	struct ucred uc = { getpid(), getuid(), getgid() };
	char ecbuf[CMSG_SPACE(sizeof(struct ucred))];
	memset(ecbuf, 0, sizeof ecbuf);
	struct iovec eiov = { (void *)"e", 1 };
	struct msghdr emsg = { .msg_iov = &eiov, .msg_iovlen = 1, .msg_control = ecbuf, .msg_controllen = sizeof ecbuf };
	struct cmsghdr *ecm = CMSG_FIRSTHDR(&emsg);
	ecm->cmsg_level = SOL_SOCKET;
	ecm->cmsg_type = SCM_CREDENTIALS;
	ecm->cmsg_len = CMSG_LEN(sizeof uc);
	memcpy(CMSG_DATA(ecm), &uc, sizeof uc);
	ssize_t sent = sendmsg(sv[0], &emsg, MSG_NOSIGNAL);
	char data[16] = { 0 };
	struct msghdr msg;
	struct walk w;
	ssize_t n = recv_crashpad(sv[1], data, sizeof data, &msg, &w);
	int ok = sent == 1 && n == 1 && data[0] == 'e' && w.ncred == 1 && w.nrights == 0 && w.has_cred &&
		w.cred.pid == getpid() && w.cred.uid == getuid() && w.cred.gid == getgid() &&
		walk_ok(&w, &msg, CMSG_SPACE(sizeof(struct ucred)));
	close(sv[0]);
	close(sv[1]);
	if (w.bad)
		record("crashpad_cmsg_explicit", 0, "unhandled cmsg %d, %d controllen=%zu sum=%zu", w.bad_level, w.bad_type,
			(size_t)msg.msg_controllen, w.sum_space);
	else if (!walk_ok(&w, &msg, CMSG_SPACE(sizeof(struct ucred))))
		record("crashpad_cmsg_explicit", 0, "controllen=%zu sum=%zu ncred=%d nrights=%d flags=%x",
			(size_t)msg.msg_controllen, w.sum_space, w.ncred, w.nrights, msg.msg_flags);
	else
		record("crashpad_cmsg_explicit", ok, "n=%zd ncred=%d nrights=%d controllen=%zu", n, w.ncred, w.nrights, (size_t)msg.msg_controllen);
}

static void check_no_passcred(void)
{
	int sv[2];
	if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) {
		record("crashpad_cmsg_no_passcred", 0, "socketpair errno=%d", errno);
		return;
	}
	ssize_t sent = send(sv[0], "n", 1, MSG_NOSIGNAL);
	char data[16] = { 0 };
	struct msghdr msg;
	struct walk w;
	ssize_t n = recv_crashpad(sv[1], data, sizeof data, &msg, &w);
	int ok = sent == 1 && n == 1 && data[0] == 'n' && w.ncred == 0 && w.nrights == 0 && !w.has_cred &&
		walk_ok(&w, &msg, 0);
	close(sv[0]);
	close(sv[1]);
	if (w.bad)
		record("crashpad_cmsg_no_passcred", 0, "unhandled cmsg %d, %d controllen=%zu sum=%zu", w.bad_level, w.bad_type,
			(size_t)msg.msg_controllen, w.sum_space);
	else if (!walk_ok(&w, &msg, 0))
		record("crashpad_cmsg_no_passcred", 0, "controllen=%zu sum=%zu ncred=%d nrights=%d flags=%x",
			(size_t)msg.msg_controllen, w.sum_space, w.ncred, w.nrights, msg.msg_flags);
	else
		record("crashpad_cmsg_no_passcred", ok, "n=%zd ncred=%d nrights=%d controllen=%zu", n, w.ncred, w.nrights, (size_t)msg.msg_controllen);
}


int main(void)
{
	alarm(30);
	check_plain();
	check_rights();
	check_explicit();
	check_no_passcred();
	char line[96];
	int len = snprintf(line, sizeof line, "MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", failed ? "FAIL" : "PASS",
		passed, failed);
	write(1, line, (size_t)len);
	return failed ? 1 : 0;
}
