use std::path::PathBuf;

/// Where one bctl installation keeps its state. `BCTL_HOME` overrides the per-user default so independent
/// installations (tests, a second checkout) never share a daemon, database, or socket.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Paths, String> {
        if let Some(home) = std::env::var_os("BCTL_HOME") {
            return Ok(Paths { home: PathBuf::from(home) });
        }
        if cfg!(target_os = "macos") {
            let user_home = std::env::var_os("HOME").ok_or("HOME is not set")?;
            return Ok(Paths { home: PathBuf::from(user_home).join("Library/Application Support/bctl") });
        }
        if let Some(state) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
            return Ok(Paths { home: PathBuf::from(state).join("bctl") });
        }
        let user_home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        Ok(Paths { home: PathBuf::from(user_home).join(".local/state/bctl") })
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
        self.home.join("runtime/smolvm/smolvm")
    }

    pub fn nanos_runtime(&self) -> PathBuf {
        self.home.join("runtime/nanos")
    }

    pub fn images(&self) -> PathBuf {
        self.home.join("images")
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
