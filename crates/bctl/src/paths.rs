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
        let user_home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        Ok(Paths { home: PathBuf::from(user_home).join("Library/Application Support/bctl") })
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

    pub fn images(&self) -> PathBuf {
        self.home.join("images")
    }

    /// Where a machine's CDP socket appears on the host, inside a directory of its own. The directory
    /// name is a hash of the machine name to stay within the ~104-byte Unix socket path limit.
    pub fn cdp_socket(&self, machine: &str) -> PathBuf {
        let hash = machine.bytes().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3));
        self.home.join("cdp").join(format!("{hash:016x}")).join("s")
    }

    pub fn run_dir(&self, run: i64) -> PathBuf {
        self.home.join("runs").join(run.to_string())
    }

    pub fn profile_dir(&self, profile: &str) -> PathBuf {
        self.home.join("profiles").join(profile)
    }

    pub fn image(&self, name: &str) -> PathBuf {
        self.images().join(format!("{name}.tar"))
    }
}
