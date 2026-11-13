use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where one bctl installation keeps its state. `BCTL_HOME` overrides the per-user default so independent
/// installations (tests, a second checkout) never share a daemon, database, or socket.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub data: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Paths, String> {
        Self::resolve(cfg!(target_os = "macos"), |key| std::env::var_os(key))
    }

    fn resolve(macos: bool, env: impl Fn(&str) -> Option<OsString>) -> Result<Paths, String> {
        if let Some(home) = env("BCTL_HOME").filter(|v| !v.is_empty()) {
            let home = PathBuf::from(home);
            return Ok(Paths { data: home.clone(), home });
        }
        if macos {
            let home = PathBuf::from(env("HOME").ok_or("HOME is not set")?).join("Library/Application Support/bctl");
            return Ok(Paths { data: home.clone(), home });
        }
        let root = |name: &str, default: &str| -> Result<PathBuf, String> {
            match env(name).filter(|v| !v.is_empty()) {
                Some(value) => Ok(PathBuf::from(value).join("bctl")),
                None => Ok(PathBuf::from(env("HOME").ok_or("HOME is not set")?).join(default)),
            }
        };
        Ok(Paths { home: root("XDG_STATE_HOME", ".local/state/bctl")?, data: root("XDG_DATA_HOME", ".local/share/bctl")? })
    }

    fn nanos_runtime_for_exe(&self, exe: Option<&Path>) -> PathBuf {
        let bundled = exe.and_then(Path::parent).and_then(Path::parent).map(|release| release.join("runtime/nanos"));
        bundled.filter(|dir| dir.is_dir()).unwrap_or_else(|| self.data.join("runtime/nanos"))
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join("bctl.sock")
    }

    pub fn database(&self) -> PathBuf {
        self.home.join("state.db")
    }

    pub fn lock(&self) -> PathBuf {
        self.home.join("daemon.lock")
    }

    pub fn log(&self) -> PathBuf {
        self.home.join("daemon.log")
    }

    pub fn runtime(&self) -> PathBuf {
        self.data.join("runtime/smolvm/smolvm")
    }

    pub fn nanos_runtime(&self) -> PathBuf {
        self.nanos_runtime_for_exe(std::env::current_exe().ok().and_then(|exe| exe.canonicalize().ok()).as_deref())
    }

    pub fn images(&self) -> PathBuf {
        self.data.join("images")
    }

    /// Where a machine's CDP socket appears on the host, inside a directory of its own. The directory
    /// name is a hash of the machine name to stay within the ~104-byte Unix socket path limit.
    pub fn cdp_socket(&self, machine: &str) -> PathBuf {
        self.home.join("cdp").join(bctl_runtime::machine_socket_key(machine)).join("s")
    }

    pub fn run_dir(&self, run: i64) -> PathBuf {
        self.home.join("runs").join(run.to_string())
    }

    pub fn profile_dir(&self, profile: &str) -> PathBuf {
        self.home.join("profiles").join(profile)
    }

    /// A session's files directory, shared with its guest on runtimes that mount host directories.
    pub fn files_dir(&self, session: &str) -> PathBuf {
        self.home.join("files").join(session)
    }

    pub fn image(&self, name: &str) -> PathBuf {
        self.images().join(format!("{name}.tar"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(paths: &Paths, data: &str, state: &str) {
        assert_eq!(paths.images(), Path::new(data).join("images"));
        assert_eq!(paths.runtime(), Path::new(data).join("runtime/smolvm/smolvm"));
        assert_eq!(paths.nanos_runtime_for_exe(None), Path::new(data).join("runtime/nanos"));
        assert_eq!(paths.database(), Path::new(state).join("state.db"));
        assert_eq!(paths.socket(), Path::new(state).join("bctl.sock"));
        assert_eq!(paths.lock(), Path::new(state).join("daemon.lock"));
        assert_eq!(paths.profile_dir("p"), Path::new(state).join("profiles/p"));
    }

    #[test]
    fn roots_follow_home_platform_and_xdg() {
        let resolve = |macos, vars: &[(&str, &str)]| Paths::resolve(macos, |key| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| OsString::from(v))).unwrap();
        check(&resolve(false, &[("BCTL_HOME", "/isolated")]), "/isolated", "/isolated");
        check(&resolve(true, &[("HOME", "/user")]), "/user/Library/Application Support/bctl", "/user/Library/Application Support/bctl");
        check(&resolve(false, &[("XDG_DATA_HOME", "/data"), ("XDG_STATE_HOME", "/state")]), "/data/bctl", "/state/bctl");
        check(&resolve(false, &[("HOME", "/user")]), "/user/.local/share/bctl", "/user/.local/state/bctl");
        check(&resolve(false, &[("HOME", "/user"), ("BCTL_HOME", ""), ("XDG_DATA_HOME", ""), ("XDG_STATE_HOME", "")]), "/user/.local/share/bctl", "/user/.local/state/bctl");
    }

    #[test]
    fn nanos_runtime_prefers_bundle_and_falls_back_to_data() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("runtime-paths-{}", std::process::id()));
        let paths = Paths { home: root.join("state"), data: root.join("data") };
        let exe = root.join("release/bin/bctl");
        assert_eq!(paths.nanos_runtime_for_exe(Some(&exe)), root.join("data/runtime/nanos"));
        std::fs::create_dir_all(root.join("release/bin")).unwrap();
        std::fs::create_dir_all(root.join("release/runtime/nanos")).unwrap();
        assert_eq!(paths.nanos_runtime_for_exe(Some(&exe)), root.join("release/runtime/nanos"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
