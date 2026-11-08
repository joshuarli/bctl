use crate::{log_error, log_info};
use rustix::fd::OwnedFd;
use std::ffi::CString;
use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};

pub const LISTEN_BACKLOG: i32 = 16;

#[cfg(target_os = "linux")]
pub const DEFAULT_SOCKET_PATH: &str = "/run/seatd.sock";
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_SOCKET_PATH: &str = "/var/run/seatd.sock";

pub fn socket_path() -> String {
    std::env::var("SEATD_SOCK").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string())
}

fn os_error() -> io::Error {
    io::Error::last_os_error()
}

fn startup_lock(path: &str) -> Result<OwnedFd, io::Error> {
    let lock_path = format!("{path}.lock");
    let clock = CString::new(lock_path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "lock path contains NUL"))?;
    unsafe {
        let fd = libc::open(
            clock.as_ptr(),
            libc::O_CREAT | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        );
        if fd < 0 {
            return Err(os_error());
        }
        if libc::flock(fd, libc::LOCK_EX) != 0 {
            let e = os_error();
            libc::close(fd);
            return Err(e);
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

pub fn path_stat_kind(path: &str) -> Result<libc::mode_t, io::Error> {
    unsafe {
        let cpath = CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket path contains NUL"))?;
        let mut st: libc::stat = std::mem::zeroed();
        if libc::lstat(cpath.as_ptr(), &mut st) != 0 {
            return Err(os_error());
        }
        Ok(st.st_mode)
    }
}

pub fn unlink_path(path: &str) -> Result<(), io::Error> {
    let cpath = CString::new(path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket path contains NUL"))?;
    unsafe {
        if libc::unlink(cpath.as_ptr()) != 0 {
            return Err(os_error());
        }
    }
    Ok(())
}

pub fn probe_live(path: &str) -> bool {
    let Ok(cpath) = CString::new(path) else {
        return false;
    };
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return false;
        }
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = cpath.as_bytes_with_nul();
        if bytes.len() > std::mem::size_of_val(&addr.sun_path) {
            libc::close(fd);
            return false;
        }
        for (i, b) in bytes.iter().enumerate() {
            addr.sun_path[i] = *b as libc::c_char;
        }
        let len = std::mem::size_of::<libc::sa_family_t>() + cpath.as_bytes().len() + 1;
        let ret = libc::connect(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            len as libc::socklen_t,
        );
        let live = ret == 0;
        libc::close(fd);
        live
    }
}

fn parent_dev_ino(path: &str) -> Result<(libc::dev_t, libc::ino_t), io::Error> {
    let parent = std::path::Path::new(path)
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"))?;
    let parent_str = parent.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "parent path not UTF-8")
    })?;
    let cparent = CString::new(parent_str)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "parent contains NUL"))?;
    unsafe {
        let fd = libc::openat(
            libc::AT_FDCWD,
            cparent.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        );
        if fd < 0 {
            return Err(os_error());
        }
        let mut sb: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut sb) != 0 {
            let e = os_error();
            libc::close(fd);
            return Err(e);
        }
        if (sb.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            libc::close(fd);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket parent is not a directory",
            ));
        }
        let dev = sb.st_dev;
        let ino = sb.st_ino;
        libc::close(fd);
        Ok((dev, ino))
    }
}

pub fn open_socket(path: &str, uid: i64, gid: i64) -> Result<OwnedFd, io::Error> {
    let _lock = startup_lock(path)?;
    unsafe { open_socket_locked(path, uid, gid) }
}

unsafe fn open_socket_locked(path: &str, uid: i64, gid: i64) -> Result<OwnedFd, io::Error> {
    unsafe {
        let fd = libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        );
        if fd < 0 {
            let e = os_error();
            log_error!("Could not create socket: {e}");
            return Err(e);
        }
        let cpath = match CString::new(path) {
            Ok(c) => c,
            Err(_) => {
                libc::close(fd);
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "socket path contains NUL"));
            }
        };
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = cpath.as_bytes_with_nul();
        if bytes.len() > std::mem::size_of_val(&addr.sun_path) {
            libc::close(fd);
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "socket path too long"));
        }
        for (i, b) in bytes.iter().enumerate() {
            addr.sun_path[i] = *b as libc::c_char;
        }
        let len = std::mem::size_of::<libc::sa_family_t>() + cpath.as_bytes().len() + 1;
        let parent_id = parent_dev_ino(path).unwrap_or((0, 0));
        let old_umask = libc::umask(0o007);
        let mut ret = libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            len as libc::socklen_t,
        );
        libc::umask(old_umask);
        if ret != 0 {
            let e = os_error();
            if e.raw_os_error() == Some(libc::EADDRINUSE) {
                if probe_live(path) {
                    log_error!("Live daemon owns socket path {path}, refusing to steal");
                    libc::close(fd);
                    return Err(e);
                }
                match path_stat_kind(path) {
                    Ok(mode) if mode & libc::S_IFMT == libc::S_IFSOCK => {}
                    _ => {
                        log_error!("Non-socket file at {path}, refusing to unlink");
                        libc::close(fd);
                        return Err(e);
                    }
                }
                if unlink_path(path).is_err() {
                    libc::close(fd);
                    return Err(e);
                }
                let old_umask2 = libc::umask(0o007);
                ret = libc::bind(
                    fd,
                    &addr as *const _ as *const libc::sockaddr,
                    len as libc::socklen_t,
                );
                libc::umask(old_umask2);
                if ret != 0 {
                    let e2 = os_error();
                    log_error!("Could not bind socket after stale unlink: {e2}");
                    libc::close(fd);
                    return Err(e2);
                }
            } else {
                log_error!("Could not bind socket: {e}");
                libc::close(fd);
                return Err(e);
            }
        }
        if parent_id != (0, 0) {
            match parent_dev_ino(path) {
                Ok(now) if now == parent_id => {}
                _ => {
                    log_error!("Socket parent replaced during bind, refusing");
                    libc::unlink(cpath.as_ptr());
                    libc::close(fd);
                    return Err(io::Error::other("socket parent replaced"));
                }
            }
        }
        if uid != -1 || gid != -1 {
            if libc::chmod(cpath.as_ptr(), 0o770) != 0 {
                let e = os_error();
                log_error!("Could not chmod socket: {e}");
                libc::unlink(cpath.as_ptr());
                libc::close(fd);
                return Err(e);
            }
            if libc::chown(cpath.as_ptr(), uid as libc::uid_t, gid as libc::gid_t) != 0 {
                let e = os_error();
                log_error!("Could not chown socket to uid {uid}, gid {gid}: {e}");
                libc::unlink(cpath.as_ptr());
                libc::close(fd);
                return Err(e);
            }
        } else if libc::chmod(cpath.as_ptr(), 0o770) != 0 {
            let e = os_error();
            log_error!("Could not chmod socket: {e}");
            libc::unlink(cpath.as_ptr());
            libc::close(fd);
            return Err(e);
        }
        if libc::listen(fd, LISTEN_BACKLOG) != 0 {
            let e = os_error();
            log_error!("Could not listen on socket: {e}");
            libc::unlink(cpath.as_ptr());
            libc::close(fd);
            return Err(e);
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

pub struct PeerCreds {
    pub pid: libc::pid_t,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
}

pub fn peer_creds(fd: RawFd) -> io::Result<PeerCreds> {
    unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        ) != 0
        {
            return Err(os_error());
        }
        Ok(PeerCreds {
            pid: cred.pid,
            uid: cred.uid,
            gid: cred.gid,
        })
    }
}

pub fn accept_client(listen_fd: RawFd) -> Option<(OwnedFd, PeerCreds)> {    unsafe {
        let fd = libc::accept4(
            listen_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
        );
        if fd < 0 {
            let e = os_error();
            log_error!("Could not accept client connection: {e}");
            return None;
        }
        match peer_creds(fd) {
            Ok(creds) => {
                log_info!(
                    "New client connected (pid: {}, uid: {}, gid: {})",
                    creds.pid,
                    creds.uid,
                    creds.gid
                );
                Some((OwnedFd::from_raw_fd(fd), creds))
            }
            Err(e) => {
                log_error!("Could not prepare new client socket: {e}");
                libc::close(fd);
                None
            }
        }
    }
}

pub fn raw_fd(fd: &OwnedFd) -> RawFd {
    fd.as_raw_fd()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_sock(tag: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("seatd-admit-{}-{tag}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn stale_socket_is_reclaimed() {
        let path = tmp_sock("stale");
        let fd = open_socket(&path, -1, -1).unwrap();
        let raw = raw_fd(&fd);
        let _ = raw;
        drop(fd);
        assert!(!probe_live(&path));
        let fd2 = open_socket(&path, -1, -1).unwrap();
        let mut sb: libc::stat = unsafe { std::mem::zeroed() };
        let c = CString::new(path.clone()).unwrap();
        assert_eq!(unsafe { libc::stat(c.as_ptr(), &mut sb) }, 0);
        assert_eq!(sb.st_mode & libc::S_IFMT, libc::S_IFSOCK);
        assert_eq!(sb.st_mode & 0o777, 0o770);
        drop(fd2);
        let _ = unlink_path(&path);
    }

    #[test]
    fn live_socket_is_not_stolen() {
        let path = tmp_sock("live");
        let live = open_socket(&path, -1, -1).unwrap();
        assert!(probe_live(&path));
        assert!(open_socket(&path, -1, -1).is_err());
        drop(live);
        let _ = unlink_path(&path);
    }

    #[test]
    fn non_socket_file_is_never_unlinked() {
        let mut p = std::env::temp_dir();
        p.push(format!("seatd-admit-{}-regular", std::process::id()));
        std::fs::write(&p, b"policy").unwrap();
        let path = p.to_string_lossy().into_owned();
        assert!(!probe_live(&path));
        assert!(open_socket(&path, -1, -1).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"policy");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn concurrent_startup_single_owner() {
        let path = tmp_sock("race");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{path}.lock"));
        let p1 = path.clone();
        let p2 = path.clone();
        let h1 = std::thread::spawn(move || open_socket(&p1, -1, -1));
        let h2 = std::thread::spawn(move || open_socket(&p2, -1, -1));
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        match (r1, r2) {
            (Ok(_a), Err(_)) | (Err(_), Ok(_a)) => {
                assert!(probe_live(&path));
            }
            (Err(_), Err(_)) => panic!("both startups failed"),
            (Ok(_a), Ok(_b)) => {
                panic!("both startups succeeded; lock did not serialize");
            }
        }
        let _ = unlink_path(&path);
        let _ = std::fs::remove_file(format!("{path}.lock"));
    }

    #[test]
    fn crash_without_unlink_is_reclaimed_by_next_owner() {
        let path = tmp_sock("crash");
        let first = open_socket(&path, -1, -1).unwrap();
        assert!(probe_live(&path));
        assert!(open_socket(&path, -1, -1).is_err());
        drop(first);
        assert!(!probe_live(&path));
        let second = open_socket(&path, -1, -1);
        assert!(second.is_ok(), "stale socket after crash must be reclaimable");
        drop(second);
        let _ = unlink_path(&path);
        let _ = std::fs::remove_file(format!("{path}.lock"));
    }

    #[test]
    fn non_directory_parent_is_rejected() {
        let mut p = std::env::temp_dir();
        p.push(format!("seatd-admit-{}-fileparent", std::process::id()));
        std::fs::write(&p, b"not a dir").unwrap();
        let path = format!("{}/sock", p.to_string_lossy());
        assert!(open_socket(&path, -1, -1).is_err());
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(format!("{path}.lock"));
    }

    #[test]
    fn replaced_parent_is_detected() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("seatd-admit-{}-parent", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sock").to_string_lossy().into_owned();
        let before = super::parent_dev_ino(&path).unwrap();
        let mut moved = std::env::temp_dir();
        moved.push(format!("seatd-admit-{}-parent-moved", std::process::id()));
        let _ = std::fs::remove_dir_all(&moved);
        std::fs::rename(&dir, &moved).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let after = super::parent_dev_ino(&path).unwrap();
        assert_ne!(before, after, "replaced parent must have fresh identity");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&moved);
    }

    #[test]
    fn failed_ownership_setup_cleans_up_socket() {
        let path = tmp_sock("chown-fail");
        let _ = std::fs::remove_file(&path);
        let r = open_socket(&path, 0, 0);
        if unsafe { libc::geteuid() } == 0 {
            drop(r);
            let _ = unlink_path(&path);
        } else {
            assert!(r.is_err(), "unprivileged chown to root must fail");
            assert!(
                !std::path::Path::new(&path).exists(),
                "bind-then-chown failure must not leave the socket behind"
            );
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{path}.lock"));
    }
}
