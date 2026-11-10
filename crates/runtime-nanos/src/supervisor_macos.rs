use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::egress::{self, Policy};
use crate::{CONSOLE_LOG, CONTROL_SOCKET, GUEST_CDP_PORT, GUEST_CHECK_PORT, GUEST_EGRESS_PORT, SUPERVISOR_LOG, VSOCK_SOCKET, connect_guest};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(50);

struct Config {
    dir: PathBuf,
    image: PathBuf,
    cdp_socket: PathBuf,
    memory_mib: u32,
    cpus: u8,
    runner: PathBuf,
    firmware: PathBuf,
    allow_private: Vec<String>,
}

struct SocketCleanup(Vec<PathBuf>);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

struct Vm(Child);

impl Drop for Vm {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Config {
    fn parse(args: &[String]) -> Result<Self, String> {
        let value = |name: &str| -> Result<String, String> {
            let at = args.iter().position(|arg| arg == name).ok_or_else(|| format!("missing {name}"))?;
            args.get(at + 1).cloned().ok_or_else(|| format!("{name} needs a value"))
        };
        let path = |name: &str| value(name).map(PathBuf::from);
        Ok(Self {
            dir: path("--dir")?,
            image: path("--image")?,
            cdp_socket: path("--cdp-socket")?,
            memory_mib: value("--memory-mib")?.parse().map_err(|_| "--memory-mib is not a number")?,
            cpus: value("--cpus")?.parse().map_err(|_| "--cpus is not a number")?,
            runner: path("--runner")?,
            firmware: path("--firmware")?,
            allow_private: args.windows(2).filter(|pair| pair[0] == "--allow-private").map(|pair| pair[1].clone()).collect(),
        })
    }
}

pub fn main(prefix: &[&str], args: &[String]) -> ExitCode {
    let result = match args.split_first() {
        Some((command, rest)) if command == "launch" => Config::parse(rest).and_then(|config| launch(prefix, &config, rest)),
        Some((command, rest)) if command == "serve" => Config::parse(rest).and_then(|config| serve(&config)),
        _ => Err("usage: bctl-nanos-vm launch|serve --dir DIR --image IMAGE --cdp-socket PATH --memory-mib N --cpus N --runner PATH --firmware PATH [--allow-private CIDR]...".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("bctl-nanos-vm: {message}");
            ExitCode::FAILURE
        }
    }
}

fn log_file(dir: &Path, name: &str) -> Result<File, String> {
    let path = dir.join(name);
    OpenOptions::new().create(true).append(true).open(&path).map_err(|error| format!("{}: {error}", path.display()))
}

fn launch(prefix: &[&str], config: &Config, args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| format!("current executable: {error}"))?;
    let mut child = Command::new(&exe).args(prefix).arg("serve").args(args).process_group(0)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(log_file(&config.dir, SUPERVISOR_LOG)?)
        .spawn().map_err(|error| format!("{}: {error}", exe.display()))?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
    });
    match receiver.recv_timeout(START_TIMEOUT) {
        Ok(line) if line.trim() == "ready" => Ok(()),
        outcome => {
            let _ = Command::new("/bin/kill").arg("-TERM").arg("--").arg(format!("-{}", child.id())).status();
            let _ = child.kill();
            let _ = child.wait();
            let log = fs::read_to_string(config.dir.join(SUPERVISOR_LOG)).unwrap_or_default();
            let last = log.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no message");
            Err(format!("the supervisor did not start libkrun ({outcome:?}): {last}"))
        }
    }
}

fn socket(dir: &Path, port: u32) -> PathBuf {
    dir.join(format!("{VSOCK_SOCKET}_{port}"))
}

fn bind(path: &Path) -> Result<UnixListener, String> {
    UnixListener::bind(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn wait_for_runner(child: &mut Child, config: &Config) -> Result<(), String> {
    let deadline = Instant::now() + START_TIMEOUT;
    while !socket(&config.dir, GUEST_CDP_PORT).exists() || !socket(&config.dir, GUEST_CHECK_PORT).exists() {
        if let Some(status) = child.try_wait().map_err(|error| format!("libkrun: {error}"))? {
            return Err(format!("libkrun exited before creating its sockets: {status}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("libkrun did not create its sockets within {START_TIMEOUT:?}"));
        }
        thread::sleep(TICK);
    }
    Ok(())
}

fn serve(config: &Config) -> Result<(), String> {
    let policy = Policy::new(&config.allow_private)?;
    let disk = config.dir.join("disk.img");
    fs::copy(&config.image, &disk).map_err(|error| format!("copy {} to {}: {error}", config.image.display(), disk.display()))?;
    let mut cleanup = SocketCleanup(Vec::new());
    let control_path = config.dir.join(CONTROL_SOCKET);
    let control = bind(&control_path)?;
    cleanup.0.push(control_path);
    let cdp = bind(&config.cdp_socket)?;
    cleanup.0.push(config.cdp_socket.clone());
    let egress_path = socket(&config.dir, GUEST_EGRESS_PORT);
    let egress = bind(&egress_path)?;
    cleanup.0.push(egress_path);
    let mut child = Vm(Command::new(&config.runner).arg(&disk).arg(&config.firmware)
        .arg(socket(&config.dir, GUEST_CDP_PORT)).arg(socket(&config.dir, GUEST_CHECK_PORT))
        .arg(socket(&config.dir, GUEST_EGRESS_PORT)).arg(config.cpus.to_string()).arg(config.memory_mib.to_string())
        .stdin(Stdio::null()).stdout(log_file(&config.dir, CONSOLE_LOG)?).stderr(log_file(&config.dir, SUPERVISOR_LOG)?)
        .spawn().map_err(|error| format!("{}: {error}", config.runner.display()))?);
    cleanup.0.push(socket(&config.dir, GUEST_CDP_PORT));
    cleanup.0.push(socket(&config.dir, GUEST_CHECK_PORT));
    wait_for_runner(&mut child.0, config)?;
    let stop = Arc::new(AtomicBool::new(false));
    let control_stop = Arc::clone(&stop);
    thread::spawn(move || {
        for client in control.incoming() {
            let mut client = match client {
                Ok(client) => client,
                Err(error) => { eprintln!("bctl-nanos-vm: control accept: {error}"); continue; }
            };
            let mut line = String::new();
            if BufReader::new(&client).read_line(&mut line).is_err() { continue; }
            let reply = match line.trim() {
                "status" if control_stop.load(Ordering::SeqCst) => "stopping",
                "status" => "running",
                "stop" => { control_stop.store(true, Ordering::SeqCst); "stopping" }
                _ => "unknown command",
            };
            let _ = writeln!(client, "{reply}");
        }
    });
    thread::spawn(move || {
        for client in egress.incoming() {
            let client = match client {
                Ok(client) => client,
                Err(error) => { eprintln!("bctl-nanos-vm: egress accept: {error}"); continue; }
            };
            let policy = policy.clone();
            thread::spawn(move || egress::serve(client, &policy));
        }
    });
    let vsock = config.dir.join(VSOCK_SOCKET);
    thread::spawn(move || {
        for client in cdp.incoming() {
            let client = match client {
                Ok(client) => client,
                Err(error) => { eprintln!("bctl-nanos-vm: CDP accept: {error}"); continue; }
            };
            let vsock = vsock.clone();
            thread::spawn(move || relay(client, &vsock));
        }
    });
    writeln!(io::stdout().lock(), "ready").map_err(|error| format!("stdout: {error}"))?;
    loop {
        if let Some(status) = child.0.try_wait().map_err(|error| format!("libkrun: {error}"))? {
            eprintln!("bctl-nanos-vm: libkrun exited: {status}");
            break;
        }
        if stop.load(Ordering::SeqCst) {
            child.0.kill().map_err(|error| format!("stop libkrun: {error}"))?;
            child.0.wait().map_err(|error| format!("wait for libkrun: {error}"))?;
            break;
        }
        thread::sleep(TICK);
    }
    Ok(())
}

fn relay(client: UnixStream, vsock: &Path) {
    let upstream = match connect_guest(vsock, GUEST_CDP_PORT, Duration::from_secs(5)) {
        Ok(stream) => stream,
        Err(error) => { eprintln!("bctl-nanos-vm: connect to guest CDP: {error}"); return; }
    };
    let (Ok(mut reader), Ok(mut writer)) = (client.try_clone(), upstream.try_clone()) else {
        eprintln!("bctl-nanos-vm: duplicate a relay stream");
        return;
    };
    let forward = thread::spawn(move || {
        let _ = io::copy(&mut reader, &mut writer);
        let _ = writer.shutdown(Shutdown::Write);
    });
    let (mut reader, mut writer) = (upstream, client);
    let _ = io::copy(&mut reader, &mut writer);
    let _ = writer.shutdown(Shutdown::Write);
    let _ = forward.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_launch_preserves_an_existing_cdp_socket() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("nanos-conflict-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("source.img");
        fs::write(&image, b"image").unwrap();
        let cdp_socket = dir.join("cdp.sock");
        let listener = UnixListener::bind(&cdp_socket).unwrap();
        let config = Config {
            dir: dir.clone(), image, cdp_socket: cdp_socket.clone(), memory_mib: 1024, cpus: 1,
            runner: dir.join("runner"), firmware: dir.join("firmware"), allow_private: Vec::new(),
        };
        assert!(serve(&config).is_err());
        assert!(cdp_socket.exists());
        assert!(!dir.join(CONTROL_SOCKET).exists());
        drop(listener);
        fs::remove_dir_all(dir).unwrap();
    }
}
