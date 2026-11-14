#define _GNU_SOURCE
#pragma GCC diagnostic ignored "-Wsign-compare"
#include <errno.h>
#include <stddef.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#define DETAIL_MAX 200

static char detail[DETAIL_MAX];

#define NOTE(...) snprintf(detail, sizeof detail, __VA_ARGS__)
#define REQUIRE(cond, ...) do { if (!(cond)) { NOTE(__VA_ARGS__); return 1; } } while (0)

static int open_fds(void)
{
    int n = 0;
    for (int fd = 0; fd < 256; fd++) {
        if (fcntl(fd, F_GETFD) != -1)
            n++;
    }
    return n;
}

static void *unmapped_page(void)
{
    void *p = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED || munmap(p, 4096))
        return 0;
    return p;
}

static struct sockaddr_un abstract_name(const char *name, socklen_t *len)
{
    struct sockaddr_un a;
    memset(&a, 0, sizeof a);
    a.sun_family = AF_UNIX;
    memcpy(a.sun_path + 1, name, strlen(name));
    *len = offsetof(struct sockaddr_un, sun_path) + 1 + strlen(name);
    return a;
}

static int send_one(int s, char c, int fd)
{
    char ctl[CMSG_SPACE(sizeof(int))];
    struct iovec iov = {&c, 1};
    struct msghdr m;
    memset(&m, 0, sizeof m);
    m.msg_iov = &iov;
    m.msg_iovlen = 1;
    if (fd >= 0) {
        memset(ctl, 0, sizeof ctl);
        m.msg_control = ctl;
        m.msg_controllen = sizeof ctl;
        struct cmsghdr *h = CMSG_FIRSTHDR(&m);
        h->cmsg_level = SOL_SOCKET;
        h->cmsg_type = SCM_RIGHTS;
        h->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(h), &fd, sizeof fd);
    }
    return sendmsg(s, &m, MSG_NOSIGNAL) == 1 ? 0 : -1;
}

struct recv_result {
    ssize_t ret;
    int err;
    int flags;
    size_t controllen;
    int rights;
};

static struct recv_result recv_with(int s, char *buf, size_t n, void *ctl, size_t ctllen)
{
    struct iovec iov = {buf, n};
    struct msghdr m;
    memset(&m, 0, sizeof m);
    m.msg_iov = &iov;
    m.msg_iovlen = 1;
    m.msg_control = ctl;
    m.msg_controllen = ctllen;
    struct recv_result r;
    errno = 0;
    r.ret = recvmsg(s, &m, MSG_DONTWAIT);
    r.err = r.ret < 0 ? errno : 0;
    r.flags = m.msg_flags;
    r.controllen = m.msg_controllen;
    r.rights = 0;
    if (r.ret >= 0 && ctl) {
        for (struct cmsghdr *h = CMSG_FIRSTHDR(&m); h; h = CMSG_NXTHDR(&m, h)) {
            if (h->cmsg_level == SOL_SOCKET && h->cmsg_type == SCM_RIGHTS)
                r.rights += (h->cmsg_len - CMSG_LEN(0)) / sizeof(int);
        }
    }
    return r;
}

static int check_getsockname_bad_addr(void)
{
    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    REQUIRE(s >= 0, "socket errno=%d", errno);
    socklen_t nl;
    struct sockaddr_un name = abstract_name("usr-gsn-addr", &nl);
    REQUIRE(!bind(s, (struct sockaddr *)&name, nl), "bind errno=%d", errno);
    socklen_t len = sizeof(struct sockaddr_un);
    errno = 0;
    int r = getsockname(s, (struct sockaddr *)0x1, &len);
    REQUIRE(r == -1 && errno == EFAULT, "bad addr ret=%d errno=%d", r, errno);
    void *guard = unmapped_page();
    REQUIRE(guard, "mmap errno=%d", errno);
    len = sizeof(struct sockaddr_un);
    errno = 0;
    r = getsockname(s, guard, &len);
    REQUIRE(r == -1 && errno == EFAULT, "protnone addr ret=%d errno=%d", r, errno);
    NOTE("bad addr ret=-1 errno=EFAULT");
    return 0;
}

static int check_getsockname_bad_len(void)
{
    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    REQUIRE(s >= 0, "socket errno=%d", errno);
    struct sockaddr_un addr;
    memset(&addr, 0, sizeof addr);
    errno = 0;
    int r = getsockname(s, (struct sockaddr *)&addr, (socklen_t *)0x1);
    REQUIRE(r == -1 && errno == EFAULT, "bad len ret=%d errno=%d", r, errno);
    void *guard = unmapped_page();
    REQUIRE(guard, "mmap errno=%d", errno);
    errno = 0;
    r = getsockname(s, (struct sockaddr *)&addr, guard);
    REQUIRE(r == -1 && errno == EFAULT, "protnone len ret=%d errno=%d", r, errno);
    NOTE("bad len ret=-1 errno=EFAULT");
    return 0;
}

static int check_getsockname_values(void)
{
    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    REQUIRE(s >= 0, "socket errno=%d", errno);
    struct sockaddr_un addr;
    socklen_t len = sizeof addr;
    memset(&addr, 0xa5, sizeof addr);
    REQUIRE(!getsockname(s, (struct sockaddr *)&addr, &len), "unbound errno=%d", errno);
    REQUIRE(len == sizeof(sa_family_t) && addr.sun_family == AF_UNIX, "unbound len=%u family=%d", len, addr.sun_family);
    socklen_t nl;
    struct sockaddr_un name = abstract_name("usr-gsn-ok", &nl);
    REQUIRE(!bind(s, (struct sockaddr *)&name, nl), "bind errno=%d", errno);
    memset(&addr, 0, sizeof addr);
    len = sizeof addr;
    REQUIRE(!getsockname(s, (struct sockaddr *)&addr, &len), "bound errno=%d", errno);
    REQUIRE(len == nl && !memcmp(&addr, &name, nl), "bound len=%u want=%u", len, nl);
    char small[4];
    memset(small, 0x5a, sizeof small);
    len = 3;
    REQUIRE(!getsockname(s, (struct sockaddr *)small, &len), "short errno=%d", errno);
    REQUIRE(len == nl && !memcmp(small, &name, 3) && small[3] == 0x5a, "short len=%u want=%u", len, nl);
    NOTE("unbound len=2 bound len=%u short copy ok", nl);
    return 0;
}

struct unmap_race {
    char *page;
    volatile int stop;
};

static void *unmap_flipper(void *arg)
{
    struct unmap_race *u = arg;
    while (!u->stop) {
        munmap(u->page, 4096);
        mmap(u->page, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    }
    return 0;
}

static int check_getsockname_unmap_race(void)
{
    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    REQUIRE(s >= 0, "socket errno=%d", errno);
    socklen_t nl;
    struct sockaddr_un name = abstract_name("usr-gsn-race", &nl);
    REQUIRE(!bind(s, (struct sockaddr *)&name, nl), "bind errno=%d", errno);
    struct unmap_race u;
    u.stop = 0;
    u.page = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(u.page != MAP_FAILED, "mmap errno=%d", errno);
    pthread_t th;
    REQUIRE(!pthread_create(&th, 0, unmap_flipper, &u), "pthread_create");
    int ok = 0, efault = 0, other = 0;
    for (int i = 0; i < 200000; i++) {
        socklen_t len = 64;
        errno = 0;
        int r = getsockname(s, (struct sockaddr *)u.page, &len);
        if (!r)
            ok++;
        else if (errno == EFAULT)
            efault++;
        else
            other++;
    }
    u.stop = 1;
    pthread_join(th, 0);
    REQUIRE(!other, "unexpected errors=%d", other);
    NOTE("ok=%d efault=%d", ok, efault);
    return 0;
}

static int check_scm_good_two_sends(void)
{
    int sv[2];
    REQUIRE(!socketpair(AF_UNIX, SOCK_STREAM, 0, sv), "socketpair errno=%d", errno);
    int nullfd = open("/dev/null", O_RDONLY);
    REQUIRE(nullfd >= 0, "open errno=%d", errno);
    REQUIRE(!send_one(sv[0], 'X', nullfd) && !send_one(sv[0], 'Y', nullfd), "send errno=%d", errno);
    int base = open_fds();
    char buf[8], ctl[CMSG_SPACE(4 * sizeof(int))];
    struct recv_result r = recv_with(sv[1], buf, sizeof buf, ctl, sizeof ctl);
    REQUIRE(r.ret == 1 && buf[0] == 'X' && r.rights == 1 && !(r.flags & MSG_CTRUNC),
            "first ret=%zd err=%d rights=%d flags=%#x", r.ret, r.err, r.rights, r.flags);
    r = recv_with(sv[1], buf, sizeof buf, ctl, sizeof ctl);
    REQUIRE(r.ret == 1 && buf[0] == 'Y' && r.rights == 1 && !(r.flags & MSG_CTRUNC),
            "second ret=%zd err=%d rights=%d flags=%#x", r.ret, r.err, r.rights, r.flags);
    REQUIRE(open_fds() == base + 2, "fds=%d want=%d", open_fds(), base + 2);
    NOTE("two sends delivered separately");
    return 0;
}

static int check_scm_bad_control_single(void)
{
    int sv[2];
    REQUIRE(!socketpair(AF_UNIX, SOCK_STREAM, 0, sv), "socketpair errno=%d", errno);
    int nullfd = open("/dev/null", O_RDONLY);
    REQUIRE(nullfd >= 0 && !send_one(sv[0], 'Q', nullfd), "send errno=%d", errno);
    void *bad = unmapped_page();
    REQUIRE(bad, "mmap errno=%d", errno);
    int base = open_fds();
    char buf[8];
    struct recv_result r = recv_with(sv[1], buf, sizeof buf, bad, CMSG_SPACE(sizeof(int)));
    int delta = open_fds() - base;
    REQUIRE(r.ret == 1 && buf[0] == 'Q' && (r.flags & MSG_CTRUNC) && !delta,
            "ret=%zd err=%d flags=%#x fd_delta=%+d", r.ret, r.err, r.flags, delta);
    r = recv_with(sv[1], buf, sizeof buf, 0, 0);
    REQUIRE(r.ret < 0 && r.err == EAGAIN, "stream not drained ret=%zd err=%d", r.ret, r.err);
    NOTE("ret=1 MSG_CTRUNC no fd installed");
    return 0;
}

static int check_scm_bad_control_after_bytes(void)
{
    int sv[2];
    REQUIRE(!socketpair(AF_UNIX, SOCK_STREAM, 0, sv), "socketpair errno=%d", errno);
    int nullfd = open("/dev/null", O_RDONLY);
    REQUIRE(nullfd >= 0 && !send_one(sv[0], 'A', -1) && !send_one(sv[0], 'B', nullfd), "send errno=%d", errno);
    void *bad = unmapped_page();
    REQUIRE(bad, "mmap errno=%d", errno);
    int base = open_fds();
    char buf[8];
    struct recv_result r = recv_with(sv[1], buf, sizeof buf, bad, CMSG_SPACE(sizeof(int)));
    int delta = open_fds() - base;
    REQUIRE(r.ret == 2 && buf[0] == 'A' && buf[1] == 'B' && (r.flags & MSG_CTRUNC) && !delta,
            "ret=%zd err=%d flags=%#x fd_delta=%+d", r.ret, r.err, r.flags, delta);
    NOTE("ret=2 MSG_CTRUNC no fd installed");
    return 0;
}

static int check_scm_small_control(void)
{
    int sv[2];
    REQUIRE(!socketpair(AF_UNIX, SOCK_STREAM, 0, sv), "socketpair errno=%d", errno);
    int nullfd = open("/dev/null", O_RDONLY);
    REQUIRE(nullfd >= 0, "open errno=%d", errno);
    REQUIRE(!send_one(sv[0], 'X', nullfd) && !send_one(sv[0], 'Y', nullfd) && !send_one(sv[0], 'Z', nullfd),
            "send errno=%d", errno);
    char buf[8], ctl[CMSG_SPACE(sizeof(int))];
    struct recv_result r = recv_with(sv[1], buf, sizeof buf, ctl, sizeof ctl);
    REQUIRE(r.ret == 1 && r.rights == 1, "first ret=%zd err=%d rights=%d", r.ret, r.err, r.rights);
    int base = open_fds();
    r = recv_with(sv[1], buf, sizeof buf, ctl, 0);
    REQUIRE(r.ret == 1 && buf[0] == 'Y' && (r.flags & MSG_CTRUNC) && !r.rights,
            "no control ret=%zd err=%d flags=%#x rights=%d", r.ret, r.err, r.flags, r.rights);
    r = recv_with(sv[1], buf, sizeof buf, ctl, CMSG_LEN(0));
    REQUIRE(r.ret == 1 && buf[0] == 'Z' && (r.flags & MSG_CTRUNC) && !r.rights,
            "header-only control ret=%zd err=%d flags=%#x rights=%d", r.ret, r.err, r.flags, r.rights);
    REQUIRE(open_fds() == base, "fd delta=%+d", open_fds() - base);
    NOTE("truncated rights dropped without installing fds");
    return 0;
}

struct reconnect {
    int client;
    struct sockaddr_un name[2];
    socklen_t len[2];
    int failures;
};

static void *reconnector(void *arg)
{
    struct reconnect *rc = arg;
    int failures = 0;
    for (int i = 0; i < 3000; i++) {
        int k = i & 1;
        if (connect(rc->client, (struct sockaddr *)&rc->name[k], rc->len[k]))
            failures++;
    }
    __atomic_fetch_add(&rc->failures, failures, __ATOMIC_RELAXED);
    return 0;
}

static int check_dgram_reconnect_stress(void)
{
    struct reconnect rc;
    memset(&rc, 0, sizeof rc);
    int listener[2];
    const char *names[2] = {"usr-dgram-a", "usr-dgram-b"};
    for (int i = 0; i < 2; i++) {
        listener[i] = socket(AF_UNIX, SOCK_DGRAM, 0);
        REQUIRE(listener[i] >= 0, "socket errno=%d", errno);
        rc.name[i] = abstract_name(names[i], &rc.len[i]);
        REQUIRE(!bind(listener[i], (struct sockaddr *)&rc.name[i], rc.len[i]), "bind errno=%d", errno);
    }
    rc.client = socket(AF_UNIX, SOCK_DGRAM, 0);
    REQUIRE(rc.client >= 0, "socket errno=%d", errno);
    REQUIRE(!connect(rc.client, (struct sockaddr *)&rc.name[0], rc.len[0]), "connect errno=%d", errno);
    pthread_t th[4];
    for (int i = 0; i < 4; i++)
        REQUIRE(!pthread_create(&th[i], 0, reconnector, &rc), "pthread_create");
    for (int i = 0; i < 4; i++)
        pthread_join(th[i], 0);
    REQUIRE(!rc.failures, "connect failures=%d", rc.failures);
    REQUIRE(!connect(rc.client, (struct sockaddr *)&rc.name[1], rc.len[1]), "final connect errno=%d", errno);
    REQUIRE(send(rc.client, "D", 1, 0) == 1, "send errno=%d", errno);
    char c = 0;
    REQUIRE(recv(listener[1], &c, 1, 0) == 1 && c == 'D', "recv errno=%d", errno);
    NOTE("12000 concurrent reconnects ok");
    return 0;
}

static int check_peercred(void)
{
    int sv[2];
    REQUIRE(!socketpair(AF_UNIX, SOCK_STREAM, 0, sv), "socketpair errno=%d", errno);
    struct ucred cred;
    socklen_t len = sizeof cred;
    memset(&cred, 0, sizeof cred);
    REQUIRE(!getsockopt(sv[0], SOL_SOCKET, SO_PEERCRED, &cred, &len), "getsockopt errno=%d", errno);
    REQUIRE(len == sizeof cred && cred.pid == getpid() && cred.uid == geteuid() && cred.gid == getegid(),
            "pair len=%u pid=%d uid=%u gid=%u", len, cred.pid, cred.uid, cred.gid);
    int listener = socket(AF_UNIX, SOCK_STREAM, 0);
    socklen_t nl;
    struct sockaddr_un name = abstract_name("usr-peercred", &nl);
    REQUIRE(listener >= 0 && !bind(listener, (struct sockaddr *)&name, nl) && !listen(listener, 1),
            "listen errno=%d", errno);
    pid_t child = fork();
    if (!child) {
        int c = socket(AF_UNIX, SOCK_STREAM, 0);
        if (c < 0 || connect(c, (struct sockaddr *)&name, nl))
            _exit(1);
        char done;
        _exit(read(c, &done, 1) == 1 ? 0 : 2);
    }
    REQUIRE(child > 0, "fork errno=%d", errno);
    int conn = accept(listener, 0, 0);
    REQUIRE(conn >= 0, "accept errno=%d", errno);
    len = sizeof cred;
    memset(&cred, 0, sizeof cred);
    REQUIRE(!getsockopt(conn, SOL_SOCKET, SO_PEERCRED, &cred, &len), "accepted getsockopt errno=%d", errno);
    REQUIRE(cred.pid == child && cred.uid == geteuid() && cred.gid == getegid(),
            "accepted pid=%d want=%d uid=%u gid=%u", cred.pid, child, cred.uid, cred.gid);
    len = sizeof cred;
    memset(&cred, 0, sizeof cred);
    REQUIRE(!getsockopt(conn, SOL_SOCKET, SO_PEERCRED, &cred, &len) && cred.pid == child, "second read pid=%d", cred.pid);
    REQUIRE(write(conn, "x", 1) == 1, "write errno=%d", errno);
    int status = -1;
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && !WEXITSTATUS(status), "child status=%#x", status);
    NOTE("socketpair and accepted peer report pid/uid/gid");
    return 0;
}

static int check_stream_connect(void)
{
    socklen_t nl;
    struct sockaddr_un name = abstract_name("usr-connect", &nl);
    int listener = socket(AF_UNIX, SOCK_STREAM, 0);
    REQUIRE(listener >= 0 && !bind(listener, (struct sockaddr *)&name, nl) && !listen(listener, 4),
            "listen errno=%d", errno);
    int c = socket(AF_UNIX, SOCK_STREAM, 0);
    REQUIRE(c >= 0 && !connect(c, (struct sockaddr *)&name, nl), "connect errno=%d", errno);
    errno = 0;
    REQUIRE(connect(c, (struct sockaddr *)&name, nl) == -1 && errno == EISCONN, "reconnect errno=%d", errno);
    int a = accept(listener, 0, 0);
    REQUIRE(a >= 0, "accept errno=%d", errno);
    REQUIRE(write(c, "k", 1) == 1, "write errno=%d", errno);
    char b = 0;
    REQUIRE(read(a, &b, 1) == 1 && b == 'k', "read errno=%d", errno);
    struct sockaddr_un none = abstract_name("usr-no-such-listener", &nl);
    int d = socket(AF_UNIX, SOCK_STREAM, 0);
    errno = 0;
    REQUIRE(connect(d, (struct sockaddr *)&none, nl) == -1 && errno == ECONNREFUSED, "refused errno=%d", errno);
    NOTE("connect, EISCONN and ECONNREFUSED");
    return 0;
}

static const struct {
    const char *name;
    int (*fn)(void);
} checks[] = {
    {"getsockname_bad_addr", check_getsockname_bad_addr},
    {"getsockname_bad_len", check_getsockname_bad_len},
    {"getsockname_values", check_getsockname_values},
    {"scm_good_two_sends", check_scm_good_two_sends},
    {"scm_small_control", check_scm_small_control},
    {"scm_bad_control_single", check_scm_bad_control_single},
    {"scm_bad_control_after_bytes", check_scm_bad_control_after_bytes},
    {"stream_connect", check_stream_connect},
    {"peercred", check_peercred},
    {"dgram_reconnect_stress", check_dgram_reconnect_stress},
    {"getsockname_unmap_race", check_getsockname_unmap_race},
};

#define NCHECKS (sizeof checks / sizeof checks[0])

static int run_check(unsigned i)
{
    int fds[2];
    if (pipe(fds))
        return 0;
    pid_t child = fork();
    if (!child) {
        close(fds[0]);
        int rc = checks[i].fn();
        size_t n = strnlen(detail, sizeof detail);
        if (write(fds[1], detail, n) < 0)
            _exit(99);
        _exit(rc);
    }
    close(fds[1]);
    char text[DETAIL_MAX + 1];
    ssize_t n = 0, got;
    while ((got = read(fds[0], text + n, DETAIL_MAX - n)) > 0)
        n += got;
    close(fds[0]);
    text[n] = 0;
    for (char *p = text; *p; p++)
        if (*p == '\n' || *p == ' ')
            *p = '_';
    int status = -1;
    int ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && !WEXITSTATUS(status);
    printf("MINI_CHROMIUM_TEST name=%s status=%s detail=%s\n", checks[i].name, ok ? "PASS" : "FAIL", text);
    fflush(stdout);
    return ok;
}

int main(void)
{
    alarm(50);
    unsigned passed = 0;
    for (unsigned i = 0; i < NCHECKS; i++)
        passed += run_check(i);
    printf("MINI_CHROMIUM_RESULT status=%s passed=%u failed=%u\n", passed == NCHECKS ? "PASS" : "FAIL",
           passed, (unsigned)NCHECKS - passed);
    return passed != NCHECKS;
}
