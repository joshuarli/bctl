use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use bctl_runtime::{InputSupport, Ready, Stage, cdp_socket_browser};

pub mod egress;
pub mod supervisor;

/// The guest vsock port `bctl-guest run-headless` relays to the browser's CDP endpoint.
pub const GUEST_CDP_PORT: u32 = 9222;
/// The guest vsock port that answers with the headless readiness report.
pub const GUEST_CHECK_PORT: u32 = 9223;
/// The host vsock port the guest relays the browser's SOCKS5 proxy connections to.
pub const GUEST_EGRESS_PORT: u32 = 1080;

const CONTROL_SOCKET: &str = "control.sock";
const VSOCK_SOCKET: &str = "vsock.sock";
const CONSOLE_LOG: &str = "console.log";
const SUPERVISOR_LOG: &str = "supervisor.log";

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

/// The host side of the Nanos runtime: QEMU, hydrogen's kernel, the vsock backend, and the supervisor binary.
#[derive(Debug, Clone)]
pub struct Toolchain {
    pub supervisor: PathBuf,
    /// Arguments the supervisor executable needs before `launch` (`["nanos-vm"]` when it is `bctl`).
    pub supervisor_args: Vec<String>,
    pub qemu: PathBuf,
    pub qemu_img: PathBuf,
    pub kernel: PathBuf,
    pub vsock_backend: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub machine: String,
    pub image: PathBuf,
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
}

impl HeadlessReport {
    /// Parses what the guest's check port answers (see docs/image-contract.md, nanos-helium).
    pub fn parse(output: &str) -> Result<HeadlessReport, String> {
        let (mut browser, mut sandbox, mut input) = (None, None, false);
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
                _ => return Err(format!("unknown stage in {line:?}")),
            }
        }
        if !input {
            return Err("no input line".into());
        }
        Ok(HeadlessReport { browser: browser.ok_or("no browser line")?, sandbox: sandbox.ok_or("no sandbox line")? })
    }

    pub fn first_missing(&self) -> Option<Stage> {
        if !self.browser {
            Some(Stage::Browser)
        } else if !self.sandbox {
            Some(Stage::Sandbox)
        } else {
            None
        }
    }
}

/// Connects to a guest vsock port through vhost-device-vsock's Unix socket with its `CONNECT` handshake.
pub fn connect_guest(vsock: &Path, port: u32, timeout: Duration) -> io::Result<UnixStream> {
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

    fn control(&self, machine: &str, command: &str) -> io::Result<String> {
        let mut stream = UnixStream::connect(self.machine_dir(machine).join(CONTROL_SOCKET))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
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
        let output = Command::new(&self.tools.supervisor)
            .args(&self.tools.supervisor_args)
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
            .arg(spec.cpus.to_string())
            .arg("--qemu")
            .arg(&self.tools.qemu)
            .arg("--qemu-img")
            .arg(&self.tools.qemu_img)
            .arg("--kernel")
            .arg(&self.tools.kernel)
            .arg("--vsock-backend")
            .arg(&self.tools.vsock_backend)
            .args(spec.allow_private.iter().flat_map(|range| ["--allow-private", range.as_str()]))
            .output()
            .map_err(|e| RuntimeError::Spawn { message: format!("{}: {e}", self.tools.supervisor.display()) })?;
        if output.status.success() {
            return Ok(());
        }
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let _ = fs::remove_dir_all(&dir);
        Err(RuntimeError::Supervisor { operation: "launch", message })
    }

    /// Stops the VM and deletes its directory; a machine that is already gone is not an error.
    pub fn remove(&self, machine: &str) -> Result<(), RuntimeError> {
        let dir = self.machine_dir(machine);
        if !dir.exists() {
            return Ok(());
        }
        if self.control(machine, "stop").is_ok() {
            let deadline = Instant::now() + Duration::from_secs(20);
            while self.machine_dir(machine).join(CONTROL_SOCKET).exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            if self.is_running(machine) {
                return Err(RuntimeError::Supervisor { operation: "remove", message: format!("{machine} did not stop within 20 s") });
            }
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
        let vsock = self.machine_dir(machine).join(VSOCK_SOCKET);
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
        let deadline = Instant::now() + timeout;
        loop {
            match self.probe(machine, cdp_socket) {
                Ok(ready) => return Ok(ready),
                Err(error @ RuntimeError::NotReady { .. }) if Instant::now() >= deadline => return Err(error),
                Err(RuntimeError::NotReady { stage, .. }) => on_stage(stage),
                Err(error) => return Err(error),
            }
            thread::sleep(Duration::from_millis(50));
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
        let report = HeadlessReport::parse("browser ok\nsandbox missing\ninput absent\n").unwrap();
        assert_eq!(report.first_missing(), Some(Stage::Sandbox));
        let ready = HeadlessReport::parse("browser ok\nsandbox ok\ninput absent\n").unwrap();
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
}
