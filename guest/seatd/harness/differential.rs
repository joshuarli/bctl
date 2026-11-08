use seatd::conn::Conn;
use seatd::wire::*;
use std::os::unix::io::{FromRawFd, OwnedFd};
use std::process::{Child, Command, Stdio};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Err(e) = self.0.kill() {
            if e.kind() != std::io::ErrorKind::InvalidInput {
                eprintln!("kill daemon: {e}");
            }
        }
        if let Err(e) = self.0.wait() {
            eprintln!("wait daemon: {e}");
        }
    }
}

fn rust_bin() -> String {
    env!("CARGO_BIN_EXE_seatd").to_string()
}

fn unlink(path: &str) {
    unsafe {
        let c = std::ffi::CString::new(path).unwrap();
        libc::unlink(c.as_ptr());
    }
}

fn spawn(bin: &str, sock: &str) -> Daemon {
    unlink(sock);
    let mut cmd = Command::new(bin);
    cmd.arg("-l")
        .arg("silent")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("SEATD_VTBOUND", "0")
        .env("SEATD_SOCK", sock);
    let child = Daemon(cmd.spawn().expect("spawn daemon"));
    for _ in 0..50 {
        let c = unsafe {
            let cstr = std::ffi::CString::new(sock).unwrap();
            libc::open(cstr.as_ptr(), libc::O_PATH | libc::O_CLOEXEC)
        };
        if c >= 0 {
            unsafe {
                libc::close(c);
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child
}

fn connect(sock: &str) -> Conn {
    unsafe {
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = sock.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            addr.sun_path[i] = *b as libc::c_char;
        }
        let len = std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1;
        let fd = libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        );
        assert!(fd >= 0);
        let mut tries = 0;
        loop {
            let r = libc::connect(
                fd,
                &addr as *const _ as *const libc::sockaddr,
                len as libc::socklen_t,
            );
            if r == 0 {
                break;
            }
            let e = std::io::Error::last_os_error().raw_os_error().unwrap();
            tries += 1;
            assert!(tries < 50 && (e == libc::ENOENT || e == libc::ECONNREFUSED), "connect {e}");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Conn::new(OwnedFd::from_raw_fd(fd))
    }
}

fn poll_in(conn: &Conn, ms: i32) -> bool {
    unsafe {
        let mut pfd = libc::pollfd {
            fd: conn.raw_fd(),
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        libc::poll(&mut pfd, 1, ms) > 0
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Msg {
    opcode: u16,
    body: Vec<u8>,
    had_fd: bool,
}

struct ScriptConn {
    conn: Conn,
}

impl ScriptConn {
    fn send(&mut self, opcode: u16, body: &[u8]) {
        self.conn
            .put(&Header { opcode, size: body.len() as u16 }.encode())
            .unwrap();
        self.conn.put(body).unwrap();
        for _ in 0..100 {
            match self.conn.flush() {
                Ok(_) => return,
                Err(e) if e == libc::EAGAIN => {
                    assert!(poll_in(&self.conn, 2000));
                }
                Err(e) => panic!("flush {e}"),
            }
        }
        panic!("flush never completed");
    }

    fn recv(&mut self) -> Option<Msg> {
        for _ in 0..100 {
            if self.conn.pending() >= 4 {
                break;
            }
            if !poll_in(&self.conn, 2000) {
                panic!("no response");
            }
            match self.conn.read() {
                Ok(0) => return None,
                Ok(_) => {}
                Err(e) if e == libc::EAGAIN => {}
                Err(e) => panic!("read {e}"),
            }
            if self.conn.pending() == 0 {
                let mut pfd = libc::pollfd {
                    fd: self.conn.raw_fd(),
                    events: libc::POLLHUP | libc::POLLERR,
                    revents: 0,
                };
                unsafe {
                    libc::poll(&mut pfd, 1, 0);
                }
                if pfd.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                    if let Ok(0) = self.conn.read() {
                        return None;
                    }
                }
            }
        }
        let mut raw = [0u8; 4];
        if self.conn.get(&mut raw).is_err() {
            return None;
        }
        let hdr = Header::decode(&raw);
        for _ in 0..100 {
            if self.conn.pending() >= hdr.size as usize {
                break;
            }
            assert!(poll_in(&self.conn, 2000));
            match self.conn.read() {
                Ok(0) => panic!("eof mid-frame"),
                Ok(_) => {}
                Err(e) if e == libc::EAGAIN => {}
                Err(e) => panic!("read {e}"),
            }
        }
        let mut body = vec![0u8; hdr.size as usize];
        self.conn.get(&mut body).unwrap();
        let had_fd = self.conn.get_fd().is_ok();
        Some(Msg { opcode: hdr.opcode, body, had_fd })
    }

    fn recv_all(&mut self, n: usize) -> Vec<Msg> {
        (0..n).map(|_| self.recv().expect("expected message")).collect()
    }

    fn expect_eof(&mut self) {
        for _ in 0..100 {
            match self.conn.read() {
                Ok(0) => return,
                Ok(_) => {
                    if self.conn.pending() > 0 {
                        panic!("data after expected eof");
                    }
                }
                Err(e) if e == libc::EAGAIN => {
                    if !poll_in(&self.conn, 2000) {
                        panic!("neither data nor eof");
                    }
                }
                Err(e) => panic!("read {e}"),
            }
            let mut pfd = libc::pollfd {
                fd: self.conn.raw_fd(),
                events: libc::POLLHUP | libc::POLLERR,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut pfd, 1, 0);
            }
            if pfd.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                if let Ok(0) = self.conn.read() {
                    return;
                }
            }
        }
        panic!("eof never arrived");
    }
}

fn run_scripts(sock: &str) -> Vec<Vec<Msg>> {
    let mut out = Vec::new();

    {
        let mut c = ScriptConn { conn: connect(sock) };
        c.send(CLIENT_PING, &[]);
        let mut t = vec![c.recv().unwrap()];
        c.send(CLIENT_OPEN_SEAT, &[]);
        t.extend(c.recv_all(2));
        c.send(CLIENT_PING, &[]);
        t.push(c.recv().unwrap());
        c.send(CLIENT_CLOSE_SEAT, &[]);
        t.push(c.recv().unwrap());
        c.send(CLIENT_PING, &[]);
        t.push(c.recv().unwrap());
        out.push(t);
    }
    {
        let mut c = ScriptConn { conn: connect(sock) };
        c.send(CLIENT_OPEN_SEAT, &[]);
        let mut t = c.recv_all(2);
        let mut path = 10u16.to_ne_bytes().to_vec();
        path.extend_from_slice(b"/dev/null\0");
        c.send(CLIENT_OPEN_DEVICE, &path);
        t.push(c.recv().unwrap());
        c.send(CLIENT_CLOSE_DEVICE, &99i32.to_ne_bytes());
        t.push(c.recv().unwrap());
        c.send(CLIENT_SWITCH_SESSION, &0i32.to_ne_bytes());
        t.push(c.recv().unwrap());
        c.send(CLIENT_SWITCH_SESSION, &99i32.to_ne_bytes());
        t.push(c.recv().unwrap());
        c.send(CLIENT_DISABLE_SEAT, &[]);
        t.push(c.recv().unwrap());
        c.send(CLIENT_CLOSE_SEAT, &[]);
        t.push(c.recv().unwrap());
        out.push(t);
    }
    {
        let mut a = ScriptConn { conn: connect(sock) };
        let mut b = ScriptConn { conn: connect(sock) };
        let mut t = Vec::new();
        a.send(CLIENT_OPEN_SEAT, &[]);
        t.extend(a.recv_all(2));
        b.send(CLIENT_OPEN_SEAT, &[]);
        t.extend(b.recv_all(1));
        a.send(CLIENT_SWITCH_SESSION, &2i32.to_ne_bytes());
        t.extend(a.recv_all(2));
        a.send(CLIENT_DISABLE_SEAT, &[]);
        t.extend(a.recv_all(1));
        t.extend(b.recv_all(1));
        b.send(CLIENT_CLOSE_SEAT, &[]);
        t.extend(b.recv_all(1));
        a.send(CLIENT_CLOSE_SEAT, &[]);
        t.extend(a.recv_all(1));
        out.push(t);
    }
    for (opcode, body) in [
        (999u16, vec![]),
        (CLIENT_OPEN_SEAT, vec![1u8]),
        (CLIENT_OPEN_DEVICE, vec![9u8, 0]),
        (CLIENT_CLOSE_DEVICE, vec![1u8, 2]),
        (CLIENT_CLOSE_SEAT, vec![]),
    ] {
        if opcode == CLIENT_CLOSE_SEAT {
            let mut c = ScriptConn { conn: connect(sock) };
            c.send(CLIENT_CLOSE_SEAT, &[]);
            c.expect_eof();
            out.push(vec![]);
            continue;
        }
        let mut c = ScriptConn { conn: connect(sock) };
        c.send(opcode, &body);
        c.expect_eof();
        out.push(vec![]);
    }
    {
        let mut c = ScriptConn { conn: connect(sock) };
        c.send(CLIENT_OPEN_SEAT, &[]);
        let t = c.recv_all(2);
        c.send(CLIENT_OPEN_SEAT, &[]);
        c.expect_eof();
        out.push(t);
    }
    out
}

fn check_pair() {
    let rust_sock = ".work/tmp/diff-rust.sock";
    let rust = spawn(&rust_bin(), rust_sock);
    let rust_t = run_scripts(rust_sock);
    drop(rust);
    assert_eq!(format!("{rust_t:#?}\n"), include_str!("protocol.golden"));
    assert_eq!(rust_t[0].len(), 6, "D1 must carry six messages");
    assert!(rust_t[1].iter().any(|m| m.opcode == SERVER_ERROR));
    assert!(rust_t[2].len() >= 7, "D3 switch flow must carry events");
}

#[test]
fn transcripts_match_golden() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    check_pair();
}

fn automatic_handoff(sock: &str) -> usize {
    let mut a = ScriptConn { conn: connect(sock) };
    let mut b = ScriptConn { conn: connect(sock) };
    let mut c = ScriptConn { conn: connect(sock) };
    a.send(CLIENT_OPEN_SEAT, &[]);
    a.recv_all(2);
    b.send(CLIENT_OPEN_SEAT, &[]);
    b.recv_all(1);
    c.send(CLIENT_OPEN_SEAT, &[]);
    c.recv_all(1);
    a.send(CLIENT_CLOSE_SEAT, &[]);
    assert_eq!(a.recv().unwrap().opcode, SERVER_SEAT_CLOSED);
    let mut pending = [
        libc::pollfd { fd: b.conn.raw_fd(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: c.conn.raw_fd(), events: libc::POLLIN, revents: 0 },
    ];
    assert!(unsafe { libc::poll(pending.as_mut_ptr(), 2, 2000) } > 0);
    if pending[0].revents & libc::POLLIN != 0 {
        assert_eq!(b.recv().unwrap().opcode, SERVER_ENABLE_SEAT);
        1
    } else {
        assert_eq!(c.recv().unwrap().opcode, SERVER_ENABLE_SEAT);
        2
    }
}

#[test]
fn automatic_handoff_newest_waiter_wins() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rust_sock = ".work/tmp/diff-handoff.sock";
    let _rust = spawn(&rust_bin(), rust_sock);
    assert_eq!(automatic_handoff(rust_sock), 2);
}
