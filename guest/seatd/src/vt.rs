use crate::sys::errno_now;
use rustix::fd::OwnedFd;
use std::ffi::CString;
use std::os::unix::io::{AsRawFd, FromRawFd};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VtMode {
    mode: u8,
    waitv: u8,
    relsig: i16,
    acqsig: i16,
    frsig: i16,
}

#[repr(C)]
struct VtStat {
    v_active: u16,
    v_signal: u16,
    v_state: u16,
}

const VT_SETMODE: libc::c_int = 0x5602;
const VT_GETMODE: libc::c_int = 0x5601;
const VT_GETSTATE: libc::c_int = 0x5603;
const VT_RELDISP: libc::c_int = 0x5605;
const VT_ACTIVATE: libc::c_int = 0x5606;
const VT_AUTO: u8 = 0x00;
const VT_PROCESS: u8 = 0x01;
const VT_ACKACQ: libc::c_int = 0x02;
const KDSKBMODE: libc::c_int = 0x4B45;
const KDGKBMODE: libc::c_int = 0x4B44;
const K_OFF: libc::c_int = 0x04;
const K_UNICODE: libc::c_int = 0x03;
const KDSETMODE: libc::c_int = 0x4B3A;
const KDGETMODE: libc::c_int = 0x4B3B;
const KD_GRAPHICS: libc::c_int = 0x01;
const KD_TEXT: libc::c_int = 0x00;

pub fn terminal_open(vt: i32) -> Result<OwnedFd, i32> {
    if vt < 0 {
        return Err(libc::EINVAL);
    }
    let path = format!("/dev/tty{vt}");
    let c = CString::new(path).map_err(|_| libc::EINVAL)?;
    unsafe {
        let fd = libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        if fd < 0 {
            Err(errno_now())
        } else {
            Ok(OwnedFd::from_raw_fd(fd))
        }
    }
}

fn process_mode() -> VtMode {
    VtMode {
        mode: VT_PROCESS,
        waitv: 0,
        relsig: libc::SIGUSR1 as i16,
        acqsig: libc::SIGUSR2 as i16,
        frsig: 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsoleMode {
    Vt(VtMode),
    Keyboard(libc::c_int),
    Display(libc::c_int),
}

fn snapshot(fd: libc::c_int) -> Result<[ConsoleMode; 3], i32> {
    let mut vt: VtMode = unsafe { std::mem::zeroed() };
    let mut keyboard: libc::c_int = 0;
    let mut display: libc::c_int = 0;
    if unsafe { libc::ioctl(fd, VT_GETMODE, &mut vt) } < 0 {
        return Err(errno_now());
    }
    if unsafe { libc::ioctl(fd, KDGKBMODE, &mut keyboard) } < 0 {
        return Err(errno_now());
    }
    if unsafe { libc::ioctl(fd, KDGETMODE, &mut display) } < 0 {
        return Err(errno_now());
    }
    Ok([ConsoleMode::Vt(vt), ConsoleMode::Keyboard(keyboard), ConsoleMode::Display(display)])
}

fn set_mode(fd: libc::c_int, mode: ConsoleMode) -> Result<(), i32> {
    let result = match mode {
        ConsoleMode::Vt(mode) => unsafe { libc::ioctl(fd, VT_SETMODE, &mode) },
        ConsoleMode::Keyboard(mode) => unsafe { libc::ioctl(fd, KDSKBMODE, mode) },
        ConsoleMode::Display(mode) => unsafe { libc::ioctl(fd, KDSETMODE, mode) },
    };
    if result < 0 { Err(errno_now()) } else { Ok(()) }
}

fn enter_graphics(previous: [ConsoleMode; 3], mut apply: impl FnMut(ConsoleMode) -> Result<(), i32>) -> Result<(), i32> {
    let graphics = [ConsoleMode::Vt(process_mode()), ConsoleMode::Keyboard(K_OFF), ConsoleMode::Display(KD_GRAPHICS)];
    for (index, mode) in graphics.into_iter().enumerate() {
        if let Err(error) = apply(mode) {
            for &mode in previous[..=index].iter().rev() {
                if let Err(e) = apply(mode) {
                    crate::log_error!("Could not restore {mode:?}: {}", std::io::Error::from_raw_os_error(e));
                }
            }
            return Err(error);
        }
    }
    Ok(())
}

pub fn open_vt(session: i32) -> Result<(), i32> {
    let tty = terminal_open(session)?;
    let fd = tty.as_raw_fd();
    enter_graphics(snapshot(fd)?, |mode| set_mode(fd, mode))
}

pub fn close_vt(session: i32) -> Result<(), i32> {
    let tty = terminal_open(session)?;
    let fd = tty.as_raw_fd();
    let mode = VtMode {
        mode: VT_AUTO,
        waitv: 0,
        relsig: 0,
        acqsig: 0,
        frsig: 0,
    };
    let mut result = Ok(());
    for mode in [ConsoleMode::Vt(mode), ConsoleMode::Keyboard(K_UNICODE), ConsoleMode::Display(KD_TEXT)] {
        if let Err(e) = set_mode(fd, mode) {
            crate::log_error!("Could not restore {mode:?}: {}", std::io::Error::from_raw_os_error(e));
            if result.is_ok() {
                result = Err(e);
            }
        }
    }
    result
}

pub fn switch_vt(cur_vt: i32, target: i32) -> Result<(), i32> {
    let tty = terminal_open(cur_vt)?;
    let fd = tty.as_raw_fd();
    let mode = process_mode();
    let r0 = unsafe { libc::ioctl(fd, VT_SETMODE, &mode) };
    if r0 < 0 {
        return Err(errno_now());
    }
    let r = unsafe { libc::ioctl(fd, VT_ACTIVATE, target) };
    if r < 0 {
        Err(errno_now())
    } else {
        Ok(())
    }
}

pub fn ack_vt(cur_vt: i32, release: bool) -> Result<(), i32> {
    let tty = terminal_open(cur_vt)?;
    let fd = tty.as_raw_fd();
    let arg = if release { 1 } else { VT_ACKACQ };
    let r = unsafe { libc::ioctl(fd, VT_RELDISP, arg) };
    if r < 0 {
        Err(errno_now())
    } else {
        Ok(())
    }
}

pub fn current_vt() -> Result<i32, i32> {
    let tty = terminal_open(0)?;
    let fd = tty.as_raw_fd();
    let mut st: VtStat = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(fd, VT_GETSTATE, &mut st) };
    if r < 0 {
        Err(errno_now())
    } else {
        Ok(st.v_active as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vt_setup_failure_restores_previous_modes() {
        let original = [
            ConsoleMode::Vt(VtMode { mode: VT_AUTO, waitv: 0, relsig: 0, acqsig: 0, frsig: 0 }),
            ConsoleMode::Keyboard(1),
            ConsoleMode::Display(KD_TEXT),
        ];
        for failing_step in 0..3 {
            let mut state = original;
            let mut calls = 0;
            let result = enter_graphics(original, |mode| {
                let fails = calls == failing_step;
                calls += 1;
                if fails {
                    return Err(libc::EIO);
                }
                match mode {
                    ConsoleMode::Vt(_) => state[0] = mode,
                    ConsoleMode::Keyboard(_) => state[1] = mode,
                    ConsoleMode::Display(_) => state[2] = mode,
                }
                Ok(())
            });
            assert_eq!(result, Err(libc::EIO));
            assert_eq!(state, original, "failed step {failing_step}");
        }
    }

    #[test]
    fn vt_rollback_failure_preserves_setup_error() {
        let original = [
            ConsoleMode::Vt(VtMode { mode: VT_AUTO, waitv: 0, relsig: 0, acqsig: 0, frsig: 0 }),
            ConsoleMode::Keyboard(K_UNICODE),
            ConsoleMode::Display(KD_TEXT),
        ];
        let mut calls = Vec::new();
        let result = enter_graphics(original, |mode| {
            calls.push(mode);
            match calls.len() {
                3 => Err(libc::EIO),
                4 => Err(libc::EPERM),
                _ => Ok(()),
            }
        });
        assert_eq!(result, Err(libc::EIO));
        assert_eq!(&calls[3..], &[original[2], original[1], original[0]]);
    }

    #[test]
    fn vt_terminal_open_rejects_negative() {
        assert_eq!(terminal_open(-1).unwrap_err(), libc::EINVAL);
    }

    #[test]
    fn vt_terminal_open_missing_fails_loudly() {
        let r = terminal_open(999);
        assert!(r.is_err());
        assert_ne!(r.unwrap_err(), 0);
    }

    #[test]
    fn vt_ops_fail_loudly_without_vt() {
        assert!(open_vt(999).is_err());
        assert!(close_vt(999).is_err());
        assert!(switch_vt(999, 1).is_err());
        assert!(ack_vt(999, true).is_err());
        let _ = current_vt();
    }
}
