use crate::admit;
use crate::conn::Conn;
use crate::effect::RealEffect;
use crate::session::{self, Client, ClientSlot, Seat};
use crate::wire::Header;
use crate::{log_error, log_info};
use rustix::fd::OwnedFd;
use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};

const WATCHED: [libc::c_int; 4] = [libc::SIGUSR1, libc::SIGUSR2, libc::SIGINT, libc::SIGTERM];

pub struct Server {
    pub running: bool,
    listen_fd: Option<OwnedFd>,
    sig_fd: Option<OwnedFd>,
    old_mask: libc::sigset_t,
    pollfds: Vec<libc::pollfd>,
    clients: Vec<ClientSlot>,
    seats: Vec<Seat>,
    fx: RealEffect,
    dirty: bool,
}


fn os_error() -> io::Error {
    io::Error::last_os_error()
}

impl Server {
    pub fn new() -> io::Result<Server> {
        let (sig_raw, old): (RawFd, libc::sigset_t) = unsafe {
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            for sig in WATCHED {
                libc::sigaddset(&mut mask, sig);
            }
            let mut old: libc::sigset_t = std::mem::zeroed();
            if libc::pthread_sigmask(libc::SIG_BLOCK, &mask, &mut old) != 0 {
                return Err(os_error());
            }
            let sig_fd = libc::signalfd(
                -1,
                &mask,
                libc::SFD_NONBLOCK | libc::SFD_CLOEXEC,
            );
            if sig_fd < 0 {
                let e = os_error();
                libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
                return Err(e);
            }
            (sig_fd, old)
        };
        let vt_bound = session::vt_bound_from_env();
        if vt_bound {
            log_info!("Created VT-bound seat seat0");
        } else {
            log_info!("Created seat seat0");
        }
        Ok(Server {
            running: true,
            listen_fd: None,
            sig_fd: Some(unsafe { OwnedFd::from_raw_fd(sig_raw) }),
            old_mask: old,
            pollfds: Vec::new(),
            clients: Vec::new(),
            seats: vec![Seat::new("seat0", vt_bound)],
            fx: RealEffect,
            dirty: true,
        })
    }

    pub fn set_listen(&mut self, fd: OwnedFd) {
        self.listen_fd = Some(fd);
    }

    fn rebuild(&mut self) {
        self.pollfds.clear();
        self.pollfds.push(libc::pollfd {
            fd: self.sig_fd.as_ref().unwrap().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        if let Some(listen) = self.listen_fd.as_ref() {
            self.pollfds.push(libc::pollfd {
                fd: listen.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        for client in self.clients.iter() {
            let mut events = libc::POLLIN;
            if client.conn.want_flush() {
                events |= libc::POLLOUT;
            }
            self.pollfds.push(libc::pollfd {
                fd: client.conn.raw_fd(),
                events,
                revents: 0,
            });
        }
        self.dirty = false;
    }

    fn handle_signals(&mut self) {
        let sig_fd = self.sig_fd.as_ref().unwrap().as_raw_fd();
        loop {
            let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
            let n = unsafe {
                libc::read(
                    sig_fd,
                    &mut info as *mut _ as *mut libc::c_void,
                    std::mem::size_of::<libc::signalfd_siginfo>(),
                )
            };
            if n != std::mem::size_of::<libc::signalfd_siginfo>() as isize {
                break;
            }
            self.dirty = true;
            match info.ssi_signo as libc::c_int {
                libc::SIGINT | libc::SIGTERM => {
                    self.running = false;
                }
                libc::SIGUSR1 => {
                    crate::log_info!("VT release signal received");
                    session::seat_vt_release(&mut self.seats[0], &mut self.clients, &mut self.fx);
                }
                libc::SIGUSR2 => {
                    crate::log_info!("VT acquire signal received");
                    session::seat_vt_activate(&mut self.seats[0], &mut self.clients, &mut self.fx);
                }
                _ => {}
            }
        }
    }

    fn handle_listen(&mut self) {
        let listen_fd = self.listen_fd.as_ref().unwrap().as_raw_fd();
        if let Some((fd, creds)) = admit::accept_client(listen_fd) {
            self.clients.push(ClientSlot {
                conn: Conn::new(fd),
                client: Client::fresh(creds.pid, creds.uid, creds.gid),
                dead: false,
            });
            self.dirty = true;
        }
    }

    fn destroy_client(&mut self, idx: usize) {
        if self.clients[idx].client.seat.is_some() {
            session::seat_remove_client(
                &mut self.seats[0],
                &mut self.clients,
                idx,
                &mut self.fx,
            );
        }
        self.clients[idx].dead = true;
        self.clients[idx].conn.close_fds();
        self.dirty = true;
    }

    fn dispatch_frames(&mut self, idx: usize) {
        loop {
            let mut raw = [0u8; 4];
            if self.clients[idx].conn.get(&mut raw).is_err() {
                break;
            }
            let hdr = Header::decode(&raw);
            if self.clients[idx].conn.pending() < hdr.size as usize {
                self.clients[idx].conn.restore(4);
                break;
            }
            match session::handle_opcode(
                &mut self.seats[0],
                &mut self.clients,
                idx,
                hdr.opcode,
                hdr.size as usize,
                &mut self.fx,
            ) {
                session::Outcome::Done => {}
                session::Outcome::Destroy => {
                    self.destroy_client(idx);
                    break;
                }
            }
        }
    }

    fn handle_client_writable(&mut self, idx: usize) {
        let client = &mut self.clients[idx];
        match client.conn.flush() {
            Ok(_) => {
                self.dirty = true;
            }
            Err(e) if e == libc::EAGAIN => {
                self.dirty = true;
            }
            Err(e) => {
                let err = io::Error::from_raw_os_error(e);
                log_error!("Could not flush client connection: {err}");
                client.dead = true;
            }
        }
    }

    fn handle_client_readable(&mut self, idx: usize) {
        let client = &mut self.clients[idx];
        match client.conn.read() {
            Ok(0) => {
                log_error!("Could not read client connection: zero-length read");
                client.dead = true;
            }
            Ok(_) => {
                self.dirty = true;
                self.dispatch_frames(idx);
            }
            Err(e) if e == libc::EAGAIN => {}
            Err(e) => {
                let err = io::Error::from_raw_os_error(e);
                log_error!("Could not read client connection: {err}");
                client.dead = true;
            }
        }
    }

    fn sweep_dead(&mut self) {
        let removed: Vec<usize> = self.clients.iter().enumerate()
            .filter_map(|(i, c)| c.dead.then_some(i)).collect();
        if removed.is_empty() {
            return;
        }
        for &idx in &removed {
            self.destroy_client(idx);
        }
        self.clients.retain(|c| !c.dead);
        let remap = |idx: usize| idx - removed.partition_point(|old| *old < idx);
        for seat in &mut self.seats {
            for idx in &mut seat.clients {
                *idx = remap(*idx);
            }
            seat.active_client = seat.active_client.map(remap);
            seat.next_client = seat.next_client.map(remap);
        }
        self.dirty = true;
    }

    pub fn poll_once(&mut self) -> io::Result<()> {
        if self.dirty {
            self.rebuild();
        }
        let n = unsafe {
            libc::poll(
                self.pollfds.as_mut_ptr(),
                self.pollfds.len() as libc::nfds_t,
                -1,
            )
        };
        if n < 0 {
            let e = os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                return Ok(());
            }
            return Err(e);
        }
        let sig_revents = self.pollfds[0].revents;
        if sig_revents & libc::POLLIN != 0 {
            self.handle_signals();
        }
        if self.listen_fd.is_some() && self.pollfds.len() > 1 {
            let listen_revents = self.pollfds[1].revents;
            if listen_revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                let listen_fd = self.listen_fd.as_ref().unwrap().as_raw_fd();
                unsafe {
                    libc::shutdown(listen_fd, libc::SHUT_RDWR);
                }
                self.running = false;
                log_error!("Server socket received an error");
                return Ok(());
            }
            if listen_revents & libc::POLLIN != 0 {
                self.handle_listen();
            }
        }
        let base = if self.listen_fd.is_some() { 2 } else { 1 };
        for i in 0..self.clients.len() {
            if base + i >= self.pollfds.len() {
                continue;
            }
            let revents = self.pollfds[base + i].revents;
            if revents == 0 {
                continue;
            }
            if revents & libc::POLLERR != 0 {
                log_error!("Connection error");
                self.destroy_client(i);
                continue;
            }
            if revents & libc::POLLHUP != 0 {
                log_info!("Client disconnected");
                self.destroy_client(i);
                continue;
            }
            if revents & libc::POLLOUT != 0 {
                self.handle_client_writable(i);
            }
            if revents & libc::POLLIN != 0 {
                self.handle_client_readable(i);
            }
        }
        self.sweep_dead();
        Ok(())
    }

    pub fn run(&mut self) {
        while self.running {
            if let Err(e) = self.poll_once() {
                log_error!("Poller failed: {e}");
                break;
            }
        }
    }

    pub fn finish(mut self) {
        let mut i = 0;
        while i < self.clients.len() {
            if self.clients[i].client.seat.is_some() {
                session::seat_remove_client(
                    &mut self.seats[0],
                    &mut self.clients,
                    i,
                    &mut self.fx,
                );
            }
            i += 1;
        }
        unsafe {
            drop(self.listen_fd.take());
            drop(self.sig_fd.take());
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.old_mask, std::ptr::null_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_processing_flushes_queued_notifications() {
        let mut server = Server::new().unwrap();
        server.seats[0].vt_bound = false;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut fds = [0; 2];
            assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0, fds.as_mut_ptr()) }, 0);
            let peer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
            server.clients.push(ClientSlot {
                conn: Conn::new(unsafe { OwnedFd::from_raw_fd(fds[0]) }),
                client: Client::fresh(1, 1, 1),
                dead: false,
            });
            server.rebuild();
            let filler = [0x55u8; 1024];
            let mut queued = 0;
            loop {
                let n = unsafe { libc::send(fds[0], filler.as_ptr().cast(), filler.len(), libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
                if n < 0 {
                    assert_eq!(os_error().raw_os_error(), Some(libc::EAGAIN));
                    break;
                }
                assert!(n > 0);
                queued += n as usize;
                assert!(queued < 4 * 1024 * 1024);
            }
            let event = Header { opcode: crate::wire::SERVER_ENABLE_SEAT, size: 0 }.encode();
            server.clients[0].conn.put(&event).unwrap();
            assert_eq!(server.clients[0].conn.flush(), Err(libc::EAGAIN));
            assert_eq!(unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGUSR2) }, 0);
            server.handle_signals();
            while queued > 0 {
                let mut drain = [0u8; 1024];
                let n = unsafe { libc::recv(peer.as_raw_fd(), drain.as_mut_ptr().cast(), queued.min(drain.len()), libc::MSG_DONTWAIT) };
                assert!(n > 0);
                queued -= n as usize;
            }
            assert_eq!(unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGUSR1) }, 0);
            server.poll_once().unwrap();
            let mut received = [0u8; 4];
            assert_eq!(unsafe { libc::recv(peer.as_raw_fd(), received.as_mut_ptr().cast(), received.len(), libc::MSG_DONTWAIT) }, 4);
            assert_eq!(received, event);
        }));
        server.finish();
        result.unwrap();
    }
}
