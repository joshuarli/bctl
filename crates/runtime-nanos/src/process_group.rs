use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};

const TERM_GRACE: Duration = Duration::from_secs(5);
const KILL_GRACE: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(50);

pub(crate) struct OwnedGroup {
    leader: Option<Child>,
    stranded: bool,
}

impl OwnedGroup {
    pub(crate) fn new(leader: Child) -> OwnedGroup {
        OwnedGroup { leader: Some(leader), stranded: false }
    }

    pub(crate) fn terminate(&mut self) -> Result<(), String> {
        let Some(leader) = self.leader.as_mut() else { return Ok(()) };
        let result = terminate(leader);
        self.stranded = result.is_err();
        result
    }

    pub(crate) fn disarm(mut self) -> Child {
        self.leader.take().expect("an armed group has a leader")
    }
}

impl Drop for OwnedGroup {
    fn drop(&mut self) {
        if !self.stranded {
            let _ = self.terminate();
        }
    }
}

fn group_pid(group: u32) -> Option<Pid> {
    i32::try_from(group).ok().and_then(Pid::from_raw)
}

fn signal_group(group: u32, signal: Signal) {
    if let Some(pid) = group_pid(group) {
        let _ = kill_process_group(pid, signal);
    }
}

// A leader that exited is reaped here, which keeps its process group id from being reused while members remain.
fn gone(leader: &mut Child, group: u32) -> bool {
    let _ = leader.try_wait();
    group_pid(group).is_none_or(|pid| test_kill_process_group(pid).is_err())
}

fn terminate(leader: &mut Child) -> Result<(), String> {
    let group = leader.id();
    for (signal, grace) in [(Signal::TERM, TERM_GRACE), (Signal::KILL, KILL_GRACE)] {
        if gone(leader, group) {
            return Ok(());
        }
        signal_group(group, signal);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            thread::sleep(TICK);
            if gone(leader, group) {
                return Ok(());
            }
        }
    }
    if gone(leader, group) {
        Ok(())
    } else {
        Err(format!("process group {group} still has members after SIGKILL"))
    }
}
