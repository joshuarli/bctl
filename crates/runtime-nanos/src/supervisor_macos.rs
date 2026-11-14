use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::egress::{self, Policy};
use crate::process_group::OwnedGroup;
use crate::profile::prepare_profile;
use crate::supervisor_lock;
use crate::{CONSOLE_LOG, CONTROL_SOCKET, GUEST_CDP_PORT, GUEST_CHECK_PORT, GUEST_EGRESS_PORT, GUEST_STOP_PORT, SUPERVISOR_LOG, UNTERMINATED_EXIT, VSOCK_SOCKET, connect_guest};

pub(crate) const START_TIMEOUT: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(50);
const READY_POLL: Duration = Duration::from_micros(500);

struct Config {
    dir: PathBuf,
    socket_dir: PathBuf,
    image: PathBuf,
    profile_dir: PathBuf,
    files_dir: PathBuf,
    cdp_socket: PathBuf,
    memory_mib: u32,
    cpus: u8,
    runner: PathBuf,
    kernel: PathBuf,
    allow_private: Vec<String>,
    start_timeout: Duration,
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
            socket_dir: path("--socket-dir")?,
            image: path("--image")?,
            profile_dir: path("--profile-dir")?,
            files_dir: path("--files-dir")?,
            cdp_socket: path("--cdp-socket")?,
            memory_mib: value("--memory-mib")?.parse().map_err(|_| "--memory-mib is not a number")?,
            cpus: value("--cpus")?.parse().map_err(|_| "--cpus is not a number")?,
            runner: path("--runner")?,
            kernel: path("--kernel")?,
            allow_private: args.windows(2).filter(|pair| pair[0] == "--allow-private").map(|pair| pair[1].clone()).collect(),
            start_timeout: match args.iter().position(|arg| arg == "--start-timeout-ms") {
                None => START_TIMEOUT,
                Some(at) => Duration::from_millis(
                    args.get(at + 1).and_then(|value| value.parse().ok()).ok_or("--start-timeout-ms needs a number of milliseconds")?,
                ),
            },
        })
    }
}

pub fn main(prefix: &[&str], args: &[String]) -> ExitCode {
    let result = match args.split_first() {
        Some((command, rest)) if command == "launch" => Config::parse(rest).map_err(Failure::from).and_then(|config| launch(prefix, &config, rest)),
        Some((command, rest)) if command == "serve" => Config::parse(rest).and_then(|config| serve(&config)).map_err(Failure::from),
        _ => Err(Failure::from("usage: bctl-nanos-vm launch|serve --dir DIR --socket-dir DIR --image IMAGE --profile-dir DIR --files-dir DIR --cdp-socket PATH --memory-mib N --cpus N --runner PATH --kernel PATH [--allow-private CIDR]... [--start-timeout-ms N]".to_string())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("bctl-nanos-vm: {}", failure.message);
            if failure.unterminated { ExitCode::from(UNTERMINATED_EXIT as u8) } else { ExitCode::FAILURE }
        }
    }
}

pub(crate) struct Failure {
    pub(crate) message: String,
    pub(crate) unterminated: bool,
}

pub(crate) enum ServeError {
    Spawn(io::Error),
    Failed(Failure),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self { message, unterminated: false }
    }
}

fn log_file(dir: &Path, name: &str) -> Result<File, String> {
    let path = dir.join(name);
    OpenOptions::new().create(true).append(true).open(&path).map_err(|error| format!("{}: {error}", path.display()))
}

pub(crate) fn launch_serve(mut command: Command, dir: &Path, start_timeout: Duration) -> Result<(), ServeError> {
    let stderr = log_file(dir, SUPERVISOR_LOG).map_err(|error| ServeError::Failed(Failure::from(error)))?;
    let mut child = command.process_group(0)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(stderr)
        .spawn().map_err(ServeError::Spawn)?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut group = OwnedGroup::new(child);
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
    });
    match receiver.recv_timeout(start_timeout) {
        Ok(line) if line.trim() == "ready" => {
            drop(group.disarm());
            Ok(())
        }
        outcome => {
            let terminated = group.terminate();
            let log = fs::read_to_string(dir.join(SUPERVISOR_LOG)).unwrap_or_default();
            let last = log.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no message");
            let mut message = format!("the supervisor did not start libkrun ({outcome:?}): {last}");
            if let Err(error) = &terminated {
                message = format!("{message}; {error}");
            }
            Err(ServeError::Failed(Failure { message, unterminated: terminated.is_err() }))
        }
    }
}

fn launch(prefix: &[&str], config: &Config, args: &[String]) -> Result<(), Failure> {
    let exe = std::env::current_exe().map_err(|error| format!("current executable: {error}"))?;
    let mut command = Command::new(&exe);
    command.args(prefix).arg("serve").args(args);
    launch_serve(command, &config.dir, config.start_timeout).map_err(|error| match error {
        ServeError::Spawn(error) => Failure::from(format!("{}: {error}", exe.display())),
        ServeError::Failed(failure) => failure,
    })
}

fn socket(dir: &Path, port: u32) -> PathBuf {
    dir.join(format!("{VSOCK_SOCKET}_{port}"))
}

fn bind(path: &Path) -> Result<UnixListener, String> {
    UnixListener::bind(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn wait_for_runner(child: &mut Child, config: &Config) -> Result<(), String> {
    let deadline = Instant::now() + config.start_timeout;
    while !socket(&config.socket_dir, GUEST_CDP_PORT).exists() || !socket(&config.socket_dir, GUEST_CHECK_PORT).exists() {
        if let Some(status) = child.try_wait().map_err(|error| format!("libkrun: {error}"))? {
            return Err(format!("libkrun exited before creating its sockets: {status}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("libkrun did not create its sockets within {:?}", config.start_timeout));
        }
        thread::sleep(READY_POLL);
    }
    Ok(())
}

fn serve(config: &Config) -> Result<(), String> {
    let startup = crate::startup::Startup::new("supervisor");
    startup.mark("begin");
    let _lock = supervisor_lock::hold(&config.dir)?;
    let policy = Policy::new(&config.allow_private)?;
    startup.mark("profile_begin");
    let profile = prepare_profile(&config.profile_dir, &config.image.with_extension("profile.img"), copy_template)?;
    startup.mark("profile_ready");
    let disk = config.dir.join("disk.img");
    fs::copy(&config.image, &disk).map_err(|error| format!("copy {} to {}: {error}", config.image.display(), disk.display()))?;
    startup.mark("root_disk_ready");
    let mut cleanup = SocketCleanup(Vec::new());
    let control_path = config.socket_dir.join(CONTROL_SOCKET);
    let control = bind(&control_path)?;
    cleanup.0.push(control_path);
    let cdp = bind(&config.cdp_socket)?;
    cleanup.0.push(config.cdp_socket.clone());
    let egress_path = socket(&config.socket_dir, GUEST_EGRESS_PORT);
    let egress = bind(&egress_path)?;
    cleanup.0.push(egress_path);
    for port in [GUEST_CDP_PORT, GUEST_CHECK_PORT, GUEST_STOP_PORT] {
        let path = socket(&config.socket_dir, port);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Err(format!("{} already exists", path.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    startup.mark("runner_spawn_begin");
    let mut child = Vm(Command::new(&config.runner).arg(&disk).arg(&config.kernel)
        .arg(socket(&config.socket_dir, GUEST_CDP_PORT)).arg(socket(&config.socket_dir, GUEST_CHECK_PORT))
        .arg(socket(&config.socket_dir, GUEST_EGRESS_PORT)).arg(config.cpus.to_string()).arg(config.memory_mib.to_string())
        .arg(&profile).arg(socket(&config.socket_dir, GUEST_STOP_PORT)).arg(&config.files_dir)
        .stdin(Stdio::null()).stdout(log_file(&config.dir, CONSOLE_LOG)?).stderr(log_file(&config.dir, SUPERVISOR_LOG)?)
        .spawn().map_err(|error| format!("{}: {error}", config.runner.display()))?);
    startup.mark("runner_spawned");
    cleanup.0.push(socket(&config.socket_dir, GUEST_CDP_PORT));
    cleanup.0.push(socket(&config.socket_dir, GUEST_CHECK_PORT));
    cleanup.0.push(socket(&config.socket_dir, GUEST_STOP_PORT));
    wait_for_runner(&mut child.0, config)?;
    startup.mark("runner_sockets_observed");
    let (stop, stop_requests) = mpsc::channel::<mpsc::Sender<(Result<(), String>, mpsc::Sender<()>)>>();
    thread::spawn(move || {
        for client in control.incoming() {
            let mut client = match client {
                Ok(client) => client,
                Err(error) => { eprintln!("bctl-nanos-vm: control accept: {error}"); continue; }
            };
            let mut line = String::new();
            if BufReader::new(&client).read_line(&mut line).is_err() { continue; }
            let reply = match line.trim() {
                "status" => "running",
                "stop" => {
                    let (reply, result) = mpsc::channel();
                    let response = stop.send(reply).map_err(|e| e.to_string()).and_then(|()| result.recv().map_err(|e| e.to_string()));
                    match response {
                        Ok((result, acknowledged)) => {
                            match result {
                                Ok(()) => { let _ = writeln!(client, "stopped"); }
                                Err(error) => { let _ = writeln!(client, "{error}"); }
                            }
                            let _ = acknowledged.send(());
                        }
                        Err(error) => { let _ = writeln!(client, "{error}"); }
                    }
                    continue;
                }
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
    let vsock = config.socket_dir.join(VSOCK_SOCKET);
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
    startup.mark("ready");
    writeln!(io::stdout().lock(), "ready").map_err(|error| format!("stdout: {error}"))?;
    loop {
        if let Some(status) = child.0.try_wait().map_err(|error| format!("libkrun: {error}"))? {
            eprintln!("bctl-nanos-vm: libkrun exited: {status}");
            break;
        }
        if let Ok(reply) = stop_requests.try_recv() {
            let result = stop_vm(&mut child.0, &config.socket_dir);
            let (acknowledged, ack) = mpsc::channel();
            if reply.send((result.clone(), acknowledged)).is_ok() {
                let _ = ack.recv_timeout(Duration::from_secs(2));
            }
            result?;
            break;
        }
        thread::sleep(TICK);
    }
    Ok(())
}

fn copy_template(template: &Path, scratch: &Path) -> io::Result<()> {
    fs::copy(template, scratch)?;
    fs::set_permissions(scratch, fs::Permissions::from_mode(0o600))
}

fn stop_vm(child: &mut Child, socket_dir: &Path) -> Result<(), String> {
    let result = (|| -> Result<(), String> {
        let stream = UnixStream::connect(socket(socket_dir, GUEST_STOP_PORT)).map_err(|e| format!("request guest shutdown: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(18))).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).map_err(|e| format!("guest shutdown: {e}"))?;
        if reply.trim() != "ok" { return Err(format!("guest shutdown failed: {}", reply.trim())); }
        Ok(())
    })();
    if result.is_ok() {
        let deadline = Instant::now() + Duration::from_secs(18);
        while Instant::now() < deadline {
            if child.try_wait().map_err(|e| format!("wait for libkrun: {e}"))?.is_some() {
                return Ok(());
            }
            thread::sleep(TICK);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    result.and(Err("guest did not stop within 18 s".into()))
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
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn failed_launch_preserves_an_existing_cdp_socket() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("nanos-conflict-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("source.img");
        fs::write(&image, b"image").unwrap();
        fs::write(image.with_extension("profile.img"), b"profile").unwrap();
        let cdp_socket = dir.join("cdp.sock");
        let listener = UnixListener::bind(&cdp_socket).unwrap();
        let config = Config {
            dir: dir.clone(), socket_dir: dir.clone(), image, profile_dir: dir.join("profile"), files_dir: dir.join("files"), cdp_socket: cdp_socket.clone(), memory_mib: 1024, cpus: 1,
            runner: dir.join("runner"), kernel: dir.join("kernel"), allow_private: Vec::new(),
            start_timeout: START_TIMEOUT,
        };
        assert!(serve(&config).is_err());
        assert!(cdp_socket.exists());
        assert!(!dir.join(CONTROL_SOCKET).exists());
        drop(listener);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_profile_cloned_from_a_sparse_template_stays_sparse() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("nanos-sparse-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let template = dir.join("template");
        File::create(&template).unwrap().set_len(256 << 20).unwrap();
        let disk = prepare_profile(&dir.join("profile"), &template, copy_template).unwrap();
        let meta = fs::metadata(&disk).unwrap();
        assert_eq!(meta.len(), 256 << 20);
        assert!(meta.blocks() * 512 < 1 << 20, "{} bytes allocated", meta.blocks() * 512);
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        fs::remove_dir_all(dir).unwrap();
    }
}
