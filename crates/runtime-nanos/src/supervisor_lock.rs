use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, Write};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

const LOCK_FILE: &str = "supervisor.lock";
const EXIT_GRACE: Duration = Duration::from_secs(2);
const TERM_GRACE: Duration = Duration::from_secs(10);
const KILL_GRACE: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(50);

/// Locks `dir`'s supervisor file for this process's lifetime and records its process group, which a supervisor
/// started with `process_group(0)` shares with the VMM it spawns.
pub(crate) fn hold(dir: &Path) -> Result<File, String> {
    let path = dir.join(LOCK_FILE);
    let mut file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(format!("another supervisor holds {}", path.display())),
        Err(TryLockError::Error(error)) => return Err(format!("{}: {error}", path.display())),
    }
    file.set_len(0).and_then(|()| writeln!(file, "{}", std::process::id())).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(file)
}

/// Stops the supervisor that holds `dir`'s lock, with its whole process group, and returns once the lock is free.
/// A free or missing lock means no supervisor is alive.
pub(crate) fn stop_holder(dir: &Path) -> Result<(), String> {
    let path = dir.join(LOCK_FILE);
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if is_free(&file, &path)? {
        return Ok(());
    }
    let group = read_group(&mut file, &path)?;
    for (signal, grace) in [(None, EXIT_GRACE), (Some("TERM"), TERM_GRACE), (Some("KILL"), KILL_GRACE)] {
        if let Some(signal) = signal {
            let _ = Command::new("/bin/kill").args(["-s", signal]).arg(format!("-{group}")).status();
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if is_free(&file, &path)? {
                return Ok(());
            }
            thread::sleep(TICK);
        }
    }
    Err(format!("supervisor process group {group} still holds {} after SIGKILL", path.display()))
}

fn is_free(file: &File, path: &Path) -> Result<bool, String> {
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            Ok(true)
        }
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => Err(format!("{}: {error}", path.display())),
    }
}

fn read_group(file: &mut File, path: &Path) -> Result<u32, String> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let mut text = String::new();
        file.rewind().and_then(|()| file.read_to_string(&mut text)).map_err(|error| format!("{}: {error}", path.display()))?;
        match text.trim().parse::<u32>() {
            Ok(group) if group > 1 => return Ok(group),
            _ if Instant::now() < deadline => thread::sleep(TICK),
            _ => return Err(format!("{} names no process group: {:?}", path.display(), text.trim())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_free_lock_stops_nothing() {
        let dir = scratch("nanos-lock-free");
        stop_holder(&dir).unwrap();
        drop(hold(&dir).unwrap());
        stop_holder(&dir).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_second_supervisor_cannot_take_a_held_lock() {
        let dir = scratch("nanos-lock-held");
        let held = hold(&dir).unwrap();
        assert!(hold(&dir).unwrap_err().contains("another supervisor"));
        drop(held);
        fs::remove_dir_all(dir).unwrap();
    }
}
