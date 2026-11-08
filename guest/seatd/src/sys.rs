use std::os::unix::io::RawFd;

pub(crate) fn errno_now() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EINVAL)
}

pub fn drm_set_master(fd: RawFd) -> Result<(), i32> {
    let ret = unsafe { libc::ioctl(fd, libc::_IO(b'd' as u32, 0x1e) as _, 0) };
    if ret < 0 {
        Err(errno_now())
    } else {
        Ok(())
    }
}

pub fn drm_drop_master(fd: RawFd) -> Result<(), i32> {
    let ret = unsafe { libc::ioctl(fd, libc::_IO(b'd' as u32, 0x1f) as _, 0) };
    if ret < 0 {
        Err(errno_now())
    } else {
        Ok(())
    }
}

fn iow(type_: u8, nr: u8) -> libc::c_int {
    (0x40000000u32 | ((std::mem::size_of::<libc::c_int>() as u32) << 16) | ((type_ as u32) << 8) | nr as u32) as libc::c_int
}

pub fn evdev_revoke(fd: RawFd) -> Result<(), i32> {
    let ret = unsafe { libc::ioctl(fd, iow(b'E', 0x91), 0) };
    if ret < 0 {
        Err(errno_now())
    } else {
        Ok(())
    }
}

pub fn hidraw_revoke(fd: RawFd) -> Result<(), i32> {
    let ret = unsafe { libc::ioctl(fd, iow(b'H', 0x0d), 0) };
    if ret < 0 {
        Err(errno_now())
    } else {
        Ok(())
    }
}
