mod config;
mod error;
mod raw;
mod value;

#[cfg(test)]
mod tests;

pub use config::{
    BrowserName, BrowserSettings, Config, ExecJob, GrabJob, Job, JobKind, ResolvedExec, ResolvedGrab, ResolvedJob,
    ResolvedKind,
};
pub use error::ConfigError;

use std::path::Path;

use bctl_core::SessionName;

pub fn parse(source: &str) -> Result<Config, ConfigError> {
    toml::from_str::<raw::RawConfig>(source)
        .map_err(|error| ConfigError::Toml { message: error.to_string() })?
        .into_config()
}

pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let source = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&source)
}

pub fn resolve(config: &Config, job: &str) -> Result<ResolvedJob, ConfigError> {
    let name = SessionName::new(job).map_err(|_| ConfigError::NoSuchJob { name: job.to_string() })?;
    let found = config
        .jobs
        .get(&name)
        .ok_or_else(|| ConfigError::NoSuchJob { name: job.to_string() })?;
    let settings = config.browsers.get(&found.browser).ok_or_else(|| ConfigError::UnknownBrowser {
        job: job.to_string(),
        browser: found.browser.to_string(),
    })?;
    let kind = match &found.kind {
        JobKind::Grab(grab) => ResolvedKind::Grab(ResolvedGrab {
            urls: grab.urls.clone(),
            max_pages: grab.max_pages,
            jobs: grab.jobs,
            robots: grab.robots.unwrap_or(true),
            max_runtime: grab.max_runtime,
        }),
        JobKind::Exec(exec) => ResolvedKind::Exec(ResolvedExec {
            argv: exec.argv.clone(),
            cwd: exec.cwd.clone(),
            timeout: exec.timeout,
        }),
    };
    Ok(ResolvedJob {
        name,
        browser: found.browser.clone(),
        profile: found.profile.clone(),
        image: settings.image.clone(),
        viewport: settings.viewport,
        cpus: settings.cpus,
        memory_mib: settings.memory_mib,
        kind,
    })
}
