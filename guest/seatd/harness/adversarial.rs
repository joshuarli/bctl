use seatd::conn::{Conn, CONNECTION_BUFFER_SIZE, MAX_FDS};

// Test threads share one fd table, so fd-number asserts would race with
// opens in other threads. Every test holds this for its whole body.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
use std::io::Read;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Child, Command, Stdio};

fn daemon_bin() -> String {
    env!("CARGO_BIN_EXE_seatd").to_string()
}

struct Daemon {
    child: Child,
    #[allow(dead_code)]
    sock: String,
}

fn spawn_daemon(sock: &str, args: &[&str], capture_stderr: bool) -> (Daemon, Option<Vec<u8>>) {
    try_unlink(sock);
    let mut rfd = [0; 2];
    assert_eq!(unsafe { libc::pipe(rfd.as_mut_ptr()) }, 0);
    let (r, w) = (rfd[0], rfd[1]);
    let mut cmd = Command::new(daemon_bin());
    cmd.arg("-l")
        .arg(if capture_stderr { "info" } else { "silent" })
        .arg("-n")
        .arg(w.to_string())
        .args(args)
        .env("SEATD_SOCK", sock)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    if capture_stderr {
        cmd.stderr(Stdio::piped());
    } else {
        cmd.stderr(Stdio::null());
    }
    let child = cmd.spawn().expect("spawn seatd");
    unsafe {
        libc::close(w);
    }
    let mut one = [0u8; 1];
    let n = unsafe { libc::read(r, one.as_mut_ptr() as *mut _, 1) };
    unsafe {
        libc::close(r);
    }
    assert_eq!((n, one), (1, *b"\n"), "readiness byte");
    (Daemon { child, sock: sock.to_string() }, None)
}

fn terminate(child: &mut Child) {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
}

fn try_unlink(path: &str) {
    unsafe {
        let c = std::ffi::CString::new(path).unwrap();
        libc::unlink(c.as_ptr());
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn admit_socket_serves_and_unlinks() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/admit-serve.sock";
    let (mut d, _) = spawn_daemon(sock, &[], false);
    let s = std::os::unix::net::UnixStream::connect(sock).expect("connect");
    drop(s);
    terminate(&mut d.child);
    d.child.wait().expect("wait");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!std::path::Path::new(sock).exists(), "socket unlinked on shutdown");
    let _ = &mut d;
}

#[test]
fn admit_sigkill_leaves_socket_for_next_start() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/admit-killed.sock";
    let (mut d, _) = spawn_daemon(sock, &[], false);
    d.child.kill().expect("kill");
    d.child.wait().expect("wait");
    assert!(std::path::Path::new(sock).exists(), "SIGKILL leaves the socket behind");
    let (mut e, _) = spawn_daemon(sock, &[], false);
    std::os::unix::net::UnixStream::connect(sock).expect("replacement serves");
    terminate(&mut e.child);
    e.child.wait().ok();
    let _ = &mut d;
}

#[test]
fn admit_peer_credentials_logged() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/admit-peer.sock";
    try_unlink(sock);
    let mut rfd = [0; 2];
    assert_eq!(unsafe { libc::pipe(rfd.as_mut_ptr()) }, 0);
    let (r, w) = (rfd[0], rfd[1]);
    let mut child = Command::new(daemon_bin())
        .arg("-l")
        .arg("info")
        .arg("-n")
        .arg(w.to_string())
        .env("SEATD_SOCK", sock)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    unsafe {
        libc::close(w);
    }
    let mut one = [0u8; 1];
    assert_eq!(unsafe { libc::read(r, one.as_mut_ptr() as *mut _, 1) }, 1);
    unsafe {
        libc::close(r);
    }
    let s = std::os::unix::net::UnixStream::connect(sock).expect("connect");
    drop(s);
    std::thread::sleep(std::time::Duration::from_millis(500));
    child.kill().expect("kill");
    child.wait().expect("wait");
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    assert!(
        err.contains(&format!("uid: {uid}")) && err.contains(&format!("gid: {gid}")),
        "peer creds in log: {err}"
    );
}

#[test]
fn admit_refuses_non_socket_path() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let path = ".work/tmp/admit-regular-file.sock";
    std::fs::write(path, b"not a socket").unwrap();
    let out = Command::new(daemon_bin())
        .arg("-l")
        .arg("silent")
        .env("SEATD_SOCK", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(std::fs::read(path).unwrap(), b"not a socket");
    try_unlink(path);
}

#[test]
fn admit_z_flag_refuses_competing_instance() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/admit-compete.sock";
    let (_d, _) = spawn_daemon(sock, &[], false);
    let out = Command::new(daemon_bin())
        .arg("-l")
        .arg("silent")
        .arg("-z")
        .env("SEATD_SOCK", sock)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn admit_replaces_leftover_socket() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/admit-replace.sock";
    let (mut first, _) = spawn_daemon(sock, &[], false);
    let (mut second, _) = spawn_daemon(sock, &[], false);
    std::os::unix::net::UnixStream::connect(sock).expect("second serves");
    second.child.kill().ok();
    second.child.wait().ok();
    first.child.kill().ok();
    first.child.wait().ok();
}

#[test]
fn admit_rejects_bad_user_group_and_levels() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for args in [
        vec!["-u", "no-such-user-xyz"],
        vec!["-g", "no-such-group-xyz"],
        vec!["-u", "root", "-z"],
        vec!["-l", "bogus"],
    ] {
        let out = Command::new(daemon_bin())
            .args(&args)
            .env("SEATD_SOCK", ".work/tmp/admit-cli.sock")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .expect("run");
        assert_eq!(out.status.code(), Some(1), "args {args:?}");
    }
}

#[test]
fn admit_group_socket_gets_0770() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/admit-mode.sock";
    let gid = unsafe { libc::getgid() };
    let group = unsafe {
        let gr = libc::getgrgid(gid);
        assert!(!gr.is_null());
        std::ffi::CStr::from_ptr((*gr).gr_name).to_string_lossy().into_owned()
    };
    let (mut d, _) = spawn_daemon(sock, &["-g", &group], false);
    let mode = std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(sock).unwrap());
    assert_eq!(mode & 0o777, 0o770, "socket mode");
    d.child.kill().ok();
    d.child.wait().ok();
}

fn pair() -> (Conn, Conn) {
    unsafe {
        let mut fds = [0; 2];
        assert_eq!(
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
                fds.as_mut_ptr()
            ),
            0
        );
        (
            Conn::new(OwnedFd::from_raw_fd(fds[0])),
            Conn::new(OwnedFd::from_raw_fd(fds[1])),
        )
    }
}

fn dev_null() -> OwnedFd {
    unsafe {
        let fd = libc::open(
            c"/dev/null".as_ptr(),
            libc::O_RDWR | libc::O_CLOEXEC,
        );
        assert!(fd >= 0);
        OwnedFd::from_raw_fd(fd)
    }
}

fn fd_is_open(fd: i32) -> bool {
    unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
}

#[test]
fn wire_fragmented_header_reassembles() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut a, mut b) = pair();
    a.put(&[1u8, 0, 0, 0]).unwrap();
    a.flush().unwrap();
    for _ in 0..4 {
        let n = b.read().unwrap();
        assert!(n <= 4);
        if b.pending() == 4 {
            break;
        }
    }
    assert_eq!(b.pending(), 4);
    let mut hdr = [0u8; 4];
    b.get(&mut hdr).unwrap();
    assert_eq!(hdr, [1u8, 0, 0, 0]);
}

#[test]
fn wire_coalesced_frames_parse_in_order() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut a, mut b) = pair();
    a.put(&[1u8, 0, 0, 0, 7u8, 0, 0, 0]).unwrap();
    a.flush().unwrap();
    b.read().unwrap();
    assert_eq!(b.pending(), 8);
    let mut first = [0u8; 4];
    let mut second = [0u8; 4];
    b.get(&mut first).unwrap();
    b.get(&mut second).unwrap();
    assert_eq!(first, [1u8, 0, 0, 0]);
    assert_eq!(second, [7u8, 0, 0, 0]);
}

#[test]
fn wire_empty_read_is_clean_eof() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (a, mut b) = pair();
    drop(a);
    assert_eq!(b.read().unwrap(), 0);
    assert_eq!(b.pending(), 0);
}

#[test]
fn wire_oversized_put_fails_loudly() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut a, _b) = pair();
    assert_eq!(a.put(&[0u8; CONNECTION_BUFFER_SIZE + 1]), Err(libc::EOVERFLOW));
    let exact = [0u8; CONNECTION_BUFFER_SIZE];
    a.put(&exact).unwrap();
}

#[test]
fn wire_malformed_short_get_is_eagain() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut a, mut b) = pair();
    a.put(&[9u8]).unwrap();
    a.flush().unwrap();
    b.read().unwrap();
    let mut hdr = [0u8; 4];
    assert_eq!(b.get(&mut hdr), Err(libc::EAGAIN));
    assert_eq!(b.pending(), 1);
}

#[test]
fn wire_fd_round_trip_exactly_once() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut a, mut b) = pair();
    let null = dev_null();
    let null_raw = null.as_raw_fd();
    a.put_fd(null).unwrap();
    a.put(&[0xABu8]).unwrap();
    a.flush().unwrap();
    assert!(!fd_is_open(null_raw), "queued fd must be closed after flush");
    b.read().unwrap();
    assert_eq!(b.queued_fds(), 1);
    let got = b.get_fd().unwrap();
    let mut data = [0u8; 1];
    b.get(&mut data).unwrap();
    assert_eq!(data, [0xAB]);
    assert!(matches!(b.get_fd(), Err(libc::EAGAIN)));
    let got_raw = got.as_raw_fd();
    assert!(fd_is_open(got_raw));
    drop(got);
    assert!(!fd_is_open(got_raw));
}

#[test]
fn wire_unexpected_fds_have_one_owner() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut a, mut b) = pair();
    let f1 = dev_null();
    let f2 = dev_null();
    let r1 = f1.as_raw_fd();
    let r2 = f2.as_raw_fd();
    a.put_fd(f1).unwrap();
    a.put_fd(f2).unwrap();
    a.put(&[1u8]).unwrap();
    a.flush().unwrap();
    assert!(!fd_is_open(r1));
    assert!(!fd_is_open(r2));
    b.read().unwrap();
    assert_eq!(b.queued_fds(), 2);
    let g1 = b.get_fd().unwrap();
    let g2 = b.get_fd().unwrap();
    assert!(fd_is_open(g1.as_raw_fd()));
    assert!(fd_is_open(g2.as_raw_fd()));
}

#[test]
fn wire_fd_queue_overflow_fails_loudly() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut a, _b) = pair();
    for _ in 0..MAX_FDS {
        a.put_fd(dev_null()).unwrap();
    }
    assert_eq!(a.put_fd(dev_null()), Err(libc::EOVERFLOW));
}

#[test]
fn wire_receiver_fd_overflow_closes_extras() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut a, mut b) = pair();
    for _ in 0..MAX_FDS {
        a.put_fd(dev_null()).unwrap();
        a.put(&[0u8]).unwrap();
        a.flush().unwrap();
        b.read().unwrap();
    }
    assert_eq!(b.pending(), MAX_FDS);
    assert_eq!(b.queued_fds(), MAX_FDS);
    let extra = dev_null();
    let extra_raw = extra.as_raw_fd();
    a.put_fd(extra).unwrap();
    a.put(&[0u8]).unwrap();
    a.flush().unwrap();
    assert!(!fd_is_open(extra_raw));
    assert_eq!(b.read(), Err(libc::EOVERFLOW));
    let mut drained = 0;
    while b.get_fd().is_ok() {
        drained += 1;
    }
    assert_eq!(drained, MAX_FDS);
}

#[test]
fn wire_partial_write_keeps_fd_exactly_once() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut fds = [0; 2];
        assert_eq!(
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
                fds.as_mut_ptr()
            ),
            0
        );
        let mut a = Conn::new(OwnedFd::from_raw_fd(fds[0]));
        let mut b = Conn::new(OwnedFd::from_raw_fd(fds[1]));
        let small: libc::c_int = 1024;
        libc::setsockopt(
            b.raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &small as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as u32,
        );
        libc::setsockopt(
            a.raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &small as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as u32,
        );
        let payload = [0x55u8; CONNECTION_BUFFER_SIZE];
        a.put(&payload).unwrap();
        let null = dev_null();
        let null_raw = null.as_raw_fd();
        a.put_fd(null).unwrap();
        let mut sent_total = 0usize;
        for _ in 0..200 {
            match a.flush() {
                Ok(n) => {
                    sent_total += n;
                    if sent_total >= CONNECTION_BUFFER_SIZE {
                        break;
                    }
                }
                Err(e) if e == libc::EAGAIN => {
                    let _ = b.read();
                }
                Err(e) => panic!("unexpected flush error {e}"),
            }
        }
        assert_eq!(sent_total, CONNECTION_BUFFER_SIZE);
        assert!(!fd_is_open(null_raw), "fd consumed exactly once on first send");
        loop {
            match b.read() {
                Ok(_) => {
                    if b.pending() >= CONNECTION_BUFFER_SIZE {
                        break;
                    }
                }
                Err(e) if e == libc::EAGAIN => break,
                Err(e) => panic!("unexpected read error {e}"),
            }
        }
        let mut out = [0u8; CONNECTION_BUFFER_SIZE];
        b.get(&mut out).unwrap();
        assert_eq!(out, payload);
        let mut fd_count = 0;
        while b.get_fd().is_ok() {
            fd_count += 1;
        }
        assert_eq!(fd_count, 1, "ancillary rights must not be resent on retry");
    }
}

use std::io::Write as _Write;
use std::os::unix::net::UnixStream;

struct Raw {
    s: UnixStream,
}

impl Raw {
    fn connect(sock: &str) -> Raw {
        let s = UnixStream::connect(sock).expect("connect");
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        s.set_write_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        Raw { s }
    }

    fn send(&mut self, opcode: u16, body: &[u8]) {
        let mut f = Vec::with_capacity(4 + body.len());
        f.extend_from_slice(&opcode.to_ne_bytes());
        f.extend_from_slice(&(body.len() as u16).to_ne_bytes());
        f.extend_from_slice(body);
        self.s.write_all(&f).unwrap();
    }

    fn recv(&mut self) -> Option<(u16, Vec<u8>)> {
        let mut hdr = [0u8; 4];
        match self.s.read_exact(&mut hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(e) => panic!("read {e}"),
        }
        let opcode = u16::from_ne_bytes([hdr[0], hdr[1]]);
        let size = u16::from_ne_bytes([hdr[2], hdr[3]]) as usize;
        let mut body = vec![0u8; size];
        self.s.read_exact(&mut body).unwrap();
        Some((opcode, body))
    }

    fn expect_eof(&mut self) {
        let mut buf = [0u8; 16];
        match self.s.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => panic!("{n} bytes after expected eof"),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(e) => panic!("read {e}"),
        }
    }
}

const S_OPEN: u16 = 0x8001;
const S_CLOSED: u16 = 0x8002;
const S_PONG: u16 = 0x8007;

fn spawn_with_env(sock: &str, vtbound: Option<&str>) -> Daemon {
    try_unlink(sock);
    let mut cmd = Command::new(daemon_bin());
    cmd.arg("-l")
        .arg("silent")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(v) = vtbound {
        cmd.env("SEATD_VTBOUND", v);
    }
    cmd.env("SEATD_SOCK", sock);
    let child = cmd.spawn().expect("spawn");
    for _ in 0..50 {
        if std::path::Path::new(sock).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(std::path::Path::new(sock).exists(), "daemon socket");
    Daemon { child, sock: sock.to_string() }
}

#[test]
fn proto_open_ping_close_unbound() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/proto-unbound.sock";
    let _d = spawn_with_env(sock, Some("0"));
    let mut c = Raw::connect(sock);
    c.send(1, &[]);
    assert_eq!(c.recv().unwrap().0, S_OPEN);
    assert_eq!(c.recv().unwrap().0, 0x8006);
    c.send(7, &[]);
    assert_eq!(c.recv().unwrap().0, S_PONG);
    c.send(2, &[]);
    assert_eq!(c.recv().unwrap().0, S_CLOSED);
}

#[test]
fn proto_vtbound_without_tty_fails_loudly() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/proto-vtbound.sock";
    let _d = spawn_with_env(sock, None);
    let mut c = Raw::connect(sock);
    c.send(1, &[]);
    c.expect_eof();
}

const S_ERROR: u16 = 0xffff;

fn open_unbound_client(sock: &str) -> (Daemon, Raw) {
    let d = spawn_with_env(sock, Some("0"));
    let mut c = Raw::connect(sock);
    c.send(1, &[]);
    assert_eq!(c.recv().unwrap().0, S_OPEN);
    assert_eq!(c.recv().unwrap().0, 0x8006);
    (d, c)
}

#[test]
fn device_short_pipelined_frame_does_not_kill_daemon() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/dev-short-pipeline.sock";
    let (_d, mut c) = open_unbound_client(sock);
    let mut survivor = Raw::connect(sock);
    let mut frames = seatd::wire::Header { opcode: 3, size: 0 }.encode().to_vec();
    frames.extend_from_slice(&seatd::wire::Header { opcode: 7, size: 0 }.encode());
    c.s.write_all(&frames).unwrap();
    c.expect_eof();
    survivor.send(7, &[]);
    assert_eq!(survivor.recv().unwrap().0, 0x8007);
}

#[test]
fn device_unsupported_paths_are_errors_not_death() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/dev-unsupported.sock";
    let (_d, mut c) = open_unbound_client(sock);
    // NOTE: valid-prefix paths that resolve to real host devices are N2
    // material and never touched here.
    for path in [
        "/dev/null",
        "/dev/zero",
        "/dev/tty",
        "/dev/dri/card99",
        "/dev/input/event99",
        "/dev/hidraw99",
        "/dev/../dev/null",
        "/dev/input/../null",
        "",
    ] {
        let mut req = ((path.len() + 1) as u16).to_ne_bytes().to_vec();
        req.extend_from_slice(path.as_bytes());
        req.push(0);
        c.send(3, &req);
        let (op, body) = c.recv().unwrap();
        assert_eq!(op, S_ERROR, "path {path:?}");
        let code = i32::from_ne_bytes(body[..4].try_into().unwrap());
        assert_eq!(code, libc::ENOENT, "path {path:?}");
    }
    c.send(7, &[]);
    assert_eq!(c.recv().unwrap().0, S_PONG);
}

#[test]
fn device_overlong_path_is_protocol_error() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/dev-overlong.sock";
    let (_d, mut c) = open_unbound_client(sock);
    let long = "a".repeat(300);
    let mut req = ((long.len() + 1) as u16).to_ne_bytes().to_vec();
    req.extend_from_slice(long.as_bytes());
    req.push(0);
    c.send(3, &req);
    c.expect_eof();
}

#[test]
fn device_ops_without_seat_destroy() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/dev-noseat.sock";
    let _d = spawn_with_env(sock, Some("0"));
    let mut c = Raw::connect(sock);
    let mut req = 10u16.to_ne_bytes().to_vec();
    req.extend_from_slice(b"/dev/null\0");
    c.send(3, &req);
    c.expect_eof();
    let mut c2 = Raw::connect(sock);
    c2.send(4, &1i32.to_ne_bytes());
    c2.expect_eof();
}

#[test]
fn device_close_then_stale_id_is_ebadf() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/dev-stale.sock";
    let (_d, mut c) = open_unbound_client(sock);
    c.send(4, &1i32.to_ne_bytes());
    let (op, body) = c.recv().unwrap();
    assert_eq!(op, S_ERROR);
    assert_eq!(i32::from_ne_bytes(body[..4].try_into().unwrap()), libc::EBADF);
    c.send(4, &1i32.to_ne_bytes());
    let (op, _) = c.recv().unwrap();
    assert_eq!(op, S_ERROR);
}

fn signal_child(child: &mut Child, sig: i32) {
    unsafe {
        libc::kill(child.id() as i32, sig);
    }
}

#[test]
fn vt_signals_unbound_are_noops() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/vt-sig-unbound.sock";
    let mut d = spawn_with_env(sock, Some("0"));
    signal_child(&mut d.child, libc::SIGUSR1);
    signal_child(&mut d.child, libc::SIGUSR2);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut c = Raw::connect(sock);
    c.send(1, &[]);
    assert_eq!(c.recv().unwrap().0, S_OPEN);
    assert_eq!(c.recv().unwrap().0, 0x8006);
    c.send(7, &[]);
    assert_eq!(c.recv().unwrap().0, S_PONG);
}

#[test]
fn vt_signal_release_without_tty_keeps_serving() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/vt-sig-bound.sock";
    let mut d = spawn_with_env(sock, None);
    signal_child(&mut d.child, libc::SIGUSR1);
    std::thread::sleep(std::time::Duration::from_millis(300));
    signal_child(&mut d.child, libc::SIGUSR2);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut c = Raw::connect(sock);
    c.send(7, &[]);
    assert_eq!(c.recv().unwrap().0, S_PONG);
}

fn proc_fd_count(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/fd")).map(|d| d.count()).unwrap_or(usize::MAX)
}

fn rest_fd_count(child: &Child) -> usize {
    std::thread::sleep(std::time::Duration::from_millis(200));
    proc_fd_count(child.id())
}

fn wait_for_fds(child: &Child, want: usize) {
    for _ in 0..300 {
        if proc_fd_count(child.id()) <= want {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!(
        "fd count {} did not settle to {want}",
        proc_fd_count(child.id())
    );
}

#[test]
fn shutdown_reconnect_storm_leaks_no_fds() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/storm-reconnect.sock";
    let mut d = spawn_with_env(sock, Some("0"));
    let baseline = rest_fd_count(&d.child);
    let mut c = Raw::connect(sock);
    c.send(1, &[]);
    assert_eq!(c.recv().unwrap().0, S_OPEN);
    drop(c);
    wait_for_fds(&d.child, baseline);
    for _ in 0..200 {
        let s = Raw::connect(sock);
        drop(s);
    }
    wait_for_fds(&d.child, baseline);
    assert_eq!(proc_fd_count(d.child.id()), baseline);
    for _ in 0..800 {
        let s = Raw::connect(sock);
        drop(s);
    }
    wait_for_fds(&d.child, baseline);
    assert_eq!(proc_fd_count(d.child.id()), baseline, "0 fd growth over 1000 reconnects");
    terminate(&mut d.child);
    d.child.wait().ok();
}

#[test]
fn shutdown_malformed_storm_leaks_no_fds() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/storm-malformed.sock";
    let mut d = spawn_with_env(sock, Some("0"));
    let baseline = rest_fd_count(&d.child);
    let mut c = Raw::connect(sock);
    c.send(1, &[]);
    assert_eq!(c.recv().unwrap().0, S_OPEN);
    drop(c);
    wait_for_fds(&d.child, baseline);
    for i in 0..100 {
        let mut s = Raw::connect(sock);
        s.send(0x7000 + (i % 100) as u16, &[0xAAu8; 32]);
        let mut buf = [0u8; 64];
        use std::io::Read as _Read;
        let _ = s.s.read(&mut buf);
    }
    wait_for_fds(&d.child, baseline);
    assert_eq!(proc_fd_count(d.child.id()), baseline);
    terminate(&mut d.child);
    d.child.wait().ok();
}

#[test]
fn shutdown_preserves_replacement_socket() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/shutdown-replacement.sock";
    let (mut first, _) = spawn_daemon(sock, &[], false);
    let (mut second, _) = spawn_daemon(sock, &[], false);
    terminate(&mut first.child);
    assert!(first.child.wait().unwrap().success());
    let mut c = Raw::connect(sock);
    c.send(7, &[]);
    assert_eq!(c.recv().unwrap().0, 0x8007);
    terminate(&mut second.child);
    assert!(second.child.wait().unwrap().success());
    assert!(!std::path::Path::new(sock).exists());
}

#[test]
fn shutdown_graceful_restart_serves() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/shutdown-restart.sock";
    let mut first = spawn_with_env(sock, Some("0"));
    terminate(&mut first.child);
    first.child.wait().ok();
    assert!(!std::path::Path::new(sock).exists());
    let mut second = spawn_with_env(sock, Some("0"));
    let mut c = Raw::connect(sock);
    c.send(1, &[]);
    assert_eq!(c.recv().unwrap().0, S_OPEN);
    terminate(&mut second.child);
    second.child.wait().ok();
}
