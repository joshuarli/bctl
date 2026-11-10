use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bctl_core::{Endpoints, MachineName, MachineObservation, MachineState};
use bctl_runtime::{Ready, Stage};
use bctl_runtime_nanos::{Backend, Nanos, Toolchain};
use bctl_runtime_smolvm::{LaunchSource, SmolVm};

use crate::paths::Paths;

/// Which VMM a host runs sessions on. One host runs one runtime: SmolVM on Apple Silicon macOS, hydrogen's
/// Nanos on Linux x86_64 with KVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeKind {
    SmolVm,
    Nanos,
}

impl RuntimeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RuntimeKind::SmolVm => "smolvm",
            RuntimeKind::Nanos => "nanos",
        }
    }

    /// `BCTL_RUNTIME` picks one explicitly; otherwise the host's platform decides.
    pub(crate) fn for_host() -> Result<RuntimeKind, String> {
        match std::env::var("BCTL_RUNTIME").as_deref() {
            Ok("smolvm") => Ok(RuntimeKind::SmolVm),
            Ok("nanos") => Ok(RuntimeKind::Nanos),
            Ok(other) => Err(format!("BCTL_RUNTIME {other:?} is neither smolvm nor nanos")),
            Err(_) if cfg!(target_os = "macos") => Ok(RuntimeKind::SmolVm),
            Err(_) => Ok(RuntimeKind::Nanos),
        }
    }

    pub(crate) fn default_image(self) -> &'static str {
        match self {
            RuntimeKind::SmolVm => "helium",
            RuntimeKind::Nanos => "nanos-helium",
        }
    }
}

#[derive(Debug)]
pub(crate) enum RuntimeError {
    SmolVm(bctl_runtime_smolvm::RuntimeError),
    Nanos(bctl_runtime_nanos::RuntimeError),
    MissingKernel { image: PathBuf, kernel: PathBuf, hint: String },
    Unsupported { runtime: RuntimeKind, operation: &'static str },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::SmolVm(error) => error.fmt(f),
            RuntimeError::Nanos(error) => error.fmt(f),
            RuntimeError::MissingKernel { image, kernel, hint } => {
                write!(f, "no kernel {} for image {}; {hint}", kernel.display(), image.display())
            }
            RuntimeError::Unsupported { runtime, operation } => write!(f, "the {} runtime cannot {operation}", runtime.as_str()),
        }
    }
}

impl From<bctl_runtime_smolvm::RuntimeError> for RuntimeError {
    fn from(error: bctl_runtime_smolvm::RuntimeError) -> Self {
        RuntimeError::SmolVm(error)
    }
}

impl From<bctl_runtime_nanos::RuntimeError> for RuntimeError {
    fn from(error: bctl_runtime_nanos::RuntimeError) -> Self {
        RuntimeError::Nanos(error)
    }
}

pub(crate) struct MachineLaunch {
    pub(crate) machine: MachineName,
    pub(crate) image: PathBuf,
    pub(crate) profile_dir: PathBuf,
    pub(crate) endpoints: Endpoints,
    pub(crate) cdp_socket: PathBuf,
    pub(crate) display: (u16, u16),
    pub(crate) memory_mib: u32,
    pub(crate) cpus: u8,
    pub(crate) allow_private: Vec<String>,
}

impl MachineLaunch {
    fn smolvm(&self) -> bctl_runtime_smolvm::LaunchSpec {
        bctl_runtime_smolvm::LaunchSpec {
            machine: self.machine.clone(),
            source: LaunchSource::Image(self.image.clone()),
            profile_dir: Some(self.profile_dir.clone()),
            endpoints: self.endpoints,
            cdp_socket: self.cdp_socket.clone(),
            display: self.display,
            memory_mib: self.memory_mib,
            cpus: self.cpus,
            allow_private: self.allow_private.clone(),
        }
    }

    fn nanos(&self) -> bctl_runtime_nanos::LaunchSpec {
        bctl_runtime_nanos::LaunchSpec {
            machine: self.machine.to_string(),
            image: self.image.clone(),
            cdp_socket: self.cdp_socket.clone(),
            memory_mib: self.memory_mib,
            cpus: self.cpus,
            allow_private: self.allow_private.clone(),
        }
    }
}

/// The hidden `bctl` subcommand that runs the Nanos VM supervisor.
pub(crate) const NANOS_SUPERVISOR_COMMAND: &str = "nanos-vm";

pub(crate) enum Runtime {
    SmolVm(SmolVm),
    Nanos(Nanos),
}

#[cfg(not(target_os = "macos"))]
fn on_path(name: &str) -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| candidate.is_file()))
        .unwrap_or_else(|| PathBuf::from(name))
}

impl Runtime {
    pub(crate) fn new(kind: RuntimeKind, paths: &Paths) -> Result<Runtime, String> {
        Ok(match kind {
            RuntimeKind::SmolVm => Runtime::SmolVm(SmolVm::new(paths.runtime())),
            RuntimeKind::Nanos => {
                let dir = paths.nanos_runtime();
                let supervisor = std::env::current_exe().map_err(|e| format!("cannot find the bctl executable: {e}"))?;
                #[cfg(target_os = "macos")]
                let backend = Backend::Libkrun { runner: dir.join("nanos-krun"), firmware: dir.join("firmware.fd") };
                #[cfg(not(target_os = "macos"))]
                let backend = Backend::Qemu {
                    qemu: on_path("qemu-system-x86_64"), qemu_img: on_path("qemu-img"), vsock_backend: dir.join("vhost-device-vsock"),
                };
                Runtime::Nanos(Nanos::new(
                    paths.home.join("machines"),
                    Toolchain {
                        supervisor,
                        supervisor_args: vec![NANOS_SUPERVISOR_COMMAND.to_string()],
                        backend,
                    },
                ))
            }
        })
    }

    pub(crate) fn kind(&self) -> RuntimeKind {
        match self {
            Runtime::SmolVm(_) => RuntimeKind::SmolVm,
            Runtime::Nanos(_) => RuntimeKind::Nanos,
        }
    }

    /// The file that must exist before a session can start, and how to provide it.
    pub(crate) fn installed(&self, paths: &Paths) -> Result<(), (PathBuf, &'static str)> {
        let (needed, hint): (Vec<PathBuf>, &'static str) = match self {
            Runtime::SmolVm(_) => (vec![paths.runtime()], "build it with `runtime/build.sh` and copy .work/runtime/smolvm there"),
            Runtime::Nanos(_) if cfg!(target_os = "macos") => (
                vec![paths.nanos_runtime().join("nanos-krun"), paths.nanos_runtime().join("firmware.fd"), paths.nanos_runtime().join("lib/libkrun.dylib")],
                "build it with `runtime/build-nanos-krun.sh` and copy .work/runtime/nanos-krun there",
            ),
            Runtime::Nanos(_) => (
                vec![paths.nanos_runtime().join("vhost-device-vsock")],
                "build it with `runtime/build-nanos.sh` and copy its bin/vhost-device-vsock there",
            ),
        };
        match needed.into_iter().find(|path| !path.exists()) {
            Some(missing) => Err((missing, hint)),
            None => Ok(()),
        }
    }

    pub(crate) fn image_path(&self, paths: &Paths, name: &str) -> PathBuf {
        match self {
            Runtime::SmolVm(_) => paths.image(name),
            Runtime::Nanos(_) => paths.images().join(format!("{name}.img")),
        }
    }

    pub(crate) fn image_hint(&self, name: &str) -> String {
        match self {
            Runtime::SmolVm(_) => format!("build it with `images/build.sh {name}` and copy the archive there"),
            Runtime::Nanos(_) if cfg!(target_os = "macos") => format!(
                "build it with `BCTL_BROWSER_ARCHIVE=DIR sh images/{name}/build-macos.sh` and copy .work/images/{name}.img there"
            ),
            Runtime::Nanos(_) => format!(
                "build it with `images/{name}/build.sh` and copy .work/images/{name}.img and .work/images/{name}.kernel.img there"
            ),
        }
    }

    pub(crate) fn list(&self) -> Result<Vec<MachineObservation>, RuntimeError> {
        match self {
            Runtime::SmolVm(smolvm) => Ok(smolvm.list()?),
            Runtime::Nanos(nanos) => {
                Ok(nanos.list()?.into_iter().map(|name| MachineObservation { name, state: MachineState::Running }).collect())
            }
        }
    }

    pub(crate) fn launch(&self, launch: &MachineLaunch) -> Result<(), RuntimeError> {
        match self {
            Runtime::SmolVm(smolvm) => Ok(smolvm.launch(&launch.smolvm())?),
            Runtime::Nanos(nanos) => {
                let kernel = bctl_runtime_nanos::image_kernel(&launch.image);
                if !cfg!(target_os = "macos") && !kernel.is_file() {
                    let name = launch.image.file_stem().unwrap_or_default().to_string_lossy();
                    return Err(RuntimeError::MissingKernel { image: launch.image.clone(), kernel, hint: self.image_hint(&name) });
                }
                Ok(nanos.launch(&launch.nanos())?)
            }
        }
    }

    pub(crate) fn pause(&self, machine: &MachineName) -> Result<(), RuntimeError> {
        match self {
            Runtime::SmolVm(smolvm) => Ok(smolvm.pause(machine)?),
            Runtime::Nanos(_) => Err(RuntimeError::Unsupported { runtime: RuntimeKind::Nanos, operation: "pause a session" }),
        }
    }

    pub(crate) fn resume(&self, launch: &MachineLaunch) -> Result<(), RuntimeError> {
        match self {
            Runtime::SmolVm(smolvm) => Ok(smolvm.resume(&launch.smolvm())?),
            Runtime::Nanos(_) => Err(RuntimeError::Unsupported { runtime: RuntimeKind::Nanos, operation: "resume a session" }),
        }
    }

    pub(crate) fn remove(&self, machine: &MachineName) -> Result<(), RuntimeError> {
        match self {
            Runtime::SmolVm(smolvm) => Ok(smolvm.remove(machine)?),
            Runtime::Nanos(nanos) => Ok(nanos.remove(&machine.to_string())?),
        }
    }

    pub(crate) fn logs(&self, machine: &MachineName) -> Result<String, RuntimeError> {
        match self {
            Runtime::SmolVm(smolvm) => Ok(smolvm.logs(machine)?),
            Runtime::Nanos(nanos) => Ok(nanos.logs(&machine.to_string(), 40)?),
        }
    }

    pub(crate) fn wait_ready(
        &self,
        machine: &MachineName,
        endpoints: Endpoints,
        cdp_socket: &Path,
        timeout: Duration,
        on_stage: impl FnMut(Stage),
    ) -> Result<Ready, RuntimeError> {
        match self {
            Runtime::SmolVm(smolvm) => Ok(smolvm.wait_ready(machine, endpoints, cdp_socket, timeout, on_stage)?),
            Runtime::Nanos(nanos) => Ok(nanos.wait_ready(&machine.to_string(), cdp_socket, timeout, on_stage)?),
        }
    }
}
