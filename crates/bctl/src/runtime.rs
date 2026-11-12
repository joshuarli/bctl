use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bctl_core::{Endpoints, MachineName, MachineObservation, MachineState};
use bctl_runtime::{Ready, Stage};
use bctl_runtime_nanos::{Backend, Nanos, Toolchain};
use bctl_runtime_smolvm::{LaunchSource, Removal, SmolVm};

use crate::paths::Paths;

/// Which VMM a host runs sessions on. One host runs one runtime: SmolVM by default, on Apple Silicon macOS and
/// Linux x86_64 with KVM, or hydrogen's Nanos when `BCTL_RUNTIME=nanos` asks for it.
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

    /// `BCTL_RUNTIME` picks one explicitly; otherwise SmolVM, the only runtime with headed sessions.
    pub(crate) fn for_host() -> Result<RuntimeKind, String> {
        RuntimeKind::chosen(std::env::var("BCTL_RUNTIME").ok().as_deref())
    }

    fn chosen(requested: Option<&str>) -> Result<RuntimeKind, String> {
        match requested {
            Some("smolvm") => Ok(RuntimeKind::SmolVm),
            Some("nanos") => Ok(RuntimeKind::Nanos),
            Some(other) => Err(format!("BCTL_RUNTIME {other:?} is neither smolvm nor nanos")),
            None => Ok(RuntimeKind::SmolVm),
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
    IncompatibleProfile { message: String },
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
            RuntimeError::IncompatibleProfile { message } => f.write_str(message),
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

fn reject_nanos_profile(dir: &Path) -> Result<(), RuntimeError> {
    let path = dir.join(bctl_runtime_nanos::profile::PROFILE_DISK);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Err(RuntimeError::IncompatibleProfile { message: format!("{} is a Nanos profile and cannot be opened by SmolVM", dir.display()) }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimeError::IncompatibleProfile { message: format!("cannot inspect {}: {error}", path.display()) }),
    }
}

pub(crate) struct MachineLaunch {
    pub(crate) machine: MachineName,
    pub(crate) image: PathBuf,
    pub(crate) profile_dir: PathBuf,
    /// The session's files directory, mounted at the browser's download directory.
    pub(crate) files_dir: PathBuf,
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
            files_dir: Some(self.files_dir.clone()),
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
            profile_dir: self.profile_dir.clone(),
            files_dir: self.files_dir.clone(),
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
                    virtiofsd: dir.join("virtiofsd"),
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
                vec![paths.nanos_runtime().join("vhost-device-vsock"), paths.nanos_runtime().join("virtiofsd")],
                "build it with `runtime/build-nanos.sh` and copy its bin/vhost-device-vsock and bin/virtiofsd there",
            ),
        };
        match needed.into_iter().find(|path| !path.exists()) {
            Some(missing) => Err((missing, hint)),
            None => Ok(()),
        }
    }

    /// The runtime's receipt for `bctl doctor`, or why it cannot run sessions.
    pub(crate) fn receipt(&self, paths: &Paths) -> Result<String, String> {
        if let Err((missing, hint)) = self.installed(paths) {
            return Err(format!("missing {}: {hint}", missing.display()));
        }
        match self {
            Runtime::SmolVm(_) => {
                let runtime = paths.runtime();
                match std::process::Command::new(&runtime).arg("--version").output() {
                    Ok(output) if output.status.success() => {
                        Ok(format!("{} ({})", String::from_utf8_lossy(&output.stdout).trim(), runtime.display()))
                    }
                    _ => Err(format!("{} --version failed", runtime.display())),
                }
            }
            Runtime::Nanos(_) if cfg!(target_os = "macos") => Ok(format!("nanos on libkrun ({})", paths.nanos_runtime().display())),
            Runtime::Nanos(_) => Ok(format!("nanos on QEMU ({})", paths.nanos_runtime().display())),
        }
    }

    /// The images in `paths.images()` this runtime can boot, and those missing a file they need beside them.
    pub(crate) fn images(&self, paths: &Paths) -> (Vec<String>, Vec<String>) {
        let suffix = match self {
            Runtime::SmolVm(_) => ".tar",
            Runtime::Nanos(_) => ".img",
        };
        let mut names: Vec<String> = std::fs::read_dir(paths.images())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.strip_suffix(suffix).map(str::to_string))
            .filter(|name| !matches!(self, Runtime::Nanos(_)) || !(name.ends_with(".profile") || name.ends_with(".kernel")))
            .collect();
        names.sort();
        let (mut complete, mut incomplete) = (Vec::new(), Vec::new());
        for name in names {
            let image = self.image_path(paths, &name);
            let companions = match self {
                Runtime::SmolVm(_) => vec![],
                Runtime::Nanos(_) if cfg!(target_os = "macos") => vec![image.with_extension("profile.img")],
                Runtime::Nanos(_) => vec![image.with_extension("profile.img"), bctl_runtime_nanos::image_kernel(&image)],
            };
            match companions.iter().find(|path| !path.is_file()) {
                Some(missing) => incomplete.push(format!("{name} (no {})", missing.file_name().unwrap_or_default().to_string_lossy())),
                None => complete.push(name),
            }
        }
        (complete, incomplete)
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
                "build it with `BCTL_BROWSER_ARCHIVE=DIR sh images/{name}/build-macos.sh` and copy .work/images/{name}.img and .work/images/{name}.profile.img there"
            ),
            Runtime::Nanos(_) => format!(
                "build it with `images/{name}/build.sh` and copy .work/images/{name}.img, .work/images/{name}.profile.img and .work/images/{name}.kernel.img there"
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
            Runtime::SmolVm(smolvm) => {
                reject_nanos_profile(&launch.profile_dir)?;
                Ok(smolvm.launch(&launch.smolvm())?)
            }
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
            Runtime::SmolVm(smolvm) => {
                reject_nanos_profile(&launch.profile_dir)?;
                Ok(smolvm.resume(&launch.smolvm())?)
            }
            Runtime::Nanos(_) => Err(RuntimeError::Unsupported { runtime: RuntimeKind::Nanos, operation: "resume a session" }),
        }
    }

    pub(crate) fn remove(&self, machine: &MachineName) -> Result<(), RuntimeError> {
        match self {
            Runtime::SmolVm(smolvm) => Ok(smolvm.remove(machine)?),
            Runtime::Nanos(nanos) => Ok(nanos.remove(&machine.to_string())?),
        }
    }

    /// Stops the guest gracefully before deleting its VM; the Nanos supervisor's removal already does.
    pub(crate) fn stop_and_remove(&self, machine: &MachineName) -> Result<Removal, RuntimeError> {
        match self {
            Runtime::SmolVm(smolvm) => Ok(smolvm.stop_and_remove(machine)?),
            Runtime::Nanos(nanos) => {
                nanos.remove(&machine.to_string())?;
                Ok(Removal::Stopped)
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unnamed_runtime_is_smolvm_on_every_host_and_a_named_one_is_honoured() {
        assert_eq!(RuntimeKind::chosen(None), Ok(RuntimeKind::SmolVm));
        assert_eq!(RuntimeKind::chosen(Some("nanos")), Ok(RuntimeKind::Nanos));
        assert_eq!(RuntimeKind::chosen(Some("smolvm")), Ok(RuntimeKind::SmolVm));
        assert!(RuntimeKind::chosen(Some("qemu")).is_err());
    }

    #[test]
    fn doctor_lists_nanos_images_and_names_a_missing_companion() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("doctor-images-{}", std::process::id()));
        let images = home.join("images");
        std::fs::create_dir_all(&images).unwrap();
        let mut files = vec!["complete.img", "complete.profile.img", "bare.img", "other.tar"];
        if !cfg!(target_os = "macos") {
            files.push("complete.kernel.img");
        }
        for file in files {
            std::fs::write(images.join(file), b"").unwrap();
        }
        let paths = Paths { home: home.clone() };
        let runtime = Runtime::new(RuntimeKind::Nanos, &paths).unwrap();
        assert_eq!(runtime.images(&paths), (vec!["complete".to_string()], vec!["bare (no bare.profile.img)".to_string()]));
        let smolvm = Runtime::new(RuntimeKind::SmolVm, &paths).unwrap();
        assert_eq!(smolvm.images(&paths), (vec!["other".to_string()], vec![]));
        std::fs::remove_dir_all(home).unwrap();
    }
}
