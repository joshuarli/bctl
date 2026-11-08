use crate::effect::Effect;
use crate::policy::resolve_device;
use crate::session::{errno_last, ClientSlot, ClientState, DeviceType, SeatDevice};
use crate::wire::MAX_SEAT_DEVICES;
use rustix::fd::OwnedFd;
use std::os::unix::io::{AsRawFd, FromRawFd};

pub fn find_device(slots: &[ClientSlot], idx: usize, device_id: i32) -> Result<usize, i32> {
    if device_id == 0 {
        return Err(libc::EINVAL);
    }
    for (i, dev) in slots[idx].client.devices.iter().enumerate() {
        if dev.device_id == device_id {
            return Ok(i);
        }
    }
    Err(libc::ENOENT)
}

pub fn deactivate_device(
    slots: &mut [ClientSlot],
    idx: usize,
    dev: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    let active = slots[idx].client.devices[dev].active;
    if !active {
        return Ok(());
    }
    let (fd, dev_type) = {
        let d = &slots[idx].client.devices[dev];
        (d.fd.as_ref().unwrap().as_raw_fd(), d.dev_type)
    };
    let r = fx.deactivate_device(fd, dev_type);
    if r.is_ok() {
        slots[idx].client.devices[dev].active = false;
    }
    r
}

pub fn close_device(slots: &mut [ClientSlot], idx: usize, dev: usize, fx: &mut dyn Effect) {
    let dev_rc = slots[idx].client.devices[dev].ref_cnt;
    if dev_rc > 1 {
        slots[idx].client.devices[dev].ref_cnt -= 1;
        return;
    }
    let saved = errno_last();
    let mut dev = slots[idx].client.devices.remove(dev);
    if dev.fd.is_some() {
        let raw = dev.fd.as_ref().unwrap().as_raw_fd();
        if dev.active {
            if let Err(e) = fx.deactivate_device(raw, dev.dev_type) {
                crate::log_error!("Could not deactivate device {}: {}", dev.path, std::io::Error::from_raw_os_error(e));
            }
        }
        dev.fd.take();
    }
    unsafe {
        *libc::__errno_location() = saved;
    }
}

pub fn activate_device(
    slots: &mut [ClientSlot],
    idx: usize,
    dev: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if slots[idx].client.devices[dev].active {
        return Ok(());
    }
    let (fd, dev_type) = {
        let d = &slots[idx].client.devices[dev];
        (d.fd.as_ref().unwrap().as_raw_fd(), d.dev_type)
    };
    fx.activate_device(fd, dev_type)?;
    slots[idx].client.devices[dev].active = true;
    Ok(())
}

pub fn open_device(
    active_client: Option<usize>,
    slots: &mut [ClientSlot],
    idx: usize,
    path: &str,
    fx: &mut dyn Effect,
) -> Result<(i32, OwnedFd), i32> {
    if slots[idx].client.state != ClientState::Active {
        return Err(libc::EPERM);
    }
    let (sanitized, dev_type) = resolve_device(path)?;
    open_device_resolved(active_client, slots, idx, &sanitized, dev_type, fx)
}

pub fn open_device_resolved(
    active_client: Option<usize>,
    slots: &mut [ClientSlot],
    idx: usize,
    sanitized: &str,
    dev_type: DeviceType,
    fx: &mut dyn Effect,
) -> Result<(i32, OwnedFd), i32> {
    if slots[idx].client.state != ClientState::Active {
        return Err(libc::EPERM);
    }
    assert_eq!(active_client, Some(idx));
    let mut device_id = 1i32;
    let mut count = 0usize;
    for dev in slots[idx].client.devices.iter_mut() {
        if dev.path == sanitized {
            dev.ref_cnt += 1;
            let dup = unsafe { libc::dup(dev.fd.as_ref().unwrap().as_raw_fd()) };
            if dup < 0 {
                dev.ref_cnt -= 1;
                return Err(errno_last());
            }
            return Ok((dev.device_id, unsafe { OwnedFd::from_raw_fd(dup) }));
        }
        if dev.device_id >= device_id {
            device_id = dev.device_id + 1;
        }
        count += 1;
    }
    if count >= MAX_SEAT_DEVICES {
        return Err(libc::EMFILE);
    }
    let fd = fx.open_device(sanitized, dev_type)?;
    slots[idx].client.devices.push(SeatDevice {
        device_id,
        fd: Some(fd),
        ref_cnt: 1,
        active: true,
        path: sanitized.to_string(),
        dev_type,
    });
    let dup = unsafe {
        libc::dup(
            slots[idx].client.devices.last().unwrap().fd.as_ref().unwrap().as_raw_fd(),
        )
    };
    if dup < 0 {
        let e = errno_last();
        let n = slots[idx].client.devices.len() - 1;
        close_device(slots, idx, n, fx);
        return Err(e);
    }
    Ok((device_id, unsafe { OwnedFd::from_raw_fd(dup) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::Conn;
    use crate::effect::NullEffect;
    use crate::session::{Client, Seat};
    use std::os::unix::io::FromRawFd;

    struct FailFx {
        null: NullEffect,
    }

    impl FailFx {
        fn new() -> (Seat, Vec<ClientSlot>, FailFx) {
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
                let mut slots = vec![ClientSlot {
                    conn: Conn::new(OwnedFd::from_raw_fd(fds[0])),
                    client: Client::fresh(1, 1, 1),
                    dead: false,
                }];
                slots[0].client.state = ClientState::Active;
                slots[0].client.seat = Some(0);
                let mut seat = Seat::new("seat0", false);
                seat.clients.push(0);
                seat.active_client = Some(0);
                (
                    seat,
                    slots,
                    FailFx {
                        null: NullEffect::new(0),
                    },
                )
            }
        }
    }

    impl Effect for FailFx {
        fn open_device(&mut self, path: &str, t: DeviceType) -> Result<OwnedFd, i32> {
            self.null.open_device(path, t)
        }

        fn activate_device(
            &mut self,
            _fd: std::os::unix::io::RawFd,
            _t: DeviceType,
        ) -> Result<(), i32> {
            Err(libc::EINVAL)
        }

        fn deactivate_device(
            &mut self,
            _fd: std::os::unix::io::RawFd,
            _t: DeviceType,
        ) -> Result<(), i32> {
            Err(libc::EIO)
        }

        fn vt_open(&mut self, s: i32) -> Result<(), i32> {
            self.null.vt_open(s)
        }

        fn vt_close(&mut self, s: i32) -> Result<(), i32> {
            self.null.vt_close(s)
        }

        fn vt_switch(&mut self, c: i32, t: i32) -> Result<(), i32> {
            self.null.vt_switch(c, t)
        }

        fn vt_ack(&mut self, c: i32, r: bool) -> Result<(), i32> {
            self.null.vt_ack(c, r)
        }

        fn current_vt(&mut self) -> Result<i32, i32> {
            self.null.current_vt()
        }
    }

    #[test]
    fn device_teardown_reports_revocation_failure() {
        let marker = "SEATD_TEST_REVOKE_FAILURE";
        if std::env::var_os(marker).is_some() {
            let (_seat, mut slots, mut fx) = FailFx::new();
            open_device_resolved(Some(0), &mut slots, 0, "/dev/input/event-test", DeviceType::Evdev, &mut fx).unwrap();
            close_device(&mut slots, 0, 0, &mut fx);
            assert!(slots[0].client.devices.is_empty());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "device::tests::device_teardown_reports_revocation_failure", "--nocapture"])
            .env(marker, "1").output().unwrap();
        assert!(output.status.success());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(diagnostic.contains("Could not deactivate device /dev/input/event-test"), "{diagnostic}");
        assert!(diagnostic.contains(&std::io::Error::from_raw_os_error(libc::EIO).to_string()), "{diagnostic}");
    }

    #[test]
    fn device_deactivate_failure_keeps_active() {
        let (_seat, mut slots, mut fx) = FailFx::new();
        let (id, _fd) =
            open_device_resolved(Some(0), &mut slots, 0, "/dev/input/event0", DeviceType::Evdev, &mut fx)
                .unwrap();
        assert_eq!(id, 1);
        assert!(slots[0].client.devices[0].active);
        assert_eq!(deactivate_device(&mut slots, 0, 0, &mut fx), Err(libc::EIO));
        assert!(slots[0].client.devices[0].active);
        assert_eq!(activate_device(&mut slots, 0, 0, &mut fx), Ok(()));
        slots[0].client.devices[0].active = false;
        assert_eq!(activate_device(&mut slots, 0, 0, &mut fx), Err(libc::EINVAL));
        assert!(!slots[0].client.devices[0].active);
    }

    #[test]
    fn device_ids_skip_closed() {
        let (_seat, mut slots, mut fx) = FailFx::new();
        let (a, _f) =
            open_device_resolved(Some(0), &mut slots, 0, "/dev/input/event0", DeviceType::Evdev, &mut fx)
                .unwrap();
        let (b, _f) =
            open_device_resolved(Some(0), &mut slots, 0, "/dev/input/event1", DeviceType::Evdev, &mut fx)
                .unwrap();
        assert_eq!((a, b), (1, 2));
        close_device(&mut slots, 0, 0, &mut fx);
        let (c, _f) =
            open_device_resolved(Some(0), &mut slots, 0, "/dev/input/event2", DeviceType::Evdev, &mut fx)
                .unwrap();
        assert_eq!(c, 3);
        assert!(find_device(&slots, 0, 1).is_err());
        assert!(find_device(&slots, 0, 0).is_err());
        assert!(find_device(&slots, 0, 2).is_ok());
    }

    #[test]
    fn device_open_rejects_inactive_client() {
        let (_seat, mut slots, mut fx) = FailFx::new();
        slots[0].client.state = ClientState::New;
        assert_eq!(
            open_device_resolved(Some(0), &mut slots, 0, "/dev/input/event0", DeviceType::Evdev, &mut fx)
                .unwrap_err(),
            libc::EPERM
        );
        assert_eq!(
            open_device(Some(0), &mut slots, 0, "/dev/input/event0", &mut fx).unwrap_err(),
            libc::EPERM
        );
    }
}
