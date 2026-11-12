use std::io;
use std::mem::MaybeUninit;
use std::os::unix::process::CommandExt;
use std::process::Command;
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

    pub fn unblock_in(&self, command: &mut Command) {
        let set = self.set;
        unsafe {
            command.pre_exec(move || {
                if libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
            });
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn child_blocks_term(command: &mut Command) -> bool {
        let out = command.args(["-c", "sed -n 's/^SigBlk:\\t//p' /proc/self/status"]).output().unwrap();
        let mask = u64::from_str_radix(String::from_utf8(out.stdout).unwrap().trim(), 16).unwrap();
        mask & (1 << (libc::SIGTERM - 1)) != 0
    }

    #[test]
    fn a_child_spawned_with_unblock_in_receives_stop_signals() {
        let signals = Signals::block().unwrap();
        assert!(child_blocks_term(&mut Command::new("sh")), "a plain child inherits the blocked mask");
        let mut command = Command::new("sh");
        signals.unblock_in(&mut command);
        assert!(!child_blocks_term(&mut command));
    }
}
