use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use bctl_core::{Endpoints, MachineName, MachineObservation, MachineState};
use bctl_runtime::{InputSupport, Ready, Stage, cdp_socket_browser};
use bctl_rfb::Client;
use miniserde::{Deserialize, json};

const GUEST_CDP_SOCKET: &str = "/run/bctl/cdp.sock";

// The guest appliance stops a headed session in up to 15s for the browser, 5s for the compositor and 5s for the seat
// (guest/appliance/src/supervise.rs), and a headless one in 8s plus 7s (headless.rs), before it syncs.
pub const GRACEFUL_STOP_SECS: u64 = 30;

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

#[derive(Debug, Clone)]
pub enum LaunchSource {
    Image(PathBuf),
    Checkpoint(PathBuf),
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub machine: MachineName,
    pub source: LaunchSource,
    /// Host directory mounted at `/profile`, so the browser profile outlives the VM.
    pub profile_dir: Option<PathBuf>,
    /// Host directory mounted at [`bctl_runtime::GUEST_FILES_DIR`], shared with the guest for uploads and downloads.
    pub files_dir: Option<PathBuf>,
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

/// The appliance parked at its pre-browser forkpoint, captured without a guest network: SmolVM cannot yet
/// restore a machine that has both virtio-net and the packed-layers virtiofs, so restored sessions have no
/// guest network.
#[derive(Debug, Clone)]
pub struct TemplateSpec {
    pub machine: String,
    pub image: PathBuf,
    pub checkpoint: PathBuf,
    pub staging_dir: PathBuf,
    pub display: (u16, u16),
    pub memory_mib: u32,
    pub cpus: u8,
    /// Host path where the guest's CDP socket appears; its directory must already exist.
    pub cdp_socket: PathBuf,
}

const TEMPLATE_WORKLOAD: &str =
    r#"{"entrypoint":["/usr/local/bin/smolvm-branch-ready","--","/usr/local/bin/bctl-guest","run"],"forkable":true}"#;

/// How a machine left. `Forced` means the guest was not asked to stop, or did not stop, before its VM was deleted,
/// so what its workload would have saved on a clean stop may be lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    Stopped,
    Forced { stop_failure: RuntimeError },
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

    fn run_input(&self, operation: &'static str, args: &[&str], input: &str, env: &[(&str, String)]) -> Result<String, RuntimeError> {
        use std::process::Stdio;
        let mut child = Command::new(&self.binary)
            .args(args)
            .arg("--error-format")
            .arg("json")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RuntimeError::Spawn { message: e.to_string() })?;
        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(input.as_bytes())
            .map_err(|e| RuntimeError::Spawn { message: e.to_string() })?;
        let output = child.wait_with_output().map_err(|e| RuntimeError::Spawn { message: e.to_string() })?;
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
        Ok(machines
            .into_iter()
            .map(|m| {
                let state = match m.state.as_str() {
                    "running" => MachineState::Running,
                    "paused" => MachineState::Paused,
                    _ => MachineState::Other,
                };
                MachineObservation { name: m.name, state }
            })
            .collect())
    }

    /// Creates and starts the VM. The display and VNC endpoint are bound at start, so they travel with it.
    pub fn launch(&self, spec: &LaunchSpec) -> Result<(), RuntimeError> {
        let name = spec.machine.to_string();
        let socket = format!("{GUEST_CDP_SOCKET}:{}", spec.cdp_socket.to_string_lossy());
        let memory = spec.memory_mib.to_string();
        let cpus = spec.cpus.to_string();
        let mut create: Vec<String> = vec!["machine".into(), "create".into(), "--name".into(), name.clone()];
        match &spec.source {
            LaunchSource::Image(image) => {
                create.extend(["--gpu", "--gpu-mode", "2d", "--mem", &memory, "--cpus", &cpus].map(str::to_string));
                create.extend(["--expose-socket", &socket, "--block-private"].map(str::to_string));
                create.push("--image".into());
                create.push(image.to_string_lossy().into_owned());
            }
            LaunchSource::Checkpoint(checkpoint) => {
                create.push("--from".into());
                create.push(checkpoint.to_string_lossy().into_owned());
                create.extend(["--expose-socket", &socket].map(str::to_string));
            }
        }
        if matches!(&spec.source, LaunchSource::Image(_)) {
            let profile = spec.profile_dir.as_ref().map(|d| format!("{}:/profile", d.to_string_lossy()));
            let files = spec.files_dir.as_ref().map(|d| format!("{}:{}", d.to_string_lossy(), bctl_runtime::GUEST_FILES_DIR));
            for volume in [profile, files].into_iter().flatten() {
                create.push("-v".into());
                create.push(volume);
            }
        }
        if matches!(&spec.source, LaunchSource::Image(_)) {
            for range in &spec.allow_private {
                create.push("--allow-private".into());
                create.push(range.clone());
            }
        }
        let args: Vec<&str> = create.iter().map(String::as_str).collect();
        self.run("machine create", &args, &[])?;
        if let Err(error) = self.run("machine start", &["machine", "start", "--name", &name], &display_env(spec)) {
            let _ = self.remove(&spec.machine);
            return Err(error);
        }
        Ok(())
    }

    /// Checkpoints the running VM in place and stops its vCPUs; the browser's state lives in the checkpoint.
    pub fn pause(&self, machine: &MachineName) -> Result<(), RuntimeError> {
        let name = machine.to_string();
        self.run("machine pause", &["machine", "pause", "--name", &name], &[]).map(drop)
    }

    /// Restores a paused VM. The restored devices must match the checkpoint, so the display and VNC endpoint are
    /// the ones the VM was launched with.
    pub fn resume(&self, spec: &LaunchSpec) -> Result<(), RuntimeError> {
        let name = spec.machine.to_string();
        self.run("machine resume", &["machine", "resume", "--name", &name], &display_env(spec)).map(drop)
    }

    /// Deletes the VM at once, killing its workload without a graceful stop; a machine that is already gone is not
    /// an error.
    pub fn remove(&self, machine: &MachineName) -> Result<(), RuntimeError> {
        self.delete_named(&machine.to_string())
    }

    /// Gives the guest [`GRACEFUL_STOP_SECS`] to stop its workload and sync, then deletes the VM. A guest that
    /// cannot be stopped is deleted anyway and reported as [`Removal::Forced`]. A paused machine has no running
    /// workload to stop, so it is deleted directly.
    pub fn stop_and_remove(&self, machine: &MachineName) -> Result<Removal, RuntimeError> {
        let name = machine.to_string();
        let paused = self.list().is_ok_and(|machines| machines.iter().any(|m| m.name == name && m.state == MachineState::Paused));
        let removal = if paused {
            Removal::Stopped
        } else {
            let timeout = GRACEFUL_STOP_SECS.to_string();
            match self.run("machine stop", &["machine", "stop", "--name", &name, "--timeout", &timeout], &[]) {
                Ok(_) => Removal::Stopped,
                Err(RuntimeError::Smolvm { code, .. }) if code == "NOT_FOUND" => Removal::Stopped,
                Err(stop_failure) => Removal::Forced { stop_failure },
            }
        };
        self.delete_named(&name)?;
        Ok(removal)
    }

    pub fn delete_named(&self, name: &str) -> Result<(), RuntimeError> {
        match self.run("machine delete", &["machine", "delete", "--name", name, "-f"], &[]) {
            Ok(_) => Ok(()),
            Err(RuntimeError::Smolvm { code, .. }) if code == "NOT_FOUND" => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn logs(&self, machine: &MachineName) -> Result<String, RuntimeError> {
        let name = machine.to_string();
        self.run("machine logs", &["machine", "logs", "--name", &name, "--tail", "40"], &[])
    }

    /// Deletes the source machine whether or not the capture succeeds.
    pub fn create_template(&self, spec: &TemplateSpec) -> Result<(), RuntimeError> {
        let name = spec.machine.as_str();
        let _ = self.delete_named(name);
        if let Err(error) = std::fs::remove_file(&spec.checkpoint)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(RuntimeError::Spawn { message: format!("remove {}: {error}", spec.checkpoint.display()) });
        }
        if let Err(error) = std::fs::create_dir_all(&spec.staging_dir) {
            return Err(RuntimeError::Spawn { message: format!("create {}: {error}", spec.staging_dir.display()) });
        }
        let image = spec.image.to_string_lossy();
        let memory = spec.memory_mib.to_string();
        let cpus = spec.cpus.to_string();
        let socket = format!("{GUEST_CDP_SOCKET}:{}", spec.cdp_socket.to_string_lossy());
        let create = vec![
            "machine", "create", "--name", name, "--spec-stdin", "--gpu", "--gpu-mode", "2d", "--mem", &memory, "--cpus", &cpus,
            "--expose-socket", &socket, "--image", &image,
        ];
        let checkpoint = spec.checkpoint.to_string_lossy();
        let staging = spec.staging_dir.to_string_lossy();
        let result = (|| {
            self.run_input("machine create", &create, TEMPLATE_WORKLOAD, &[])?;
            // SmolVM's restore leaves the guest agent unanswered unless the capture had the same display
            // and VNC surfaces as the restore.
            let vnc = free_port().map_err(|e| RuntimeError::Spawn { message: format!("find a VNC port: {e}") })?;
            self.run(
                "machine start",
                &["machine", "start", "--name", name],
                &[
                    ("SMOLVM_DISPLAY", format!("{}x{}", spec.display.0, spec.display.1)),
                    ("SMOLVM_VNC", format!("127.0.0.1:{vnc}")),
                ],
            )?;
            self.run(
                "machine checkpoint",
                &[
                    "machine", "checkpoint", "--name", name, "--output", &checkpoint, "--host-mounts", "--wait-ready", "--ready-timeout",
                    "120", "--staging-dir", &staging,
                ],
                &[],
            )?;
            Ok(())
        })();
        let _ = self.delete_named(name);
        result
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
        let running = self.list()?.into_iter().any(|m| m.name == name && m.state == MachineState::Running);
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

fn display_env(spec: &LaunchSpec) -> [(&'static str, String); 2] {
    [
        ("SMOLVM_DISPLAY", format!("{}x{}", spec.display.0, spec.display.1)),
        ("SMOLVM_VNC", format!("127.0.0.1:{}", spec.endpoints.vnc_port)),
    ]
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
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
