use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum ConfigError {
    Toml { message: String },
    UnsupportedVersion { found: i64 },
    InvalidName { table: String, source: bctl_core::InvalidName },
    UnknownBrowser { job: String, browser: String },
    InvalidValue { path: String, reason: String },
    NoSuchJob { name: String },
    Read { path: PathBuf, source: io::Error },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Toml { message } => f.write_str(message),
            ConfigError::UnsupportedVersion { found } => {
                write!(f, "unsupported bctl.toml version {found}, expected version 1")
            }
            ConfigError::InvalidName { table, source } => {
                write!(f, "invalid name {:?} in {table}: {}", source.name, source.reason)
            }
            ConfigError::UnknownBrowser { job, browser } => {
                write!(f, "job {job:?} references unknown browser {browser:?}")
            }
            ConfigError::InvalidValue { path, reason } => write!(f, "invalid value at {path}: {reason}"),
            ConfigError::NoSuchJob { name } => write!(f, "no job named {name:?}"),
            ConfigError::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::InvalidName { source, .. } => Some(source),
            ConfigError::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}
