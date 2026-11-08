use rustix::fd::OwnedFd;
use std::io;
use std::mem;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::ptr;

pub const CONNECTION_BUFFER_SIZE: usize = 256;
pub const MAX_FDS: usize = CONNECTION_BUFFER_SIZE / mem::size_of::<RawFd>();

const CMSG_BUF: usize = 2048;

#[repr(C, align(8))]
struct CmsgBuf([u8; CMSG_BUF]);

impl CmsgBuf {
    fn new() -> Self {
        Self([0u8; CMSG_BUF])
    }
    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr()
    }
    fn len(&self) -> usize {
        CMSG_BUF
    }
}

fn errno_now() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(libc::EINVAL)
}

struct Ring {
    head: u32,
    tail: u32,
    data: [u8; CONNECTION_BUFFER_SIZE],
}

impl Ring {
    fn new() -> Ring {
        Ring {
            head: 0,
            tail: 0,
            data: [0u8; CONNECTION_BUFFER_SIZE],
        }
    }

    fn mask(idx: u32) -> usize {
        (idx & (CONNECTION_BUFFER_SIZE as u32 - 1)) as usize
    }

    fn size(&self) -> usize {
        self.head.wrapping_sub(self.tail) as usize
    }

    fn consume(&mut self, n: usize) {
        self.tail = self.tail.wrapping_add(n as u32);
    }

    fn restore(&mut self, n: usize) {
        self.tail = self.tail.wrapping_sub(n as u32);
    }

    fn copy_out(&self, dst: &mut [u8]) {
        let t = Self::mask(self.tail);
        let n = dst.len();
        if t + n <= CONNECTION_BUFFER_SIZE {
            dst.copy_from_slice(&self.data[t..t + n]);
        } else {
            let first = CONNECTION_BUFFER_SIZE - t;
            dst[..first].copy_from_slice(&self.data[t..]);
            dst[first..].copy_from_slice(&self.data[..n - first]);
        }
    }

    fn put(&mut self, src: &[u8]) -> Result<(), i32> {
        if src.len() > CONNECTION_BUFFER_SIZE {
            return Err(libc::EOVERFLOW);
        }
        let h = Self::mask(self.head);
        if h + src.len() <= CONNECTION_BUFFER_SIZE {
            self.data[h..h + src.len()].copy_from_slice(src);
        } else {
            let first = CONNECTION_BUFFER_SIZE - h;
            self.data[h..].copy_from_slice(&src[..first]);
            self.data[..src.len() - first].copy_from_slice(&src[first..]);
        }
        self.head = self.head.wrapping_add(src.len() as u32);
        Ok(())
    }

    fn used_iovs(&self) -> ([libc::iovec; 2], usize) {
        let head = Self::mask(self.head);
        let tail = Self::mask(self.tail);
        let base = self.data.as_ptr() as *mut libc::c_void;
        unsafe {
            if tail < head {
                (
                    [
                        libc::iovec {
                            iov_base: base.add(tail),
                            iov_len: head - tail,
                        },
                        libc::iovec {
                            iov_base: ptr::null_mut(),
                            iov_len: 0,
                        },
                    ],
                    1,
                )
            } else if head == 0 {
                (
                    [
                        libc::iovec {
                            iov_base: base.add(tail),
                            iov_len: CONNECTION_BUFFER_SIZE - tail,
                        },
                        libc::iovec {
                            iov_base: ptr::null_mut(),
                            iov_len: 0,
                        },
                    ],
                    1,
                )
            } else {
                (
                    [
                        libc::iovec {
                            iov_base: base.add(tail),
                            iov_len: CONNECTION_BUFFER_SIZE - tail,
                        },
                        libc::iovec {
                            iov_base: base,
                            iov_len: head,
                        },
                    ],
                    2,
                )
            }
        }
    }

    fn free_iovs(&mut self) -> ([libc::iovec; 2], usize) {
        let head = Self::mask(self.head);
        let tail = Self::mask(self.tail);
        let base = self.data.as_mut_ptr() as *mut libc::c_void;
        unsafe {
            if head < tail {
                (
                    [
                        libc::iovec {
                            iov_base: base.add(head),
                            iov_len: tail - head,
                        },
                        libc::iovec {
                            iov_base: ptr::null_mut(),
                            iov_len: 0,
                        },
                    ],
                    1,
                )
            } else if tail == 0 {
                (
                    [
                        libc::iovec {
                            iov_base: base.add(head),
                            iov_len: CONNECTION_BUFFER_SIZE - head,
                        },
                        libc::iovec {
                            iov_base: ptr::null_mut(),
                            iov_len: 0,
                        },
                    ],
                    1,
                )
            } else {
                (
                    [
                        libc::iovec {
                            iov_base: base.add(head),
                            iov_len: CONNECTION_BUFFER_SIZE - head,
                        },
                        libc::iovec {
                            iov_base: base,
                            iov_len: tail,
                        },
                    ],
                    2,
                )
            }
        }
    }

    fn close_fds(&mut self) {
        let size = self.size();
        if size == 0 {
            return;
        }
        let mut fds = [0 as RawFd; MAX_FDS];
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(fds.as_mut_ptr() as *mut u8, MAX_FDS * 4)
        };
        self.copy_out(&mut bytes[..size]);
        let count = size / mem::size_of::<RawFd>();
        for &fd in fds.iter().take(count) {
            unsafe {
                libc::close(fd);
            }
        }
        self.consume(count * mem::size_of::<RawFd>());
    }
}

pub struct Conn {
    fd: Option<OwnedFd>,
    input: Ring,
    output: Ring,
    fds_in: Ring,
    fds_out: Ring,
    want_flush: bool,
}

impl Conn {
    pub fn new(fd: OwnedFd) -> Conn {
        Conn {
            fd: Some(fd),
            input: Ring::new(),
            output: Ring::new(),
            fds_in: Ring::new(),
            fds_out: Ring::new(),
            want_flush: false,
        }
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd.as_ref().unwrap().as_raw_fd()
    }

    pub fn pending(&self) -> usize {
        self.input.size()
    }

    pub fn want_flush(&self) -> bool {
        self.want_flush
    }

    pub fn restore(&mut self, count: usize) {
        self.input.restore(count);
    }

    pub fn queued_fds(&self) -> usize {
        self.fds_in.size() / mem::size_of::<RawFd>()
    }

    pub fn read(&mut self) -> Result<usize, i32> {
        if self.input.size() >= CONNECTION_BUFFER_SIZE {
            return Err(libc::EOVERFLOW);
        }
        let (mut iovs, count) = self.input.free_iovs();
        let mut cmsg = CmsgBuf::new();
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_iov = iovs.as_mut_ptr();
        msg.msg_iovlen = count as _;
        msg.msg_control = cmsg.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg.len() as _;
        let fd = self.raw_fd();
        let (len, flags) = loop {
            let mut msg_copy = msg;
            let n = unsafe {
                libc::recvmsg(fd, &mut msg_copy, libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC)
            };
            if n < 0 {
                let e = errno_now();
                if e == libc::EINTR {
                    continue;
                }
                return Err(e);
            }
            msg = msg_copy;
            break (n as usize, msg.msg_flags);
        };
        if len == 0 {
            return Ok(0);
        }
        if (flags & libc::MSG_CTRUNC) != 0 || (flags & libc::MSG_TRUNC) != 0 {
            let _ = self.decode_cmsg(&msg);
            self.close_fds();
            return Err(libc::EOVERFLOW);
        }
        self.decode_cmsg(&msg)?;
        self.input.head = self.input.head.wrapping_add(len as u32);
        Ok(self.input.size())
    }

    fn decode_cmsg(&mut self, msg: &libc::msghdr) -> Result<(), i32> {
        let mut overflow = false;
        let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(msg) };
        while !cmsg.is_null() {
            unsafe {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET
                    && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                {
                    let hdr = libc::CMSG_LEN(0);
                    let size = (*cmsg).cmsg_len.saturating_sub(hdr);
                    if size % (mem::size_of::<RawFd>() as u32) != 0 {
                        overflow = true;
                        let count = size / mem::size_of::<RawFd>() as u32;
                        let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                        for i in 0..count {
                            libc::close(*data.add(i as usize));
                        }
                    } else {
                        let max = (CONNECTION_BUFFER_SIZE - self.fds_in.size()) as u32;
                        if size > max || overflow {
                            overflow = true;
                            let count = size / mem::size_of::<RawFd>() as u32;
                            let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                            for i in 0..count {
                                libc::close(*data.add(i as usize));
                            }
                        } else if self.fds_in.put(std::slice::from_raw_parts(
                            libc::CMSG_DATA(cmsg),
                            size as usize,
                        )) != Ok(())
                        {
                            return Err(errno_now());
                        }
                    }
                }
                cmsg = libc::CMSG_NXTHDR(msg, cmsg);
            }
        }
        if overflow {
            return Err(libc::EOVERFLOW);
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<usize, i32> {
        if !self.want_flush {
            return Ok(0);
        }
        let tail = self.output.tail;
        while self.output.size() > 0 {
            let (mut iovs, count) = self.output.used_iovs();
            let mut cmsg = CmsgBuf::new();
            let mut clen: u32 = 0;
            let mut size = self.fds_out.size();
            if size > MAX_FDS * mem::size_of::<RawFd>() {
                size = MAX_FDS * mem::size_of::<RawFd>();
            }
            if size > 0 {
                unsafe {
                    let mut tmp: libc::msghdr = mem::zeroed();
                    tmp.msg_control = cmsg.as_mut_ptr() as *mut libc::c_void;
                    tmp.msg_controllen = cmsg.len() as _;
                    let c = libc::CMSG_FIRSTHDR(&tmp);
                    (*c).cmsg_level = libc::SOL_SOCKET;
                    (*c).cmsg_type = libc::SCM_RIGHTS;
                    (*c).cmsg_len = libc::CMSG_LEN(size as u32);
                    let mut tmp = [0u8; MAX_FDS * 4];
                    self.fds_out.copy_out(&mut tmp[..size]);
                    ptr::copy_nonoverlapping(tmp.as_ptr(), libc::CMSG_DATA(c), size);
                    clen = (*c).cmsg_len;
                }
            }
            let mut msg: libc::msghdr = unsafe { mem::zeroed() };
            msg.msg_iov = iovs.as_mut_ptr();
            msg.msg_iovlen = count as _;
            if clen > 0 {
                msg.msg_control = cmsg.as_mut_ptr() as *mut libc::c_void;
                msg.msg_controllen = clen as _;
            }
            let fd = self.raw_fd();
            let len = loop {
                let n = unsafe {
                    libc::sendmsg(fd, &msg, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT)
                };
                if n < 0 {
                    let e = errno_now();
                    if e == libc::EINTR {
                        continue;
                    }
                    return Err(e);
                }
                if n == 0 {
                    return Err(libc::EAGAIN);
                }
                break n as usize;
            };
            self.fds_out.close_fds();
            self.output.tail = self.output.tail.wrapping_add(len as u32);
        }
        self.want_flush = false;
        Ok(self.output.head.wrapping_sub(tail) as usize)
    }

    pub fn put(&mut self, data: &[u8]) -> Result<(), i32> {
        if self.output.size() + data.len() > CONNECTION_BUFFER_SIZE {
            self.want_flush = true;
            self.flush()?;
        }
        if self.output.size() + data.len() > CONNECTION_BUFFER_SIZE {
            return Err(libc::EOVERFLOW);
        }
        self.output.put(data)?;
        self.want_flush = true;
        Ok(())
    }

    pub fn put_fd(&mut self, fd: OwnedFd) -> Result<(), i32> {
        if self.fds_out.size() >= MAX_FDS * mem::size_of::<RawFd>() {
            return Err(libc::EOVERFLOW);
        }
        let raw = fd.as_raw_fd();
        let bytes = raw.to_ne_bytes();
        let r = self.fds_out.put(&bytes);
        if r.is_ok() {
            mem::forget(fd);
            Ok(())
        } else {
            Err(libc::EOVERFLOW)
        }
    }

    pub fn get(&mut self, dst: &mut [u8]) -> Result<usize, i32> {
        if dst.len() > self.input.size() {
            return Err(libc::EAGAIN);
        }
        self.input.copy_out(dst);
        self.input.consume(dst.len());
        Ok(dst.len())
    }

    pub fn get_fd(&mut self) -> Result<OwnedFd, i32> {
        if mem::size_of::<RawFd>() > self.fds_in.size() {
            return Err(libc::EAGAIN);
        }
        let mut bytes = [0u8; 4];
        self.fds_in.copy_out(&mut bytes);
        self.fds_in.consume(mem::size_of::<RawFd>());
        let raw = RawFd::from_ne_bytes(bytes);
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }

    pub fn close_fds(&mut self) {
        self.fds_in.close_fds();
        self.fds_out.close_fds();
    }

    pub fn take_fd(mut self) -> OwnedFd {
        self.close_fds();
        self.fd.take().unwrap()
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.close_fds();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;

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

    #[test]
    fn wire_conn_byte_round_trip() {
        let (mut a, mut b) = pair();
        a.put(&[85u8]).unwrap();
        assert_eq!(a.flush().unwrap(), 1);
        assert_eq!(b.read().unwrap(), 1);
        assert_eq!(b.pending(), 1);
        let mut out = [0u8; 1];
        assert_eq!(b.get(&mut out).unwrap(), 1);
        assert_eq!(out, [85u8]);
        assert_eq!(b.pending(), 0);
    }

    #[test]
    fn wire_conn_short_read_is_eagain() {
        let (mut a, mut b) = pair();
        a.put(&[85u8]).unwrap();
        a.flush().unwrap();
        b.read().unwrap();
        let mut big = [0u8; 4];
        assert_eq!(b.get(&mut big), Err(libc::EAGAIN));
        assert_eq!(b.pending(), 1);
        let mut one = [0u8; 1];
        assert_eq!(b.get(&mut one).unwrap(), 1);
        assert_eq!(one, [85u8]);
    }

    #[test]
    fn wire_conn_max_write() {
        let (mut a, mut b) = pair();
        let big = [7u8; CONNECTION_BUFFER_SIZE];
        a.put(&big).unwrap();
        assert_eq!(a.flush().unwrap(), CONNECTION_BUFFER_SIZE);
        assert_eq!(b.read().unwrap(), CONNECTION_BUFFER_SIZE);
        let mut out = [0u8; CONNECTION_BUFFER_SIZE];
        assert_eq!(b.get(&mut out).unwrap(), CONNECTION_BUFFER_SIZE);
        assert_eq!(out, big);
        assert_eq!(b.read(), Err(libc::EAGAIN));
    }

    #[test]
    fn wire_conn_put_overflow_fails_loudly() {
        let (mut a, _b) = pair();
        let big = [0u8; CONNECTION_BUFFER_SIZE + 1];
        assert_eq!(a.put(&big), Err(libc::EOVERFLOW));
    }

    #[test]
    fn ring_wraparound_preserves_order() {
        let mut r = Ring::new();
        let first = [1u8; 200];
        r.put(&first).unwrap();
        let mut out = [0u8; 200];
        r.copy_out(&mut out);
        assert_eq!(out, first);
        r.consume(200);
        assert_eq!(r.size(), 0);
        let second = [2u8; 200];
        r.put(&second).unwrap();
        let mut out2 = [0u8; 200];
        r.copy_out(&mut out2);
        assert_eq!(out2, second);
        r.consume(100);
        let third = [3u8; 100];
        r.put(&third).unwrap();
        let mut combined = [0u8; 200];
        r.copy_out(&mut combined);
        assert_eq!(&combined[..100], &[2u8; 100]);
        assert_eq!(&combined[100..], &[3u8; 100]);
    }

    #[test]
    fn ring_counters_wrap_without_loss() {
        let mut r = Ring::new();
        r.head = u32::MAX - 10;
        r.tail = u32::MAX - 10;
        assert_eq!(r.size(), 0);
        let data = [9u8; 10];
        r.put(&data).unwrap();
        assert_eq!(r.size(), 10);
        assert_eq!(r.head, u32::MAX);
        let more = [8u8; 5];
        r.put(&more).unwrap();
        assert_eq!(r.head, 4);
        assert_eq!(r.size(), 15);
        let mut out = [0u8; 15];
        r.copy_out(&mut out);
        assert_eq!(&out[..10], &[9u8; 10]);
        assert_eq!(&out[10..], &[8u8; 5]);
        r.consume(15);
        assert_eq!(r.size(), 0);
    }

    #[test]
    fn ring_restore_recovers_frame_header() {
        let mut r = Ring::new();
        r.put(&[1, 2, 3, 4]).unwrap();
        let mut hdr = [0u8; 4];
        r.copy_out(&mut hdr);
        r.consume(4);
        assert_eq!(r.size(), 0);
        r.restore(4);
        assert_eq!(r.size(), 4);
        let mut hdr2 = [0u8; 4];
        r.copy_out(&mut hdr2);
        assert_eq!(hdr, hdr2);
    }

    #[test]
    fn fd_round_trip_exactly_once() {
        let (mut a, mut b) = pair();
        let f = std::fs::File::open("/dev/null").unwrap();
        let raw = {
            use std::os::unix::io::AsRawFd;
            f.as_raw_fd()
        };
        let dup = unsafe { libc::dup(raw) };
        assert!(dup >= 0);
        a.put_fd(unsafe { OwnedFd::from_raw_fd(dup) }).unwrap();
        a.put(&[1u8]).unwrap();
        a.flush().unwrap();
        b.read().unwrap();
        assert_eq!(b.queued_fds(), 1);
        let received = b.get_fd().unwrap();
        assert!(received.as_raw_fd() >= 0);
        assert_eq!(b.queued_fds(), 0);
        assert!(b.get_fd().is_err());
    }

    #[test]
    fn eof_with_partial_frame_signals_dead() {
        let (mut a, mut b) = pair();
        a.put(&[1u8, 2u8]).unwrap();
        a.flush().unwrap();
        assert!(b.read().unwrap() >= 2);
        drop(a);
        assert_eq!(b.read(), Ok(0));
    }

    #[test]
    fn unexpected_fds_closed_on_drop() {
        let (mut a, mut b) = pair();
        for _ in 0..3 {
            let dup = unsafe { libc::dup(1) };
            assert!(dup >= 0);
            a.put_fd(unsafe { OwnedFd::from_raw_fd(dup) }).unwrap();
        }
        a.put(&[0u8]).unwrap();
        a.flush().unwrap();
        b.read().unwrap();
        assert_eq!(b.queued_fds(), 3);
        drop(b);
    }
}
