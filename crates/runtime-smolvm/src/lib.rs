use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use bctl_core::{Endpoints, MachineName, MachineObservation};
use bctl_rfb::Client;
use miniserde::{Deserialize, json};

const GUEST_CDP_SOCKET: &str = "/run/bctl/cdp.sock";

/// Why a smolvm operation failed. `Smolvm` carries smolvm's own stable error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    Spawn { message: String },
    Smolvm { operation: &'static str, code: String, message: String },
    UnexpectedOutput { operation: &'static str, detail: String },
    NotReady { stage: Stage, detail: String },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::Spawn { message } => write!(f, "cannot run smolvm: {message}"),
            RuntimeError::Smolvm { operation, code, message } => write!(f, "smolvm {operation} failed ({code}): {message}"),
            RuntimeError::UnexpectedOutput { operation, detail } => write!(f, "smolvm {operation} printed something unexpected: {detail}"),
            RuntimeError::NotReady { stage, detail } => write!(f, "not ready at the {} stage: {detail}", stage.as_str()),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Readiness stages in the order an appliance reaches them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Machine,
    Guest,
    Seat,
    Compositor,
    Browser,
    Sandbox,
    Cdp,
    Display,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Machine => "machine",
            Stage::Guest => "guest",
            Stage::Seat => "seat",
            Stage::Compositor => "compositor",
            Stage::Browser => "browser",
            Stage::Sandbox => "sandbox",
            Stage::Cdp => "cdp",
            Stage::Display => "display",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSupport {
    Interactive,
    ViewOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub input: InputSupport,
    pub browser: String,
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub machine: MachineName,
    pub image: PathBuf,
    /// Host directory mounted at `/profile`, so the browser profile outlives the VM.
    pub profile_dir: Option<PathBuf>,
    pub endpoints: Endpoints,
    /// Host path where the guest's CDP socket appears; its directory must already exist and be private.
    pub cdp_socket: PathBuf,
    pub display: (u16, u16),
    pub memory_mib: u32,
    pub cpus: u8,
    /// IPv4 ranges in CIDR notation the guest may reach although private egress is blocked.
    pub allow_private: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestReport {
    pub stages: Vec<(Stage, bool)>,
    pub input: InputSupport,
}

impl GuestReport {
    /// Parses `bctl-guest check` output (see docs/image-contract.md).
    pub fn parse(output: &str) -> Result<GuestReport, String> {
        let mut stages = Vec::new();
        let mut input = None;
        for line in output.lines().filter(|l| !l.trim().is_empty()) {
            let (name, verdict) = line.split_once(' ').ok_or_else(|| format!("malformed line {line:?}"))?;
            let stage = match name {
                "seat" => Stage::Seat,
                "compositor" => Stage::Compositor,
                "browser" => Stage::Browser,
                "sandbox" => Stage::Sandbox,
                "input" => {
                    input = Some(match verdict {
                        "ok" => InputSupport::Interactive,
                        "absent" => InputSupport::ViewOnly,
                        _ => return Err(format!("malformed line {line:?}")),
                    });
                    continue;
                }
                _ => return Err(format!("unknown stage in {line:?}")),
            };
            let ok = match verdict {
                "ok" => true,
                "missing" => false,
                _ => return Err(format!("malformed line {line:?}")),
            };
            stages.push((stage, ok));
        }
        for required in [Stage::Seat, Stage::Compositor, Stage::Browser, Stage::Sandbox] {
            if !stages.iter().any(|(s, _)| *s == required) {
                return Err(format!("no {} line", required.as_str()));
            }
        }
        Ok(GuestReport { stages, input: input.ok_or("no input line")? })
    }

    pub fn first_missing(&self) -> Option<Stage> {
        self.stages.iter().filter(|(_, ok)| !ok).map(|(s, _)| *s).min()
    }
}

#[derive(Deserialize)]
struct ListedMachine {
    name: String,
    state: String,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

pub struct SmolVm {
    binary: PathBuf,
}

impl SmolVm {
    pub fn new(binary: impl Into<PathBuf>) -> SmolVm {
        SmolVm { binary: binary.into() }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    fn run(&self, operation: &'static str, args: &[&str], env: &[(&str, String)]) -> Result<String, RuntimeError> {
        let output = Command::new(&self.binary)
            .args(args)
            .arg("--error-format")
            .arg("json")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .output()
            .map_err(|e| RuntimeError::Spawn { message: e.to_string() })?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let envelope = stderr.lines().rev().find_map(|line| json::from_str::<ErrorEnvelope>(line).ok());
        Err(match envelope {
            Some(ErrorEnvelope { error }) => RuntimeError::Smolvm { operation, code: error.code, message: error.message },
            None => RuntimeError::Smolvm { operation, code: "UNKNOWN".into(), message: stderr.trim().to_string() },
        })
    }

    pub fn list(&self) -> Result<Vec<MachineObservation>, RuntimeError> {
        let stdout = self.run("machine ls", &["machine", "ls", "--json"], &[])?;
        let machines: Vec<ListedMachine> = json::from_str(&stdout)
            .map_err(|_| RuntimeError::UnexpectedOutput { operation: "machine ls", detail: stdout.trim().chars().take(200).collect() })?;
        Ok(machines.into_iter().map(|m| MachineObservation { running: m.state == "running", name: m.name }).collect())
    }

    /// Creates and starts the VM. The display and VNC endpoint are bound at start, so they travel with it.
    pub fn launch(&self, spec: &LaunchSpec) -> Result<(), RuntimeError> {
        let name = spec.machine.to_string();
        let image = spec.image.to_string_lossy();
        let socket = format!("{GUEST_CDP_SOCKET}:{}", spec.cdp_socket.to_string_lossy());
        let memory = spec.memory_mib.to_string();
        let cpus = spec.cpus.to_string();
        let mut create = vec!["machine", "create", "--name", &name, "--gpu", "--mem", &memory, "--cpus", &cpus, "--expose-socket", &socket, "--block-private", "--image", &image];
        let volume = spec.profile_dir.as_ref().map(|d| format!("{}:/profile", d.to_string_lossy()));
        if let Some(volume) = &volume {
            create.extend(["-v", volume]);
        }
        for range in &spec.allow_private {
            create.extend(["--allow-private", range]);
        }
        self.run("machine create", &create, &[])?;
        let env = [
            ("SMOLVM_DISPLAY", format!("{}x{}", spec.display.0, spec.display.1)),
            ("SMOLVM_VNC", format!("127.0.0.1:{}", spec.endpoints.vnc_port)),
        ];
        if let Err(error) = self.run("machine start", &["machine", "start", "--name", &name], &env) {
            let _ = self.remove(&spec.machine);
            return Err(error);
        }
        Ok(())
    }

    /// Stops and deletes the VM; a machine that is already gone is not an error.
    pub fn remove(&self, machine: &MachineName) -> Result<(), RuntimeError> {
        let name = machine.to_string();
        for (operation, args) in [
            ("machine stop", vec!["machine", "stop", "--name", &name]),
            ("machine delete", vec!["machine", "delete", "--name", &name, "-f"]),
        ] {
            match self.run(operation, &args, &[]) {
                Ok(_) => {}
                Err(RuntimeError::Smolvm { code, .. }) if code == "NOT_FOUND" => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn guest_check(&self, machine: &MachineName) -> Result<GuestReport, RuntimeError> {
        let name = machine.to_string();
        let output = Command::new(&self.binary)
            .args(["machine", "exec", "--name", &name, "--", "bctl-guest", "check"])
            .output()
            .map_err(|e| RuntimeError::Spawn { message: e.to_string() })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        GuestReport::parse(&stdout).map_err(|detail| RuntimeError::NotReady {
            stage: Stage::Guest,
            detail: format!("{detail}; stderr: {}", String::from_utf8_lossy(&output.stderr).trim()),
        })
    }

    /// Waits until every stage holds: machine, guest agent, seat, compositor, browser, sandbox, CDP through the
    /// exposed socket, and a display that has drawn something. An open port alone is never taken as ready.
    pub fn wait_ready(
        &self,
        machine: &MachineName,
        endpoints: Endpoints,
        cdp_socket: &Path,
        timeout: Duration,
        mut on_stage: impl FnMut(Stage),
    ) -> Result<Ready, RuntimeError> {
        let deadline = Instant::now() + timeout;
        let name = machine.to_string();
        loop {
            match self.probe(&name, machine, endpoints, cdp_socket) {
                Ok(ready) => return Ok(ready),
                Err(error @ RuntimeError::NotReady { .. }) if Instant::now() >= deadline => return Err(error),
                Err(RuntimeError::NotReady { stage, .. }) => on_stage(stage),
                Err(error) => return Err(error),
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn probe(&self, name: &str, machine: &MachineName, endpoints: Endpoints, cdp_socket: &Path) -> Result<Ready, RuntimeError> {
        let not_ready = |stage, detail: String| RuntimeError::NotReady { stage, detail };
        let running = self.list()?.into_iter().any(|m| m.name == name && m.running);
        if !running {
            return Err(not_ready(Stage::Machine, "machine is not running".into()));
        }
        let report = self.guest_check(machine)?;
        if let Some(stage) = report.first_missing() {
            return Err(not_ready(stage, "bctl-guest check reports it missing".into()));
        }
        let browser = cdp_socket_browser(cdp_socket).ok_or_else(|| not_ready(Stage::Cdp, "no /json/version through the CDP socket".into()))?;
        let mut vnc = Client::connect(("127.0.0.1", endpoints.vnc_port), Duration::from_secs(5))
            .map_err(|e| not_ready(Stage::Display, format!("VNC: {e}")))?;
        let frame = vnc.refresh().map_err(|e| not_ready(Stage::Display, format!("VNC: {e}")))?;
        let black = bctl_rfb::Rgb(0, 0, 0);
        let drawn = (0..frame.height()).step_by(16).any(|y| (0..frame.width()).step_by(16).any(|x| frame.pixel(x, y) != black));
        if !drawn {
            return Err(not_ready(Stage::Display, "framebuffer is still black".into()));
        }
        Ok(Ready { input: report.input, browser })
    }
}

/// The `Browser` field of `/json/version` on a host loopback port, if CDP answers there.
pub fn cdp_browser(port: u16) -> Option<String> {
    let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    version_browser(stream, &format!("127.0.0.1:{port}"))
}

/// The `Browser` field of `/json/version` on a Unix socket, if CDP answers there.
pub fn cdp_socket_browser(socket: &Path) -> Option<String> {
    let stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    version_browser(stream, "localhost")
}

fn version_browser(mut stream: impl Read + Write, host: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Version {
        #[serde(rename = "Browser")]
        browser: String,
    }
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: {host}\r\n\r\n").ok()?;
    let mut response = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        response.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&response);
        let (head, body) = text.split_once("\r\n\r\n")?;
        let length: usize = head
            .lines()
            .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))?
            .parse()
            .ok()?;
        if body.len() >= length {
            return json::from_str::<Version>(&body[..length]).ok().map(|v| v.browser);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guest_report_names_the_first_missing_stage_in_order() {
        let report = GuestReport::parse("seat ok\ncompositor missing\nbrowser missing\nsandbox missing\ninput ok\n").unwrap();
        assert_eq!(report.first_missing(), Some(Stage::Compositor));
        assert_eq!(report.input, InputSupport::Interactive);
    }

    #[test]
    fn absent_input_is_view_only_not_a_failure() {
        let report = GuestReport::parse("seat ok\ncompositor ok\nbrowser ok\nsandbox ok\ninput absent\n").unwrap();
        assert_eq!((report.first_missing(), report.input), (None, InputSupport::ViewOnly));
    }

    #[test]
    fn incomplete_or_unknown_guest_output_is_rejected() {
        assert!(GuestReport::parse("seat ok\ncompositor ok\nbrowser ok\ninput ok\n").is_err());
        assert!(GuestReport::parse("seat ok\ncompositor ok\nbrowser ok\nsandbox ok\n").is_err());
        assert!(GuestReport::parse("seat ok\ngpu ok\ncompositor ok\nbrowser ok\nsandbox ok\ninput ok\n").is_err());
        assert!(GuestReport::parse("seat maybe\ncompositor ok\nbrowser ok\nsandbox ok\ninput ok\n").is_err());
    }
}
