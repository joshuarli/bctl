/* Raw seatd-protocol guest qualification client (no libseat).
 * Speaks the seatd wire protocol directly: framing is u16 opcode + u16
 * size in native byte order, device FDs arrive via SCM_RIGHTS. This is
 * test-only code; it checks in the N1/N2 guest proofs without depending
 * on any upstream C sources. Message flow mirrors the previous probe:
 * open seat, open DRM + evdev, exclusivity and revoke checks, session
 * switch with disable ack, reopen, close, VT restoration checks. */
#include <errno.h>
#include <fcntl.h>
#include <linux/input.h>
#include <linux/kd.h>
#include <linux/vt.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#define DRM_SET_MASTER 0x641e
#define DRM_DROP_MASTER 0x641f

#define C_OPEN_SEAT 1
#define C_CLOSE_SEAT 2
#define C_OPEN_DEVICE 3
#define C_CLOSE_DEVICE 4
#define C_DISABLE_SEAT 5
#define C_SWITCH_SESSION 6

#define S_SEAT_OPENED 0x8001
#define S_DEVICE_OPENED 0x8003
#define S_DEVICE_CLOSED 0x8004
#define S_DISABLE_SEAT 0x8005
#define S_ENABLE_SEAT 0x8006
#define S_SESSION_SWITCHED 0x8008
#define S_SEAT_DISABLED 0x8009

static int sock = -1;
static uint8_t rbuf[4096];
static size_t rlen = 0;
static int pending_fds[64];
static int pending_nfds = 0;

static void require(int ok, const char *what) {
	if (!ok) {
		perror(what);
		exit(1);
	}
}

static void refill(void) {
	struct pollfd p = { .fd = sock, .events = POLLIN };
	require(poll(&p, 1, 30000) > 0, "reply timeout");
	uint8_t cbuf[CMSG_SPACE(sizeof(int)) * 4];
	struct iovec iov = { .iov_base = rbuf + rlen, .iov_len = sizeof rbuf - rlen };
	struct msghdr m;
	memset(&m, 0, sizeof m);
	m.msg_iov = &iov;
	m.msg_iovlen = 1;
	m.msg_control = cbuf;
	m.msg_controllen = sizeof cbuf;
	ssize_t n = recvmsg(sock, &m, 0);
	require(n > 0, "recvmsg");
	rlen += (size_t)n;
	struct cmsghdr *c;
	for (c = CMSG_FIRSTHDR(&m); c; c = CMSG_NXTHDR(&m, c)) {
		if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
			size_t nfd = (c->cmsg_len - CMSG_LEN(0)) / sizeof(int);
			int *fds = (int *)CMSG_DATA(c);
			for (size_t i = 0; i < nfd; i++) {
				require(pending_nfds < 64, "too many fds");
				pending_fds[pending_nfds++] = fds[i];
			}
		}
	}
}

static void take_msg(uint16_t *op, uint8_t *body, size_t *nbody) {
	for (;;) {
		if (rlen >= 4) {
			uint16_t o = rbuf[0] | ((uint16_t)rbuf[1] << 8);
			uint16_t s = rbuf[2] | ((uint16_t)rbuf[3] << 8);
			require(s <= sizeof rbuf - 4, "frame too large");
			if (rlen >= 4u + s) {
				*op = o;
				memcpy(body, rbuf + 4, s);
				*nbody = s;
				memmove(rbuf, rbuf + 4 + s, rlen - 4 - s);
				rlen -= 4 + s;
				return;
			}
		}
		if (rlen == sizeof rbuf) {
			fprintf(stderr, "client: reply overflow\n");
			exit(1);
		}
		refill();
	}
}

static void send_msg(uint16_t op, const void *body, uint16_t n) {
	uint8_t f[4];
	f[0] = op & 0xff;
	f[1] = (op >> 8) & 0xff;
	f[2] = n & 0xff;
	f[3] = (n >> 8) & 0xff;
	require(write(sock, f, 4) == 4, "send header");
	if (n)
		require(write(sock, body, n) == n, "send body");
}

static void expect(uint16_t want, uint8_t *body, size_t *nbody) {
	uint16_t op;
	take_msg(&op, body, nbody);
	if (op != want) {
		fprintf(stderr, "client: opcode 0x%x, want 0x%x\n", op, want);
		exit(1);
	}
}

static int32_t to_i32(uint8_t *b) {
	int32_t v;
	memcpy(&v, b, 4);
	return v;
}

static int take_fd(void) {
	require(pending_nfds > 0, "no device fd arrived");
	return pending_fds[--pending_nfds];
}

static void close_pending(void) {
	while (pending_nfds > 0)
		close(pending_fds[--pending_nfds]);
}

static int32_t open_device(const char *path, int *fdp) {
	uint8_t body[2 + 256];
	size_t pathlen = strlen(path) + 1;
	require(pathlen <= 256, "path too long");
	body[0] = pathlen & 0xff;
	body[1] = (pathlen >> 8) & 0xff;
	memcpy(body + 2, path, pathlen);
	send_msg(C_OPEN_DEVICE, body, (uint16_t)(2 + pathlen));
	uint8_t rb[256];
	size_t rn = 0;
	expect(S_DEVICE_OPENED, rb, &rn);
	require(rn == 4, "device id size");
	*fdp = take_fd();
	return to_i32(rb);
}

static void drain_shutdown(int sigpid) {
	struct pollfd p = { .fd = sock, .events = POLLIN };
	require(kill(sigpid, SIGTERM) == 0, "signal daemon");
	for (int i = 0; i < 20 && !(p.revents & (POLLHUP | POLLERR)); i++) {
		require(poll(&p, 1, 1000) > 0, "daemon disconnect timeout");
		if (p.revents & POLLIN) {
			char buffer[256];
			if (read(sock, buffer, sizeof buffer) == 0)
				break;
		}
	}
	require(p.revents & (POLLHUP | POLLERR), "daemon disconnect");
}

int main(void) {
	int tty = open("/dev/tty0", O_RDWR | O_CLOEXEC), fd, input, gpu, input_id, gpu_id, mode, kb;
	struct vt_mode vt;
	uint8_t rb[256];
	size_t rn = 0;
	require(tty >= 0, "tty0");
	{
		struct sockaddr_un addr;
		const char *path = getenv("SEATD_SOCK");
		if (!path)
			path = "/run/seatd.sock";
		memset(&addr, 0, sizeof addr);
		addr.sun_family = AF_UNIX;
		strncpy(addr.sun_path, path, sizeof addr.sun_path - 1);
		sock = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
		require(sock >= 0, "socket");
		require(connect(sock, (struct sockaddr *)&addr, sizeof addr) == 0, "connect");
	}
	send_msg(C_OPEN_SEAT, NULL, 0);
	expect(S_SEAT_OPENED, rb, &rn);
	expect(S_ENABLE_SEAT, rb, &rn);
	if (getenv("VM_SHUTDOWN_PID")) {
		drain_shutdown(atoi(getenv("VM_SHUTDOWN_PID")));
		fd = open("/dev/tty1", O_RDWR | O_CLOEXEC);
		require(fd >= 0, "tty1 shutdown");
		require(ioctl(fd, VT_GETMODE, &vt) == 0 && vt.mode == VT_AUTO, "shutdown vt mode");
		require(ioctl(fd, KDGETMODE, &mode) == 0 && mode == KD_TEXT, "shutdown display");
		require(ioctl(fd, KDGKBMODE, &kb) == 0 && kb == K_UNICODE, "shutdown keyboard");
		puts("VM_SHUTDOWN_RESTORE_OK");
		return 0;
	}
	gpu_id = open_device("/dev/dri/card0", &gpu);
	(void)gpu_id;
	input_id = open_device("/dev/input/event0", &input);
	fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
	require(fd >= 0, "open competing drm");
	require(ioctl(fd, DRM_SET_MASTER, 0) < 0, "initial drm exclusivity");
	close(fd);
	require(ioctl(input, EVIOCGVERSION, &mode) == 0, "initial evdev");
	{
		int32_t two = 2;
		send_msg(C_SWITCH_SESSION, &two, 4);
	}
	/* VT-bound kernels report the release asynchronously: SWITCHED is
	 * sent synchronously for the accepted ioctl, DISABLE follows the
	 * release signal, in either arrival order. */
	{
		int seen_disable = 0, seen_switched = 0;
		for (int i = 0; i < 20 && !(seen_disable && seen_switched); i++) {
			uint16_t op;
			take_msg(&op, rb, &rn);
			if (op == S_DISABLE_SEAT)
				seen_disable = 1;
			else if (op == S_SESSION_SWITCHED)
				seen_switched = 1;
			else {
				fprintf(stderr, "client: unexpected opcode 0x%x during switch\n", op);
				exit(1);
			}
		}
		require(seen_disable && seen_switched, "switch handshake");
	}
	send_msg(C_DISABLE_SEAT, NULL, 0);
	expect(S_SEAT_DISABLED, rb, &rn);
	errno = 0;
	require(ioctl(input, EVIOCGVERSION, &mode) < 0 && errno == ENODEV, "evdev revoked");
	fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
	require(fd >= 0, "open second drm");
	require(ioctl(fd, DRM_SET_MASTER, 0) == 0, "second drm master");
	require(ioctl(fd, DRM_DROP_MASTER, 0) == 0, "drop second drm master");
	close(fd);
	puts("VM_RELEASE_REVOKE_HANDOFF_OK");
	fflush(stdout);
	require(ioctl(tty, VT_ACTIVATE, 1) == 0, "activate vt1");
	expect(S_ENABLE_SEAT, rb, &rn);
	fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
	require(fd >= 0, "open competing drm after acquire");
	require(ioctl(fd, DRM_SET_MASTER, 0) < 0, "regained drm exclusivity");
	close(fd);
	{
		uint8_t idb[4];
		memcpy(idb, &input_id, 4);
		send_msg(C_CLOSE_DEVICE, idb, 4);
	}
	expect(S_DEVICE_CLOSED, rb, &rn);
	close(input);
	input_id = open_device("/dev/input/event0", &input);
	require(ioctl(input, EVIOCGVERSION, &mode) == 0, "reopened evdev");
	puts("VM_ACQUIRE_REOPEN_OK");
	fflush(stdout);
	{
		uint8_t idb[4];
		memcpy(idb, &input_id, 4);
		send_msg(C_CLOSE_DEVICE, idb, 4);
	}
	expect(S_DEVICE_CLOSED, rb, &rn);
	close(input);
	{
		uint8_t idb[4];
		memcpy(idb, &gpu_id, 4);
		send_msg(C_CLOSE_DEVICE, idb, 4);
	}
	expect(S_DEVICE_CLOSED, rb, &rn);
	close(gpu);
	send_msg(C_CLOSE_SEAT, NULL, 0);
	expect(0x8002, rb, &rn);
	close_pending();
	fd = open("/dev/tty1", O_RDWR | O_CLOEXEC);
	require(fd >= 0, "tty1");
	require(ioctl(fd, VT_GETMODE, &vt) == 0 && vt.mode == VT_AUTO, "restore vt mode");
	require(ioctl(fd, KDGETMODE, &mode) == 0 && mode == KD_TEXT, "restore display");
	require(ioctl(fd, KDGKBMODE, &kb) == 0 && kb == K_UNICODE, "restore keyboard");
	puts("VM_RESTORE_OK");
	return 0;
}
