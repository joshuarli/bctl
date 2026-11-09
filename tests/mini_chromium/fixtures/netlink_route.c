#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stddef.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
#define NETLINK_ROUTE 0
struct nlmsghdr { uint32_t nlmsg_len; uint16_t nlmsg_type, nlmsg_flags; uint32_t nlmsg_seq, nlmsg_pid; };
struct sockaddr_nl { unsigned short nl_family, nl_pad; uint32_t nl_pid, nl_groups; };
struct rtgenmsg { unsigned char rtgen_family; };
#define NLMSG_ALIGN(len) (((len) + 3) & ~3u)
#define NLMSG_HDRLEN ((int)NLMSG_ALIGN(sizeof(struct nlmsghdr)))
#define NLMSG_LENGTH(len) ((len) + NLMSG_HDRLEN)
#define NLMSG_NEXT(nlh, len) ((len) -= NLMSG_ALIGN((nlh)->nlmsg_len), (struct nlmsghdr *)(((char *)(nlh)) + NLMSG_ALIGN((nlh)->nlmsg_len)))
#define NLMSG_OK(nlh, len) ((len) >= (int)sizeof(struct nlmsghdr) && (nlh)->nlmsg_len >= sizeof(struct nlmsghdr) && (int)(nlh)->nlmsg_len <= (len))
#define NLMSG_ERROR 2
#define NLMSG_DONE 3
#define NLM_F_REQUEST 1
#define NLM_F_DUMP 0x300
#define RTM_GETLINK 18
#define RTM_GETADDR 22
#define RTMGRP_LINK 1
#define RTMGRP_IPV4_IFADDR 0x10
#define RTMGRP_IPV6_IFADDR 0x100
static int dump(int fd, int type, int seq, char *detail)
{
	struct { struct nlmsghdr h; struct rtgenmsg g; } req = { { NLMSG_LENGTH(sizeof(struct rtgenmsg)), type, NLM_F_REQUEST | NLM_F_DUMP, seq, 0 }, { AF_UNSPEC } };
	struct sockaddr_nl k = { .nl_family = AF_NETLINK };
	if (sendto(fd, &req, req.h.nlmsg_len, 0, (struct sockaddr *)&k, sizeof k) != (ssize_t)req.h.nlmsg_len)
		return 1;
	char buf[8192];
	int msgs = 0;
	for (int reads = 0; reads < 20; reads++) {
		int n = (int)recv(fd, buf, sizeof buf, 0);
		if (n <= 0) { sprintf(detail + strlen(detail), "recv=%d,errno=%d ", n, errno); return 2; }
		for (struct nlmsghdr *h = (struct nlmsghdr *)buf; NLMSG_OK(h, n); h = NLMSG_NEXT(h, n)) {
			msgs++;
			if (h->nlmsg_type == NLMSG_DONE) {
				errno = 0;
				int more = (int)recv(fd, buf, sizeof buf, MSG_DONTWAIT);
				sprintf(detail + strlen(detail), "type%d_msgs=%d,reads=%d,drain=%d/%d ", type, msgs, reads + 1, more, errno);
				return more == -1 && errno == EAGAIN ? 0 : 5;
			}
			if (h->nlmsg_type == NLMSG_ERROR) { sprintf(detail + strlen(detail), "error "); return 3; }
			if (h->nlmsg_len % 4) sprintf(detail + strlen(detail), "unaligned_len=%u ", h->nlmsg_len);
		}
	}
	return 4;
}
int main(void)
{
	alarm(10);
	char detail[512] = "";
	int fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE);
	struct sockaddr_nl a = { .nl_family = AF_NETLINK, .nl_groups = RTMGRP_IPV4_IFADDR | RTMGRP_IPV6_IFADDR | RTMGRP_LINK };
	int b = bind(fd, (struct sockaddr *)&a, sizeof a);
	int r1 = dump(fd, RTM_GETADDR, 1, detail);
	int r2 = dump(fd, RTM_GETLINK, 2, detail);
	int ok = fd >= 0 && !b && !r1 && !r2;
	printf("MINI_CHROMIUM_TEST name=netlink_route_dump_drain status=%s detail=%sr1=%d,r2=%d\n", ok ? "PASS" : "FAIL", detail, r1, r2);
	printf("MINI_CHROMIUM_RESULT status=%s passed=%d failed=%d\n", ok ? "PASS" : "FAIL", ok, !ok);
	return !ok;
}
