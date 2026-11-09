//! Brokered seat devices through the native seatd client, seatd-only.
//!
//! Speaks the libseat seatd protocol over SEATD_SOCK (or /run/seatd.sock)
//! using the shared protocol crate. LIBSEAT_BACKEND must select seatd; a
//! failed connect, a protocol violation, or a lost daemon is a hard error,
//! never a silent direct-open fallback. Device descriptors arrive over
//! SCM_RIGHTS and are owned by the caller.

use seatd_protocol::conn::Conn;
use seatd_protocol::wire::{
    Header, CLIENT_CLOSE_DEVICE, CLIENT_DISABLE_SEAT, CLIENT_OPEN_DEVICE, CLIENT_OPEN_SEAT,
    CLIENT_SWITCH_SESSION, MAX_PATH_LEN, MAX_SEAT_LEN, SERVER_DEVICE_CLOSED, SERVER_DEVICE_OPENED,
    SERVER_DISABLE_SEAT, SERVER_ENABLE_SEAT, SERVER_ERROR, SERVER_PONG, SERVER_SEAT_CLOSED,
    SERVER_SEAT_DISABLED, SERVER_SEAT_OPENED, SERVER_SESSION_SWITCHED,
};
use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;

const DEFAULT_SOCKET_PATH: &str = "/run/seatd.sock";

fn errno_text(context: &str, errno: i32) -> String {
    format!("{context}: {}", io::Error::from_raw_os_error(errno))
}

fn would_block(errno: i32) -> bool {
    io::Error::from_raw_os_error(errno).kind() == io::ErrorKind::WouldBlock
}

fn require_seatd_backend(selected: Option<String>) -> Result<(), String> {
    match selected {
        Some(value) if value == "seatd" => Ok(()),
        other => Err(format!(
            "LIBSEAT_BACKEND must be seatd for brokered devices, got {other:?}"
        )),
    }
}

fn seatd_socket_path(selected: Option<String>) -> String {
    selected.unwrap_or_else(|| DEFAULT_SOCKET_PATH.to_string())
}

fn is_response(opcode: u16) -> bool {
    matches!(
        opcode,
        SERVER_SEAT_OPENED
            | SERVER_SEAT_CLOSED
            | SERVER_DEVICE_OPENED
            | SERVER_DEVICE_CLOSED
            | SERVER_SESSION_SWITCHED
            | SERVER_SEAT_DISABLED
            | SERVER_ERROR
    )
}

/// The daemon is gone when its side of the session socket hangs up.
/// Poll with a zero timeout so this never blocks the event loop.
fn daemon_liveness(fd: BorrowedFd<'_>) -> Result<(), String> {
    let mut fds = [rustix::event::PollFd::new(
        &fd,
        rustix::event::PollFlags::IN,
    )];
    let zero = rustix::time::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    match rustix::event::poll(&mut fds, Some(&zero)) {
        Ok(_) => {
            if fds[0]
                .revents()
                .intersects(rustix::event::PollFlags::HUP | rustix::event::PollFlags::ERR)
            {
                return Err("seat daemon lost".to_string());
            }
            Ok(())
        }
        Err(e) => {
            if e == rustix::io::Errno::INTR {
                return Ok(());
            }
            Err(format!("seat poll failed: {e}"))
        }
    }
}

fn parse_seat_name(body: &[u8]) -> Result<String, String> {
    if body.len() < 2 {
        return Err("seatd seat name response is too short".to_string());
    }
    let len = u16::from_ne_bytes([body[0], body[1]]) as usize;
    if body.len() != 2 + len {
        return Err("seatd seat name length does not match the response".to_string());
    }
    if len > MAX_SEAT_LEN {
        return Err(format!("seatd seat name too long: {len}"));
    }
    let mut name = &body[2..];
    if let Some((&0, rest)) = name.split_last() {
        name = rest;
    }
    if name.contains(&0) {
        return Err("seatd seat name is not NUL terminated".to_string());
    }
    Ok(String::from_utf8_lossy(name).into_owned())
}

/// An open seatd session. Owns the connection, a duplicated poll descriptor,
/// and the listener transition flags.
pub struct Session {
    conn: RefCell<Conn>,
    fd: OwnedFd,
    enabled: Cell<bool>,
    disabled: Cell<bool>,
    failed: Cell<bool>,
}

impl Session {
    /// Require the seatd backend and open the seat.
    pub fn open() -> Result<Self, String> {
        require_seatd_backend(std::env::var("LIBSEAT_BACKEND").ok())?;
        let path = seatd_socket_path(std::env::var("SEATD_SOCK").ok());
        Self::open_path(&path)
    }

    fn open_path(path: &str) -> Result<Self, String> {
        let stream = UnixStream::connect(path)
            .map_err(|e| format!("seatd connect {path}: {e}"))?;
        stream
            .set_nonblocking(true)
            .map_err(|e| format!("seatd nonblocking {path}: {e}"))?;
        Self::from_conn(stream.into())
    }

    fn from_conn(fd: OwnedFd) -> Result<Self, String> {
        let poll_fd = fd
            .try_clone()
            .map_err(|e| format!("dup seat fd: {e}"))?;
        let session = Self {
            conn: RefCell::new(Conn::new(fd)),
            fd: poll_fd,
            enabled: Cell::new(false),
            disabled: Cell::new(false),
            failed: Cell::new(false),
        };
        session.send(CLIENT_OPEN_SEAT, &[])?;
        let body = session.exchange(SERVER_SEAT_OPENED)?;
        let name = parse_seat_name(&body)?;
        eprintln!("TINYLAND_SEAT_OPEN seat={name}");
        Ok(session)
    }

    /// Borrow the duplicated session fd for the shared poll set.
    pub fn poll_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Dispatch pending seat events without waiting. Daemon loss is fatal.
    pub fn dispatch(&self) -> Result<(), String> {
        self.check()?;
        if let Err(error) = daemon_liveness(self.fd.as_fd()) {
            return self.fail(error);
        }
        if let Some((opcode, _)) = self.pump_once()? {
            return self.fail(format!("unexpected seat response opcode {opcode}"));
        }
        Ok(())
    }

    /// Dispatch with a bounded wait, for the revoke race: the kernel
    /// reports a revoked device as gone before the daemon's disable
    /// notification arrives, so drain first and let a suspend win over
    /// an unplug verdict. Daemon loss is fatal here too.
    pub fn dispatch_wait(&self, timeout_ms: i32) -> Result<(), String> {
        self.check()?;
        if let Err(error) = daemon_liveness(self.fd.as_fd()) {
            return self.fail(error);
        }
        if let Some((opcode, _)) = self.pump_once()? {
            return self.fail(format!("unexpected seat response opcode {opcode}"));
        }
        let timespec = rustix::time::Timespec {
            tv_sec: (timeout_ms / 1000) as i64,
            tv_nsec: ((timeout_ms % 1000) * 1_000_000) as i64,
        };
        let timeout = if timeout_ms < 0 {
            None
        } else {
            Some(&timespec)
        };
        self.wait_readable(timeout)?;
        if let Some((opcode, _)) = self.pump_once()? {
            return self.fail(format!("unexpected seat response opcode {opcode}"));
        }
        Ok(())
    }

    /// Ask the broker to switch sessions. On a VT-bound seat the session
    /// number is the VT number, so this is how a compositor whose keyboards
    /// are grabbed hands a Ctrl+Alt+Fn switch to the privileged broker.
    /// Failure leaves the session unaffected by design.
    pub fn switch_session(&self, session: i32) -> Result<(), String> {
        self.check()?;
        if session < 0 {
            return Err(format!("seatd switch_session: invalid session {session}"));
        }
        self.send(CLIENT_SWITCH_SESSION, &session.to_ne_bytes())?;
        self.exchange(SERVER_SESSION_SWITCHED)
            .map(|_| ())
            .map_err(|error| format!("seatd switch_session: {error}"))
    }

    /// True after disable_seat until devices are dropped and acknowledged.
    pub fn take_disabled(&self) -> bool {
        self.disabled.replace(false)
    }

    /// True after enable_seat until the session reopens its devices.
    pub fn take_enabled(&self) -> bool {
        self.enabled.replace(false)
    }

    /// Acknowledge a disable event after all seat devices are dropped.
    pub fn acknowledge_disable(&self) -> Result<(), String> {
        self.check()?;
        self.send(CLIENT_DISABLE_SEAT, &[])?;
        self.exchange(SERVER_SEAT_DISABLED)
            .map(|_| ())
            .map_err(|error| format!("seatd disable_seat: {error}"))
    }

    /// Open one brokered device. The returned fd is owned by the caller;
    /// pass the id to close_device when the fd is dropped or revoked.
    pub fn open_device(&self, path: &Path) -> Result<(OwnedFd, i32), String> {
        self.check()?;
        let raw = path.as_os_str().as_encoded_bytes();
        if raw.contains(&0) {
            return Err(format!("device path has NUL: {}", path.display()));
        }
        let path_len = raw.len() + 1;
        if path_len > MAX_PATH_LEN {
            return Err(format!("device path too long: {}", path.display()));
        }
        let mut arg = Vec::with_capacity(path_len);
        arg.extend_from_slice(raw);
        arg.push(0);
        {
            let mut conn = self.conn.borrow_mut();
            conn.put(
                &Header {
                    opcode: CLIENT_OPEN_DEVICE,
                    size: (2 + path_len) as u16,
                }
                .encode(),
            )
            .map_err(|e| errno_text("seatd open_device send", e))?;
            conn.put(&(path_len as u16).to_ne_bytes())
                .map_err(|e| errno_text("seatd open_device send", e))?;
        }
        self.flush()?;
        {
            let mut conn = self.conn.borrow_mut();
            conn.put(&arg)
                .map_err(|e| errno_text("seatd open_device send", e))?;
        }
        self.flush()?;
        let body = self
            .exchange(SERVER_DEVICE_OPENED)
            .map_err(|error| format!("seatd open_device {}: {error}", path.display()))?;
        if body.len() != 4 {
            return self.fail("seatd device response has the wrong size".to_string());
        }
        let id = i32::from_ne_bytes([body[0], body[1], body[2], body[3]]);
        let fd = self
            .conn
            .borrow_mut()
            .get_fd()
            .map_err(|e| errno_text("seatd device fd", e));
        match fd {
            Ok(fd) => Ok((fd, id)),
            Err(error) => self.fail(format!(
                "seatd returned no fd for {}: {error}",
                path.display()
            )),
        }
    }

    /// Release the broker side of a device whose fd was already dropped.
    pub fn close_device(&self, id: i32, origin: &str) -> Result<(), String> {
        self.check()?;
        if id < 0 {
            return Err(format!("seatd close_device {origin}: invalid id {id}"));
        }
        self.send(CLIENT_CLOSE_DEVICE, &id.to_ne_bytes())?;
        self.exchange(SERVER_DEVICE_CLOSED)
            .map(|_| ())
            .map_err(|error| format!("seatd close_device {origin}: {error}"))
    }

    fn check(&self) -> Result<(), String> {
        if self.failed.get() {
            Err("seat session failed".to_string())
        } else {
            Ok(())
        }
    }

    fn fail<T>(&self, message: String) -> Result<T, String> {
        self.failed.set(true);
        Err(message)
    }

    fn send(&self, opcode: u16, body: &[u8]) -> Result<(), String> {
        {
            let mut conn = self.conn.borrow_mut();
            conn.put(
                &Header {
                    opcode,
                    size: body.len() as u16,
                }
                .encode(),
            )
            .map_err(|e| errno_text("seatd send", e))?;
            if !body.is_empty() {
                conn.put(body).map_err(|e| errno_text("seatd send", e))?;
            }
        }
        self.flush()
    }

    fn flush(&self) -> Result<(), String> {
        loop {
            match self.conn.borrow_mut().flush() {
                Ok(_) => return Ok(()),
                Err(e) if would_block(e) => self.wait_writable()?,
                Err(e) => return self.fail(errno_text("seatd send", e)),
            }
        }
    }

    fn wait_writable(&self) -> Result<(), String> {
        loop {
            let mut fds = [rustix::event::PollFd::new(
                &self.fd,
                rustix::event::PollFlags::OUT,
            )];
            match rustix::event::poll(&mut fds, None) {
                Ok(_) => {
                    let revents = fds[0].revents();
                    if revents.intersects(
                        rustix::event::PollFlags::HUP | rustix::event::PollFlags::ERR,
                    ) {
                        return self.fail("seat daemon lost".to_string());
                    }
                    return Ok(());
                }
                Err(e) if e == rustix::io::Errno::INTR => {}
                Err(e) => return self.fail(format!("seat poll failed: {e}")),
            }
        }
    }

    fn wait_readable(
        &self,
        timeout: Option<&rustix::time::Timespec>,
    ) -> Result<(), String> {
        loop {
            let mut fds = [rustix::event::PollFd::new(
                &self.fd,
                rustix::event::PollFlags::IN,
            )];
            match rustix::event::poll(&mut fds, timeout) {
                Ok(_) => {
                    let revents = fds[0].revents();
                    if revents.intersects(
                        rustix::event::PollFlags::HUP | rustix::event::PollFlags::ERR,
                    ) {
                        return self.fail("seat daemon lost".to_string());
                    }
                    return Ok(());
                }
                Err(e) if e == rustix::io::Errno::INTR => {}
                Err(e) => return self.fail(format!("seat poll failed: {e}")),
            }
        }
    }

    fn fill(&self) -> Result<(), String> {
        match self.conn.borrow_mut().read() {
            Ok(0) => self.fail("seat daemon lost".to_string()),
            Ok(_) => Ok(()),
            Err(e) if would_block(e) => Ok(()),
            Err(e) => self.fail(errno_text("seatd read", e)),
        }
    }

    fn parse_available(&self) -> Result<Option<(u16, Vec<u8>)>, String> {
        loop {
            let (header, body) = {
                let mut conn = self.conn.borrow_mut();
                if conn.pending() < 4 {
                    return Ok(None);
                }
                let mut raw = [0u8; 4];
                conn.get(&mut raw)
                    .map_err(|e| errno_text("seatd frame header", e))?;
                let header = Header::decode(&raw);
                if conn.pending() < header.size as usize {
                    conn.restore(4);
                    return Ok(None);
                }
                let mut body = vec![0u8; header.size as usize];
                if header.size > 0 {
                    conn.get(&mut body)
                        .map_err(|e| errno_text("seatd frame body", e))?;
                }
                (header, body)
            };
            match header.opcode {
                SERVER_PONG => {
                    if header.size != 0 {
                        return self.fail("seatd pong has a body".to_string());
                    }
                }
                SERVER_ENABLE_SEAT => {
                    if header.size != 0 {
                        return self.fail("seatd enable event has a body".to_string());
                    }
                    self.enabled.set(true);
                }
                SERVER_DISABLE_SEAT => {
                    if header.size != 0 {
                        return self.fail("seatd disable event has a body".to_string());
                    }
                    self.disabled.set(true);
                }
                opcode if is_response(opcode) => return Ok(Some((opcode, body))),
                opcode => return self.fail(format!("unexpected seat message opcode {opcode}")),
            }
        }
    }

    fn pump_once(&self) -> Result<Option<(u16, Vec<u8>)>, String> {
        if let Some(message) = self.parse_available()? {
            return Ok(Some(message));
        }
        self.fill()?;
        self.parse_available()
    }

    fn exchange(&self, expected: u16) -> Result<Vec<u8>, String> {
        loop {
            if let Some((opcode, body)) = self.pump_once()? {
                if opcode == expected {
                    return Ok(body);
                }
                if opcode == SERVER_ERROR {
                    if body.len() != 4 {
                        return self.fail("seatd error response has the wrong size".to_string());
                    }
                    let errno = i32::from_ne_bytes([body[0], body[1], body[2], body[3]]);
                    return Err(format!(
                        "seatd error: {}",
                        io::Error::from_raw_os_error(errno)
                    ));
                }
                return self.fail(format!("unexpected seat response opcode {opcode}"));
            }
            self.wait_readable(None)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::thread;

    fn five_seconds() -> rustix::time::Timespec {
        rustix::time::Timespec {
            tv_sec: 5,
            tv_nsec: 0,
        }
    }

    struct ServerConn {
        conn: Conn,
    }

    impl ServerConn {
        fn new(fd: OwnedFd) -> Self {
            Self {
                conn: Conn::new(fd),
            }
        }

        fn read_msg(&mut self) -> (u16, Vec<u8>) {
            loop {
                if self.conn.pending() >= 4 {
                    break;
                }
                self.fill();
            }
            let mut raw = [0u8; 4];
            self.conn.get(&mut raw).unwrap();
            let header = Header::decode(&raw);
            loop {
                if self.conn.pending() >= header.size as usize {
                    break;
                }
                self.fill();
            }
            let mut body = vec![0u8; header.size as usize];
            self.conn.get(&mut body).unwrap();
            (header.opcode, body)
        }

        fn fill(&mut self) {
            loop {
                match self.conn.read() {
                    Ok(0) => panic!("client closed the connection"),
                    Ok(_) => return,
                    Err(e) if would_block(e) => {
                        let fd = self.conn.as_fd();
                        let mut fds = [rustix::event::PollFd::new(
                            &fd,
                            rustix::event::PollFlags::IN,
                        )];
                        match rustix::event::poll(&mut fds, Some(&five_seconds())) {
                            Ok(0) => panic!("timed out waiting for the client"),
                            Ok(_) => {
                                let revents = fds[0].revents();
                                if revents.intersects(
                                    rustix::event::PollFlags::HUP
                                        | rustix::event::PollFlags::ERR,
                                ) {
                                    panic!("client hung up");
                                }
                            }
                            Err(e) => panic!("poll: {e}"),
                        }
                    }
                    Err(e) => panic!("read: {e}"),
                }
            }
        }

        fn send(&mut self, opcode: u16, body: &[u8]) {
            self.conn
                .put(&Header { opcode, size: body.len() as u16 }.encode())
                .unwrap();
            if !body.is_empty() {
                self.conn.put(body).unwrap();
            }
            self.flush();
        }

        fn send_fd(&mut self, opcode: u16, body: &[u8], fd: OwnedFd) {
            self.conn.put_fd(fd).unwrap();
            self.send(opcode, body);
        }

        fn flush(&mut self) {
            loop {
                match self.conn.flush() {
                    Ok(_) => return,
                    Err(e) if would_block(e) => {
                        let fd = self.conn.as_fd();
                        let mut fds = [rustix::event::PollFd::new(
                            &fd,
                            rustix::event::PollFlags::OUT,
                        )];
                        match rustix::event::poll(&mut fds, Some(&five_seconds())) {
                            Ok(0) => panic!("timed out flushing to the client"),
                            Ok(_) => {
                                let revents = fds[0].revents();
                                if revents.intersects(
                                    rustix::event::PollFlags::HUP
                                        | rustix::event::PollFlags::ERR,
                                ) {
                                    panic!("client hung up");
                                }
                            }
                            Err(e) => panic!("poll: {e}"),
                        }
                    }
                    Err(e) => panic!("flush: {e}"),
                }
            }
        }
    }

    fn start_server<F>(script: F) -> (OwnedFd, thread::JoinHandle<()>)
    where
        F: FnOnce(&mut ServerConn) + Send + 'static,
    {
        let (client, server) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let handle = thread::spawn(move || {
            let mut conn = ServerConn::new(server.into());
            script(&mut conn);
        });
        (client.into(), handle)
    }

    fn seat_opened_body(name: &str) -> Vec<u8> {
        let mut body = ((name.len() + 1) as u16).to_ne_bytes().to_vec();
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body
    }

    #[test]
    fn backend_is_required_to_be_seatd() {
        for selected in [None, Some("builtin".to_string())] {
            let error = require_seatd_backend(selected).unwrap_err();
            assert!(error.contains("LIBSEAT_BACKEND"), "unexpected: {error}");
        }
        assert!(require_seatd_backend(Some("seatd".to_string())).is_ok());
    }

    #[test]
    fn socket_path_honors_seatd_sock() {
        assert_eq!(seatd_socket_path(None), "/run/seatd.sock");
        assert_eq!(
            seatd_socket_path(Some("/run/isolated.sock".to_string())),
            "/run/isolated.sock"
        );
    }

    #[test]
    fn open_without_daemon_is_a_hard_error() {
        let error = match Session::open_path("/nonexistent/bctl-seatd-test.sock") {
            Ok(_) => panic!("open_path unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.contains("seatd connect"), "unexpected: {error}");
        assert!(error.contains("bctl-seatd-test"), "unexpected: {error}");
    }

    #[test]
    fn client_opens_and_closes_brokered_device() {
        let (release, wait) = std::sync::mpsc::channel();
        let (client, server) = start_server(move |conn| {
            let (opcode, body) = conn.read_msg();
            assert_eq!(opcode, CLIENT_OPEN_SEAT);
            assert!(body.is_empty());
            conn.send(SERVER_SEAT_OPENED, &seat_opened_body("seat0"));

            let (opcode, body) = conn.read_msg();
            assert_eq!(opcode, CLIENT_OPEN_DEVICE);
            let path_len = u16::from_ne_bytes([body[0], body[1]]) as usize;
            assert_eq!(path_len, body.len() - 2);
            assert_eq!(&body[2..], b"/dev/null\0");

            let null = std::fs::File::open("/dev/null").unwrap();
            conn.send_fd(SERVER_DEVICE_OPENED, &1i32.to_ne_bytes(), null.into());

            let (opcode, body) = conn.read_msg();
            assert_eq!(opcode, CLIENT_CLOSE_DEVICE);
            assert_eq!(i32::from_ne_bytes(body.try_into().unwrap()), 1);
            conn.send(SERVER_DEVICE_CLOSED, &[]);
            wait.recv().unwrap();
        });

        let session = Session::from_conn(client).unwrap();
        let (fd, id) = session.open_device(Path::new("/dev/null")).unwrap();
        assert_eq!(id, 1);
        let mut file = std::fs::File::from(fd);
        let mut contents = [0u8; 4];
        assert_eq!(file.read(&mut contents).unwrap(), 0);
        session.close_device(1, "test").unwrap();
        release.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn client_dispatches_enable_disable_and_acknowledges_disable() {
        let (release, wait) = std::sync::mpsc::channel();
        let (client, server) = start_server(move |conn| {
            let (opcode, _) = conn.read_msg();
            assert_eq!(opcode, CLIENT_OPEN_SEAT);
            conn.send(SERVER_SEAT_OPENED, &seat_opened_body("seat0"));
            conn.send(SERVER_ENABLE_SEAT, &[]);
            conn.send(SERVER_DISABLE_SEAT, &[]);

            let (opcode, body) = conn.read_msg();
            assert_eq!(opcode, CLIENT_DISABLE_SEAT);
            assert!(body.is_empty());
            conn.send(SERVER_SEAT_DISABLED, &[]);
            conn.send(SERVER_ENABLE_SEAT, &[]);
            wait.recv().unwrap();
        });

        let session = Session::from_conn(client).unwrap();
        let mut enabled = false;
        let mut disabled = false;
        for _ in 0..100 {
            enabled |= session.take_enabled();
            disabled |= session.take_disabled();
            if enabled && disabled {
                break;
            }
            session.dispatch_wait(50).unwrap();
        }
        assert!(enabled, "enable event was not dispatched");
        assert!(disabled, "disable event was not dispatched");
        session.acknowledge_disable().unwrap();
        let mut enabled_again = false;
        for _ in 0..100 {
            session.dispatch_wait(50).unwrap();
            if session.take_enabled() {
                enabled_again = true;
                break;
            }
        }
        assert!(enabled_again, "enable event was not dispatched after resume");
        release.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn client_reports_daemon_loss_on_disconnect() {
        let (release, wait) = std::sync::mpsc::channel();
        let (client, server) = start_server(move |conn| {
            let (opcode, _) = conn.read_msg();
            assert_eq!(opcode, CLIENT_OPEN_SEAT);
            conn.send(SERVER_SEAT_OPENED, &seat_opened_body("seat0"));
            wait.recv().unwrap();
        });

        let session = Session::from_conn(client).unwrap();
        release.send(()).unwrap();
        server.join().unwrap();
        let mut loss = None;
        for _ in 0..100 {
            match session.dispatch() {
                Ok(()) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(error) => {
                    loss = Some(error);
                    break;
                }
            }
        }
        let error = loss.expect("closed broker was not reported as a loss");
        assert!(error.contains("seat daemon lost"), "unexpected: {error}");
        assert!(session.dispatch_wait(10).is_err());
    }
}
