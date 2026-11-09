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

    pub fn profile_dir(&self, profile: &str) -> PathBuf {
        self.home.join("profiles").join(profile)
    }

    pub fn image(&self, name: &str) -> PathBuf {
        self.images().join(format!("{name}.tar"))
    }
}
