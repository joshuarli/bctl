use crate::policy::validate_opened;
use crate::session::DeviceType;
use crate::sys;
use crate::vt;
use rustix::fd::OwnedFd;
use std::ffi::CString;
use std::os::unix::io::{FromRawFd, RawFd};

fn errno_now() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EINVAL)
}

pub trait Effect {
    fn open_device(&mut self, path: &str, dev_type: DeviceType) -> Result<OwnedFd, i32>;
    fn activate_device(&mut self, fd: RawFd, dev_type: DeviceType) -> Result<(), i32>;
    fn deactivate_device(&mut self, fd: RawFd, dev_type: DeviceType) -> Result<(), i32>;
    fn vt_open(&mut self, session: i32) -> Result<(), i32>;
    fn vt_close(&mut self, session: i32) -> Result<(), i32>;
    fn vt_switch(&mut self, cur_vt: i32, target: i32) -> Result<(), i32>;
    fn vt_ack(&mut self, cur_vt: i32, release: bool) -> Result<(), i32>;
    fn current_vt(&mut self) -> Result<i32, i32>;
}

pub struct RealEffect;

impl Effect for RealEffect {
    fn open_device(&mut self, path: &str, dev_type: DeviceType) -> Result<OwnedFd, i32> {
        let c = CString::new(path).map_err(|_| libc::ENOENT)?;
        unsafe {
            let fd = libc::open(
                c.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            );
            if fd < 0 {
                return Err(errno_now());
            }
            if dev_type == DeviceType::Drm && sys::drm_set_master(fd).is_err() {
                crate::log_error!("Could not make device fd drm master, continuing");
            }
            if let Err(e) = validate_opened(fd, path) {
                libc::close(fd);
                return Err(e);
            }
            Ok(OwnedFd::from_raw_fd(fd))
        }
    }

    fn activate_device(&mut self, fd: RawFd, dev_type: DeviceType) -> Result<(), i32> {
        match dev_type {
            DeviceType::Drm => {
                if sys::drm_set_master(fd).is_err() {
                    crate::log_error!("Could not make device fd drm master, continuing");
                }
                Ok(())
            }
            DeviceType::Evdev | DeviceType::Hidraw => Err(libc::EINVAL),
            DeviceType::Wscons => Ok(()),
        }
    }

    fn deactivate_device(&mut self, fd: RawFd, dev_type: DeviceType) -> Result<(), i32> {
        match dev_type {
            DeviceType::Drm => sys::drm_drop_master(fd),
            DeviceType::Evdev => sys::evdev_revoke(fd),
            DeviceType::Hidraw => sys::hidraw_revoke(fd),
            DeviceType::Wscons => Ok(()),
        }
    }

    fn vt_open(&mut self, session: i32) -> Result<(), i32> {
        vt::open_vt(session)
    }

    fn vt_close(&mut self, session: i32) -> Result<(), i32> {
        vt::close_vt(session)
    }

    fn vt_switch(&mut self, cur_vt: i32, target: i32) -> Result<(), i32> {
        vt::switch_vt(cur_vt, target)
    }

    fn vt_ack(&mut self, cur_vt: i32, release: bool) -> Result<(), i32> {
        vt::ack_vt(cur_vt, release)
    }

    fn current_vt(&mut self) -> Result<i32, i32> {
        vt::current_vt()
    }
}

pub struct NullEffect {
    pub cur_vt: i32,
    pub calls: Vec<String>,
}

impl NullEffect {
    pub fn new(cur_vt: i32) -> NullEffect {
        NullEffect {
            cur_vt,
            calls: Vec::new(),
        }
    }

    pub fn null_fd() -> OwnedFd {
        unsafe {
            let fd = libc::open(
                c"/dev/null".as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC,
            );
            assert!(fd >= 0);
            OwnedFd::from_raw_fd(fd)
        }
    }
}

impl Effect for NullEffect {
    fn open_device(&mut self, path: &str, _dev_type: DeviceType) -> Result<OwnedFd, i32> {
        self.calls.push(format!("open:{path}"));
        Ok(Self::null_fd())
    }

    fn activate_device(&mut self, _fd: RawFd, _dev_type: DeviceType) -> Result<(), i32> {
        self.calls.push("activate".to_string());
        Ok(())
    }

    fn deactivate_device(&mut self, _fd: RawFd, _dev_type: DeviceType) -> Result<(), i32> {
        self.calls.push("deactivate".to_string());
        Ok(())
    }

    fn vt_open(&mut self, session: i32) -> Result<(), i32> {
        self.calls.push(format!("vt_open:{session}"));
        Ok(())
    }

    fn vt_close(&mut self, session: i32) -> Result<(), i32> {
        self.calls.push(format!("vt_close:{session}"));
        Ok(())
    }

    fn vt_switch(&mut self, cur_vt: i32, target: i32) -> Result<(), i32> {
        self.calls.push(format!("vt_switch:{cur_vt}->{target}"));
        self.cur_vt = target;
        Ok(())
    }

    fn vt_ack(&mut self, cur_vt: i32, release: bool) -> Result<(), i32> {
        self.calls.push(format!("vt_ack:{cur_vt}:{}", release as u8));
        Ok(())
    }

    fn current_vt(&mut self) -> Result<i32, i32> {
        Ok(self.cur_vt)
    }
}

