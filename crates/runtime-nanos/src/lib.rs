use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use bctl_runtime::{InputSupport, Ready, Stage, cdp_socket_browser};

pub mod egress;
pub mod profile;
#[cfg(not(target_os = "macos"))]
pub mod supervisor;
#[cfg(target_os = "macos")]
#[path = "supervisor_macos.rs"]
pub mod supervisor;
mod process_group;
mod supervisor_lock;
mod startup;

/// The guest vsock port `bctl-guest run-headless` relays to the browser's CDP endpoint.
pub const GUEST_CDP_PORT: u32 = 9222;
/// The guest vsock port that answers with the headless readiness report.
pub const GUEST_CHECK_PORT: u32 = 9223;
/// The guest vsock port that asks a persistent guest to stop the browser and flush its profile volume.
pub const GUEST_STOP_PORT: u32 = 9224;
/// The host vsock port the guest relays the browser's SOCKS5 proxy connections to.
pub const GUEST_EGRESS_PORT: u32 = 1080;

const CONTROL_SOCKET: &str = "control.sock";
const VSOCK_SOCKET: &str = "vsock.sock";
const CONSOLE_LOG: &str = "console.log";
const SUPERVISOR_LOG: &str = "supervisor.log";
const UNTERMINATED_EXIT: i32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    Spawn { message: String },
    Supervisor { operation: &'static str, message: String },
    NotReady { stage: Stage, detail: String },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::Spawn { message } => write!(f, "cannot run the Nanos supervisor: {message}"),
            RuntimeError::Supervisor { operation, message } => write!(f, "Nanos {operation} failed: {message}"),
            RuntimeError::NotReady { stage, detail } => write!(f, "not ready at the {} stage: {detail}", stage.as_str()),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// The kernel an image must boot with: the image build writes it beside the image, from the same build as the
/// klibs the image carries.
pub fn image_kernel(image: &Path) -> PathBuf {
    image.with_extension("kernel.img")
}

/// The host side of the Nanos runtime.
#[derive(Debug, Clone)]
pub struct Toolchain {
    pub supervisor: PathBuf,
    /// Arguments the supervisor executable needs before `launch` (`["nanos-vm"]` when it is `bctl`).
    pub supervisor_args: Vec<String>,
    pub backend: Backend,
}

#[derive(Debug, Clone)]
pub enum Backend {
    #[cfg(not(target_os = "macos"))]
    Qemu { qemu: PathBuf, qemu_img: PathBuf, vsock_backend: PathBuf, virtiofsd: PathBuf },
    #[cfg(target_os = "macos")]
    Libkrun { runner: PathBuf, firmware: PathBuf },
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub machine: String,
    pub image: PathBuf,
    pub profile_dir: PathBuf,
    /// Host directory the guest mounts at [`bctl_runtime::GUEST_FILES_DIR`]; it must already exist.
    pub files_dir: PathBuf,
    /// Host path where the CDP socket appears; its directory must already exist and be private.
    pub cdp_socket: PathBuf,
    pub memory_mib: u32,
    pub cpus: u8,
    /// IPv4 ranges in CIDR notation the guest may reach although every non-public address is blocked.
    pub allow_private: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadlessReport {
    pub browser: bool,
    pub sandbox: bool,
    pub profile: Option<bool>,
}

impl HeadlessReport {
    /// Parses what the guest's check port answers (see docs/image-contract.md, nanos-helium).
    pub fn parse(output: &str) -> Result<HeadlessReport, String> {
        let (mut browser, mut sandbox, mut profile, mut input) = (None, None, None, false);
        for line in output.lines().filter(|l| !l.trim().is_empty()) {
            let (name, verdict) = line.split_once(' ').ok_or_else(|| format!("malformed line {line:?}"))?;
            let ok = match (name, verdict) {
                ("input", "absent") => {
                    input = true;
                    continue;
                }
                (_, "ok") => true,
                (_, "missing") => false,
                _ => return Err(format!("malformed line {line:?}")),
            };
            match name {
                "browser" => browser = Some(ok),
                "sandbox" => sandbox = Some(ok),
                "profile" => profile = Some(ok),
                _ => return Err(format!("unknown stage in {line:?}")),
            }
        }
        if !input {
            return Err("no input line".into());
        }
        Ok(HeadlessReport { browser: browser.ok_or("no browser line")?, sandbox: sandbox.ok_or("no sandbox line")?, profile })
    }

    pub fn first_missing(&self) -> Option<Stage> {
        if self.profile != Some(true) {
            return Some(Stage::Guest);
        }
        if !self.browser {
            Some(Stage::Browser)
        } else if !self.sandbox {
            Some(Stage::Sandbox)
        } else {
            None
        }
    }
}

/// Connects to a guest vsock port through the host backend.
pub fn connect_guest(vsock: &Path, port: u32, timeout: Duration) -> io::Result<UnixStream> {
    #[cfg(target_os = "macos")]
    {
        let _ = timeout;
        UnixStream::connect(vsock.with_file_name(format!("{VSOCK_SOCKET}_{port}")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let stream = UnixStream::connect(vsock)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        (&stream).write_all(format!("CONNECT {port}\n").as_bytes())?;
        let mut reply = Vec::new();
        let mut byte = [0u8; 1];
        while reply.last() != Some(&b'\n') {
            if (&stream).read(&mut byte)? == 0 {
                return Err(io::Error::new(io::ErrorKind::ConnectionRefused, format!("guest port {port} is not listening")));
            }
            reply.push(byte[0]);
            if reply.len() > 64 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "overlong vsock handshake reply"));
            }
        }
        if !reply.starts_with(b"OK ") {
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, format!("guest port {port}: {}", String::from_utf8_lossy(&reply).trim())));
        }
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        Ok(stream)
    }
}

pub struct Nanos {
    root: PathBuf,
    tools: Toolchain,
}

impl Nanos {
    /// `root` holds one directory per machine.
    pub fn new(root: impl Into<PathBuf>, tools: Toolchain) -> Nanos {
        Nanos { root: root.into(), tools }
    }

    pub fn machine_dir(&self, machine: &str) -> PathBuf {
        self.root.join(machine)
    }

    fn socket_dir(&self, machine: &str) -> PathBuf {
        self.root.parent().unwrap_or(&self.root).join("ns").join(bctl_runtime::machine_socket_key(machine))
    }

    fn control(&self, machine: &str, command: &str) -> io::Result<String> {
        let mut stream = UnixStream::connect(self.socket_dir(machine).join(CONTROL_SOCKET))?;
        stream.set_read_timeout(Some(Duration::from_secs(45)))?;
        stream.write_all(format!("{command}\n").as_bytes())?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        Ok(line.trim().to_string())
    }

    pub fn is_running(&self, machine: &str) -> bool {
        self.control(machine, "status").is_ok_and(|s| s == "running")
    }

    /// Names of machines whose supervisor answers as running.
    pub fn list(&self) -> Result<Vec<String>, RuntimeError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(RuntimeError::Supervisor { operation: "list", message: format!("{}: {error}", self.root.display()) }),
        };
        let mut running: Vec<String> = entries
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|name| self.is_running(name))
            .collect();
        running.sort();
        Ok(running)
    }

    /// Creates the machine directory and starts the supervisor, which returns once QEMU is running.
    pub fn launch(&self, spec: &LaunchSpec) -> Result<(), RuntimeError> {
        let dir = self.machine_dir(&spec.machine);
        if dir.exists() {
            return Err(RuntimeError::Supervisor { operation: "launch", message: format!("{} already exists", dir.display()) });
        }
        fs::create_dir_all(&dir).map_err(|e| RuntimeError::Spawn { message: format!("{}: {e}", dir.display()) })?;
        let socket_dir = {
            let path = self.socket_dir(&spec.machine);
            let created = fs::create_dir_all(path.parent().expect("socket directory has a parent"))
                .and_then(|()| fs::DirBuilder::new().mode(0o700).create(&path));
            if let Err(error) = created {
                let _ = fs::remove_dir_all(&dir);
                return Err(RuntimeError::Spawn { message: format!("{}: {error}", path.display()) });
            }
            path
        };
        let mut command = Command::new(&self.tools.supervisor);
        command.args(&self.tools.supervisor_args)
            .arg("launch")
            .arg("--dir")
            .arg(&dir)
            .arg("--image")
            .arg(&spec.image)
            .arg("--cdp-socket")
            .arg(&spec.cdp_socket)
            .arg("--memory-mib")
            .arg(spec.memory_mib.to_string())
            .arg("--cpus")
            .arg(spec.cpus.to_string());
        command.arg("--socket-dir").arg(&socket_dir);
        command.arg("--profile-dir").arg(&spec.profile_dir);
        command.arg("--files-dir").arg(&spec.files_dir);
        match &self.tools.backend {
            #[cfg(not(target_os = "macos"))]
            Backend::Qemu { qemu, qemu_img, vsock_backend, virtiofsd } => {
                command.arg("--qemu").arg(qemu).arg("--qemu-img").arg(qemu_img)
                    .arg("--kernel").arg(image_kernel(&spec.image)).arg("--vsock-backend").arg(vsock_backend)
                    .arg("--virtiofsd").arg(virtiofsd);
            }
            #[cfg(target_os = "macos")]
            Backend::Libkrun { runner, firmware } => {
                command.arg("--runner").arg(runner).arg("--firmware").arg(firmware);
            }
        }
        let output = command.args(spec.allow_private.iter().flat_map(|range| ["--allow-private", range.as_str()])).output();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                let _ = fs::remove_dir_all(&dir);
                let _ = fs::remove_dir_all(&socket_dir);
                return Err(RuntimeError::Spawn { message: format!("{}: {error}", self.tools.supervisor.display()) });
            }
        };
        if output.status.success() {
            return Ok(());
        }
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if output.status.code() == Some(UNTERMINATED_EXIT) {
            return Err(RuntimeError::Supervisor { operation: "launch", message: format!("{message}; {} and {} are kept because processes remain", dir.display(), socket_dir.display()) });
        }
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&socket_dir);
        Err(RuntimeError::Supervisor { operation: "launch", message })
    }

    /// Stops the VM and deletes its directory; a machine that is already gone is not an error.
    pub fn remove(&self, machine: &str) -> Result<(), RuntimeError> {
        let dir = self.machine_dir(machine);
        if !dir.exists() {
            return Ok(());
        }
        let stopped = match self.control(machine, "stop") {
            Ok(reply) => Some(reply),
            Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => None,
            Err(error) => return Err(RuntimeError::Supervisor { operation: "remove", message: error.to_string() }),
        };
        if let Some(reply) = stopped {
            if reply != "stopped" && reply != "stopping" {
                return Err(RuntimeError::Supervisor { operation: "remove", message: reply });
            }
            let deadline = Instant::now() + Duration::from_secs(45);
            while self.socket_dir(machine).join(CONTROL_SOCKET).exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            if self.is_running(machine) {
                return Err(RuntimeError::Supervisor { operation: "remove", message: format!("{machine} did not stop within 45 s") });
            }
        }
        supervisor_lock::stop_holder(&dir).map_err(|message| RuntimeError::Supervisor { operation: "remove", message })?;
        let sockets = self.socket_dir(machine);
        if sockets.exists() {
            fs::remove_dir_all(&sockets).map_err(|e| RuntimeError::Supervisor { operation: "remove", message: format!("{}: {e}", sockets.display()) })?;
        }
        fs::remove_dir_all(&dir).map_err(|e| RuntimeError::Supervisor { operation: "remove", message: format!("{}: {e}", dir.display()) })
    }

    /// The last `lines` lines of the guest console, then of the supervisor's own log.
    pub fn logs(&self, machine: &str, lines: usize) -> Result<String, RuntimeError> {
        let dir = self.machine_dir(machine);
        let mut out = String::new();
        for name in [CONSOLE_LOG, SUPERVISOR_LOG] {
            let text = fs::read_to_string(dir.join(name)).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(lines).collect();
            for line in tail.into_iter().rev() {
                out.push_str(line);
                out.push('\n');
            }
        }
        Ok(out)
    }

    pub fn check(&self, machine: &str) -> Result<HeadlessReport, RuntimeError> {
        let vsock = self.socket_dir(machine).join(VSOCK_SOCKET);
        let not_ready = |detail: String| RuntimeError::NotReady { stage: Stage::Guest, detail };
        let mut stream = connect_guest(&vsock, GUEST_CHECK_PORT, Duration::from_secs(5)).map_err(|e| not_ready(e.to_string()))?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| not_ready(e.to_string()))?;
        let mut report = String::new();
        stream.read_to_string(&mut report).map_err(|e| not_ready(e.to_string()))?;
        HeadlessReport::parse(&report).map_err(not_ready)
    }

    /// Waits until the machine runs, the guest answers its check port with the browser and its sandbox up, and
    /// `/json/version` answers through the host CDP socket.
    pub fn wait_ready(&self, machine: &str, cdp_socket: &Path, timeout: Duration, mut on_stage: impl FnMut(Stage)) -> Result<Ready, RuntimeError> {
        let startup = startup::Startup::new("readiness");
        startup.mark("begin");
        let deadline = Instant::now() + timeout;
        loop {
            startup.mark("probe_begin");
            match self.probe(machine, cdp_socket) {
                Ok(ready) => { startup.mark("ready"); return Ok(ready); }
                Err(error @ RuntimeError::NotReady { .. }) if Instant::now() >= deadline => return Err(error),
                Err(RuntimeError::NotReady { stage, .. }) => { startup.mark(stage.as_str()); on_stage(stage); }
                Err(error) => return Err(error),
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn probe(&self, machine: &str, cdp_socket: &Path) -> Result<Ready, RuntimeError> {
        let not_ready = |stage, detail: String| RuntimeError::NotReady { stage, detail };
        if !self.is_running(machine) {
            if !self.machine_dir(machine).exists() {
                return Err(RuntimeError::Supervisor { operation: "probe", message: format!("{machine} does not exist") });
            }
            return Err(not_ready(Stage::Machine, "the supervisor does not answer as running".into()));
        }
        let report = self.check(machine)?;
        if let Some(stage) = report.first_missing() {
            return Err(not_ready(stage, "the guest check reports it missing".into()));
        }
        let browser = cdp_socket_browser(cdp_socket).ok_or_else(|| not_ready(Stage::Cdp, "no /json/version through the CDP socket".into()))?;
        Ok(Ready { input: InputSupport::Headless, browser })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_headless_report_names_the_first_missing_stage() {
        let report = HeadlessReport::parse("browser ok\nsandbox missing\ninput absent\nprofile ok\n").unwrap();
        assert_eq!(report.first_missing(), Some(Stage::Sandbox));
        let ready = HeadlessReport::parse("browser ok\nsandbox ok\ninput absent\nprofile ok\n").unwrap();
        assert_eq!(ready.first_missing(), None);
    }

    #[test]
    fn incomplete_or_unknown_headless_reports_are_rejected() {
        assert!(HeadlessReport::parse("browser ok\nsandbox ok\n").is_err());
        assert!(HeadlessReport::parse("browser ok\ninput absent\n").is_err());
        assert!(HeadlessReport::parse("browser ok\nsandbox ok\nseat ok\ninput absent\n").is_err());
        assert!(HeadlessReport::parse("browser maybe\nsandbox ok\ninput absent\n").is_err());
        assert!(HeadlessReport::parse("browser ok\nsandbox ok\ninput ok\n").is_err());
    }

    #[test]
    fn an_older_image_without_a_profile_report_is_not_ready() {
        let report = HeadlessReport::parse("browser ok\nsandbox ok\ninput absent\n").unwrap();
        assert_eq!(report.first_missing(), Some(Stage::Guest));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn libkrun_connects_to_the_requested_port_without_a_handshake() {
        use std::os::unix::net::UnixListener;

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("nanos-port-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{VSOCK_SOCKET}_{GUEST_CHECK_PORT}"));
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"browser ok\n").unwrap();
        });
        let mut client = connect_guest(&dir.join(VSOCK_SOCKET), GUEST_CHECK_PORT, Duration::from_secs(1)).unwrap();
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "browser ok\n");
        server.join().unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn a_long_machine_name_can_reach_its_control_socket() {
        use std::os::unix::net::UnixListener;

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let dir = root.join(".work/tmp").join(format!("nanos-long-{}", std::process::id()));
        let runtime = Nanos::new(dir.join("machines"), Toolchain {
            supervisor: PathBuf::new(), supervisor_args: Vec::new(),
            #[cfg(target_os = "macos")]
            backend: Backend::Libkrun { runner: PathBuf::new(), firmware: PathBuf::new() },
            #[cfg(not(target_os = "macos"))]
            backend: Backend::Qemu { qemu: PathBuf::new(), qemu_img: PathBuf::new(), vsock_backend: PathBuf::new(), virtiofsd: PathBuf::new() },
        });
        let machine = "long-machine-name-".repeat(3);
        let socket_dir = runtime.socket_dir(&machine);
        fs::create_dir_all(&socket_dir).unwrap();
        let listener = UnixListener::bind(socket_dir.join(CONTROL_SOCKET)).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            assert_eq!(line, "status\n");
            stream.write_all(b"running\n").unwrap();
        });
        assert!(runtime.is_running(&machine));
        server.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remove_stops_a_supervisor_whose_control_socket_is_gone() {
        use std::fs::{File, OpenOptions};
        use std::os::unix::process::CommandExt;

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let dir = root.join(".work/tmp").join(format!("nanos-orphan-{}", std::process::id()));
        let runtime = Nanos::new(dir.join("machines"), Toolchain {
            supervisor: PathBuf::new(), supervisor_args: Vec::new(),
            #[cfg(target_os = "macos")]
            backend: Backend::Libkrun { runner: PathBuf::new(), firmware: PathBuf::new() },
            #[cfg(not(target_os = "macos"))]
            backend: Backend::Qemu { qemu: PathBuf::new(), qemu_img: PathBuf::new(), vsock_backend: PathBuf::new(), virtiofsd: PathBuf::new() },
        });
        let machine = "orphan";
        let machine_dir = runtime.machine_dir(machine);
        fs::create_dir_all(&machine_dir).unwrap();
        let mut supervisor = Command::new("/bin/sh").arg("-c").arg("sleep 60 & wait").process_group(0).spawn().unwrap();
        let group = supervisor.id();
        let mut lock: File = OpenOptions::new().create(true).truncate(true).write(true).open(machine_dir.join("supervisor.lock")).unwrap();
        lock.try_lock().unwrap();
        writeln!(lock, "{group}").unwrap();
        let released = thread::spawn(move || {
            supervisor.wait().unwrap();
            drop(lock);
        });

        let removed = runtime.remove(machine);
        let alive = || Command::new("pgrep").arg("-g").arg(group.to_string()).stdout(std::process::Stdio::null()).status().unwrap().success();
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        let survived = alive();
        if survived {
            let _ = Command::new("/bin/kill").args(["-s", "KILL"]).arg(format!("-{group}")).status();
        }
        released.join().unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(!survived, "the supervisor's process group {group} survived remove");
        removed.unwrap();
        assert!(!machine_dir.exists());
    }
}
