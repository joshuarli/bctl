use std::mem::MaybeUninit;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Stop(i32),
    Child,
}

pub struct Signals {
    set: libc::sigset_t,
}

impl Signals {
    pub fn block() -> Result<Signals, String> {
        let mut set = MaybeUninit::<libc::sigset_t>::uninit();
        let set = unsafe {
            libc::sigemptyset(set.as_mut_ptr());
            for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGCHLD] {
                libc::sigaddset(set.as_mut_ptr(), signal);
            }
            set.assume_init()
        };
        let result = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
        if result != 0 {
            return Err(format!("block signals: {}", std::io::Error::from_raw_os_error(result)));
        }
        Ok(Signals { set })
    }

    pub fn wait(&self, timeout: Duration) -> Option<Event> {
        let timeout = libc::timespec { tv_sec: timeout.as_secs() as _, tv_nsec: timeout.subsec_nanos() as _ };
        match unsafe { libc::sigtimedwait(&self.set, std::ptr::null_mut(), &timeout) } {
            libc::SIGCHLD => Some(Event::Child),
            signal if signal > 0 => Some(Event::Stop(signal)),
            _ => None,
        }
    }
}

pub fn send(pid: u32, signal: i32) -> std::io::Result<()> {
    if unsafe { libc::kill(pid as libc::pid_t, signal) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}
