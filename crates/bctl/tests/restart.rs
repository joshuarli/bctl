use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const FAKE_SMOLVM: &str = r#"#!/bin/sh
ctl="$(dirname "$0")/ctl"
case "$1 $2" in
"machine ls")
    touch "$ctl/listing"
    while [ ! -e "$ctl/release" ] && [ -d "$ctl" ]; do sleep 0.02; done
    echo '[]'
    ;;
esac
exit 0
"#;

struct Home {
    path: PathBuf,
    daemon: Option<Child>,
}

impl Home {
    fn new(tag: &str) -> Home {
        let path = std::env::temp_dir().join(format!("bctl-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        let ctl = path.join("runtime/smolvm/ctl");
        fs::create_dir_all(&ctl).unwrap();
        let binary = path.join("runtime/smolvm/smolvm");
        fs::write(&binary, FAKE_SMOLVM).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        Home { path, daemon: None }
    }

    fn socket(&self) -> PathBuf {
        self.path.join("bctl.sock")
    }

    fn lock_path(&self) -> PathBuf {
        self.path.join("daemon.lock")
    }

    fn flag(&self, name: &str) -> PathBuf {
        self.path.join("runtime/smolvm/ctl").join(name)
    }

    fn hold_lock(&self) -> File {
        let lock = File::create(self.lock_path()).unwrap();
        lock.try_lock().unwrap();
        lock
    }

    fn command(&self, binary: &str) -> Command {
        let mut command = Command::new(binary);
        command.env("BCTL_HOME", &self.path).env("BCTL_RUNTIME", "smolvm");
        command
    }

    fn client(&self) -> Child {
        self.command(env!("CARGO_BIN_EXE_bctl")).args(["runs", "1", "--json"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()
    }

    fn start_daemon(&mut self) {
        let daemon = self.command(env!("CARGO_BIN_EXE_bctl")).arg("daemon").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        self.daemon = Some(daemon);
    }

    fn locked(&self) -> bool {
        File::open(self.lock_path()).is_ok_and(|lock| lock.try_lock().is_err())
    }

    fn daemon_pids(&self) -> Vec<String> {
        let output = Command::new("lsof").arg("-t").arg(self.lock_path()).output().unwrap();
        String::from_utf8_lossy(&output.stdout).split_whitespace().map(str::to_string).collect()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::write(self.flag("release"), b"");
        if let Some(daemon) = &mut self.daemon {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        for pid in self.daemon_pids() {
            let _ = Command::new("kill").arg(pid).status();
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn finish(client: Child) -> (Output, String) {
    let output = client.wait_with_output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    (output, text)
}

fn accept_and_read_one_byte(listener: &UnixListener) -> UnixStream {
    let (mut stream, _) = listener.accept().unwrap();
    stream.read_exact(&mut [0u8; 1]).unwrap();
    stream
}

fn accepted_connections(listener: &UnixListener) -> usize {
    listener.set_nonblocking(true).unwrap();
    std::iter::from_fn(|| listener.accept().ok()).count()
}

fn bind(path: &Path) -> UnixListener {
    UnixListener::bind(path).unwrap()
}

#[test]
fn a_command_waits_out_a_reset_from_the_dead_daemons_socket_while_the_new_daemon_starts() {
    let mut home = Home::new("w");
    let old_lock = home.hold_lock();
    let old_listener = bind(&home.socket());

    let client = home.client();
    let in_flight = accept_and_read_one_byte(&old_listener);

    drop(old_lock);
    home.start_daemon();
    wait_for("the new daemon to be restoring under the lock", || home.locked() && home.flag("listing").exists());

    drop(in_flight);
    drop(old_listener);
    thread::sleep(Duration::from_millis(300));
    fs::write(home.flag("release"), b"").unwrap();

    let (_, text) = finish(client);
    assert!(!text.contains("DAEMON_UNREACHABLE"), "the command gave up on the restarting daemon: {text}");
    assert!(text.contains("NO_SUCH_RUN"), "the command got no answer from the new daemon: {text}");
}

#[test]
fn a_command_starts_a_daemon_when_a_reset_comes_from_a_socket_nobody_owns() {
    let home = Home::new("f");
    fs::write(home.flag("release"), b"").unwrap();
    File::create(home.lock_path()).unwrap();
    let old_listener = bind(&home.socket());

    let client = home.client();
    let in_flight = accept_and_read_one_byte(&old_listener);
    drop(in_flight);
    drop(old_listener);

    let (_, text) = finish(client);
    assert!(text.contains("NO_SUCH_RUN"), "the command got no answer from the new daemon: {text}");
    assert_eq!(home.daemon_pids().len(), 1, "exactly one daemon should own the lock");
}

#[test]
fn a_request_the_daemon_read_before_dying_is_never_sent_again() {
    let home = Home::new("r");
    let _lock = home.hold_lock();
    let listener = bind(&home.socket());

    let client = home.client();
    let (stream, _) = listener.accept().unwrap();
    let mut request = String::new();
    BufReader::new(&stream).read_line(&mut request).unwrap();
    assert!(request.contains("\"op\""), "{request:?}");
    drop(stream);

    let (output, text) = finish(client);
    assert!(!output.status.success());
    assert!(text.contains("DAEMON_UNREACHABLE"), "{text}");
    assert_eq!(accepted_connections(&listener), 0, "the command sent a request a daemon had already read");
}
