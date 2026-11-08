use crate::session::DeviceType;
use std::ffi::{CStr, CString};
use std::os::unix::io::RawFd;

fn errno_now() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EINVAL)
}

pub fn classify_linux(path: &str) -> Option<DeviceType> {
    if path.starts_with("/dev/input/event") {
        Some(DeviceType::Evdev)
    } else if path.starts_with("/dev/dri/") {
        Some(DeviceType::Drm)
    } else if path.starts_with("/dev/hidraw") {
        Some(DeviceType::Hidraw)
    } else {
        None
    }
}

pub fn canonicalize(path: &str) -> Result<String, i32> {
    let c = CString::new(path).map_err(|_| libc::ENOENT)?;
    unsafe {
        let resolved = libc::realpath(c.as_ptr(), std::ptr::null_mut());
        if resolved.is_null() {
            return Err(errno_now());
        }
        let s = CStr::from_ptr(resolved).to_string_lossy().into_owned();
        libc::free(resolved as *mut libc::c_void);
        Ok(s)
    }
}

pub fn resolve_device(path: &str) -> Result<(String, DeviceType), i32> {
    let sanitized = canonicalize(path)?;
    let dev_type = classify_linux(&sanitized).ok_or(libc::ENOENT)?;
    Ok((sanitized, dev_type))
}

fn stat_path(path: &str) -> Result<libc::stat, i32> {
    let c = CString::new(path).map_err(|_| libc::ENOENT)?;
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::stat(c.as_ptr(), &mut st) != 0 {
            return Err(errno_now());
        }
        Ok(st)
    }
}

fn fstat_fd(fd: RawFd) -> Result<libc::stat, i32> {
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut st) != 0 {
            return Err(errno_now());
        }
        Ok(st)
    }
}

pub fn check_char(fd: RawFd) -> Result<(), i32> {
    let st = fstat_fd(fd)?;
    if st.st_mode & libc::S_IFMT != libc::S_IFCHR {
        return Err(libc::ENODEV);
    }
    Ok(())
}

pub fn check_identity(fd: RawFd, path: &str) -> Result<(), i32> {
    let pst = stat_path(path)?;
    let fst = fstat_fd(fd)?;
    if pst.st_dev != fst.st_dev || pst.st_ino != fst.st_ino {
        return Err(libc::EACCES);
    }
    Ok(())
}

pub fn validate_opened(fd: RawFd, sanitized: &str) -> Result<(), i32> {
    check_identity(fd, sanitized)?;
    check_char(fd)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    fn open_null() -> (std::fs::File, RawFd) {
        let f = std::fs::File::open("/dev/null").unwrap();
        let raw = f.as_raw_fd();
        (f, raw)
    }

    #[test]
    fn device_classify_prefixes() {
        assert_eq!(classify_linux("/dev/input/event0"), Some(DeviceType::Evdev));
        assert_eq!(classify_linux("/dev/input/event99"), Some(DeviceType::Evdev));
        assert_eq!(classify_linux("/dev/dri/card0"), Some(DeviceType::Drm));
        assert_eq!(classify_linux("/dev/hidraw0"), Some(DeviceType::Hidraw));
        assert_eq!(classify_linux("/dev/null"), None);
        assert_eq!(classify_linux("/dev/drm/card0"), None);
        assert_eq!(classify_linux("/dev/input/event"), Some(DeviceType::Evdev));
        assert_eq!(classify_linux("/dev/input/event0x"), Some(DeviceType::Evdev));
    }

    #[test]
    fn device_canonicalize_missing_is_enoent() {
        let r = canonicalize("/dev/input/event-does-not-exist-xyz");
        assert_eq!(r.unwrap_err(), libc::ENOENT);
    }

    #[test]
    fn device_canonicalize_resolves() {
        assert_eq!(canonicalize("/dev/null").unwrap(), "/dev/null");
    }

    #[test]
    fn device_check_char_accepts_char_rejects_regular() {
        let (_f, raw) = open_null();
        assert_eq!(check_char(raw), Ok(()));
        let dir = ".work/tmp/policy-probe";
        std::fs::create_dir_all(dir).unwrap();
        let p = format!("{dir}/regular");
        std::fs::write(&p, b"x").unwrap();
        let f = std::fs::File::open(&p).unwrap();
        assert_eq!(check_char(f.as_raw_fd()), Err(libc::ENODEV));
    }

    #[test]
    fn device_check_identity_binds_path_to_fd() {
        let (_f, raw) = open_null();
        assert_eq!(check_identity(raw, "/dev/null"), Ok(()));
        assert_eq!(check_identity(raw, "/dev/zero"), Err(libc::EACCES));
        assert_eq!(check_identity(raw, "/no/such/path-xyz"), Err(libc::ENOENT));
    }

    #[test]
    fn device_validate_opened_orders_identity_before_type() {
        let dir = ".work/tmp/policy-probe";
        std::fs::create_dir_all(dir).unwrap();
        let p = format!("{dir}/regular");
        std::fs::write(&p, b"x").unwrap();
        let f = std::fs::File::open(&p).unwrap();
        assert_eq!(validate_opened(f.as_raw_fd(), &p), Err(libc::ENODEV));
    }

    #[test]
    fn device_resolve_rejects_unsupported() {
        assert_eq!(resolve_device("/dev/null").unwrap_err(), libc::ENOENT);
        assert_eq!(resolve_device("/does/not/exist-xyz").unwrap_err(), libc::ENOENT);
    }
}
