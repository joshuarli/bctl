use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub struct VsockListener(OwnedFd);

pub struct VsockStream(File);

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 { Err(io::Error::last_os_error()) } else { Ok(result) }
}

impl VsockListener {
    pub fn bind(port: u32) -> io::Result<VsockListener> {
        let fd = check(unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) })?;
        let socket = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        address.svm_cid = libc::VMADDR_CID_ANY;
        address.svm_port = port;
        let length = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
        check(unsafe { libc::bind(socket.as_raw_fd(), (&raw const address).cast(), length) })?;
        check(unsafe { libc::listen(socket.as_raw_fd(), 64) })?;
        Ok(VsockListener(socket))
    }

    pub fn accept(&self) -> io::Result<VsockStream> {
        loop {
            match check(unsafe { libc::accept4(self.0.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_CLOEXEC) }) {
                Ok(fd) => return Ok(VsockStream(unsafe { File::from_raw_fd(fd) })),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }

    pub fn incoming(&self) -> impl Iterator<Item = io::Result<VsockStream>> + '_ {
        std::iter::repeat_with(|| self.accept())
    }
}

impl VsockStream {
    pub fn connect(cid: u32, port: u32) -> io::Result<VsockStream> {
        let fd = check(unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) })?;
        let socket = unsafe { File::from_raw_fd(fd) };
        let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        address.svm_cid = cid;
        address.svm_port = port;
        let length = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
        loop {
            match check(unsafe { libc::connect(socket.as_raw_fd(), (&raw const address).cast(), length) }) {
                Ok(_) => return Ok(VsockStream(socket)),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }

    pub fn try_clone(&self) -> io::Result<VsockStream> {
        self.0.try_clone().map(VsockStream)
    }

    pub fn shutdown_write(&self) -> io::Result<()> {
        check(unsafe { libc::shutdown(self.0.as_raw_fd(), libc::SHUT_WR) }).map(drop)
    }
}

impl Read for VsockStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for VsockStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
