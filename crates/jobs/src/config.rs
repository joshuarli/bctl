use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use bctl_core::{InvalidName, ProfileName, Resources, SessionName};

#[derive(Debug, Clone)]
pub struct Config {
    pub browsers: BTreeMap<BrowserName, BrowserSettings>,
    pub jobs: BTreeMap<SessionName, Job>,
}

#[derive(Debug, Clone)]
pub struct BrowserSettings {
    pub image: String,
    pub viewport: [u32; 2],
    pub cpus: u16,
    pub memory_mib: u32,
}

/// A `[browsers]` key validated with the same rules as a session name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrowserName(SessionName);

impl BrowserName {
    pub fn new(name: &str) -> Result<BrowserName, InvalidName> {
        SessionName::new(name).map(BrowserName)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for BrowserName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    pub name: SessionName,
    pub browser: BrowserName,
    pub profile: ProfileName,
    pub kind: JobKind,
}

#[derive(Debug, Clone)]
pub enum JobKind {
    Grab(GrabJob),
    Exec(ExecJob),
}

/// `robots` and `max_runtime` are unset when the file omitted them; `resolve` fills `robots` with `true`.
#[derive(Debug, Clone)]
pub struct GrabJob {
    pub urls: Vec<String>,
    pub max_pages: Option<u32>,
    pub jobs: Option<u16>,
    pub robots: Option<bool>,
    pub max_runtime: Option<Duration>,
}

/// `cwd` stays relative to the directory holding `bctl.toml`.
#[derive(Debug, Clone)]
pub struct ExecJob {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Option<Duration>,
}

/// A job joined with the settings of the browser it names, with every default applied.
#[derive(Debug, Clone)]
pub struct ResolvedJob {
    pub name: SessionName,
    pub browser: BrowserName,
    pub profile: ProfileName,
    pub image: String,
    pub viewport: [u32; 2],
    pub cpus: u16,
    pub memory_mib: u32,
    pub kind: ResolvedKind,
}

impl ResolvedJob {
    pub fn resources(&self) -> Resources {
        Resources { viewport: self.viewport, cpus: self.cpus, memory_mib: self.memory_mib }
    }
}

#[derive(Debug, Clone)]
pub enum ResolvedKind {
    Grab(ResolvedGrab),
    Exec(ResolvedExec),
}

#[derive(Debug, Clone)]
pub struct ResolvedGrab {
    pub urls: Vec<String>,
    pub max_pages: Option<u32>,
    pub jobs: Option<u16>,
    pub robots: bool,
    pub max_runtime: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct ResolvedExec {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Option<Duration>,
}
