use std::collections::BTreeMap;

use bctl_core::{ProfileName, SessionName};
use serde::Deserialize;

use crate::BrowserName;
use crate::config::{BrowserSettings, Config, ExecJob, GrabJob, Job, JobKind};
use crate::error::ConfigError;
use crate::value::{duration, invalid, relative_path, url};

const SUPPORTED_VERSION: i64 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    version: i64,
    #[serde(default)]
    browsers: BTreeMap<String, RawBrowser>,
    #[serde(default)]
    jobs: BTreeMap<String, RawJob>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBrowser {
    image: String,
    viewport: Option<[u32; 2]>,
    cpus: Option<u16>,
    memory_mib: Option<u32>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum RawJob {
    Grab(RawGrab),
    Exec(RawExec),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrab {
    browser: String,
    profile: Option<String>,
    urls: Vec<String>,
    max_pages: Option<u32>,
    jobs: Option<u16>,
    robots: Option<bool>,
    max_runtime: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExec {
    browser: String,
    profile: Option<String>,
    argv: Vec<String>,
    cwd: Option<String>,
    timeout: Option<String>,
}

impl RawConfig {
    pub(crate) fn into_config(self) -> Result<Config, ConfigError> {
        if self.version != SUPPORTED_VERSION {
            return Err(ConfigError::UnsupportedVersion { found: self.version });
        }
        let mut browsers = BTreeMap::new();
        for (key, raw) in self.browsers {
            let name = browser_name(&key)?;
            let settings = raw.into_settings(&name)?;
            browsers.insert(name, settings);
        }
        let mut jobs = BTreeMap::new();
        for (key, raw) in self.jobs {
            let name = job_name(&key)?;
            let job = raw.into_job(&name, &browsers)?;
            jobs.insert(name, job);
        }
        Ok(Config { browsers, jobs })
    }
}

fn browser_name(key: &str) -> Result<BrowserName, ConfigError> {
    BrowserName::new(key).map_err(|source| ConfigError::InvalidName {
        table: format!("browsers.{key}"),
        source,
    })
}

fn job_name(key: &str) -> Result<SessionName, ConfigError> {
    SessionName::new(key).map_err(|source| ConfigError::InvalidName {
        table: format!("jobs.{key}"),
        source,
    })
}

fn profile_name(job: &SessionName, raw: Option<&str>) -> Result<ProfileName, ConfigError> {
    let text = raw.unwrap_or(job.as_str());
    ProfileName::new(text).map_err(|source| ConfigError::InvalidName {
        table: format!("jobs.{job}.profile"),
        source,
    })
}

fn referenced_browser(job: &SessionName, browser: &str, browsers: &BTreeMap<BrowserName, BrowserSettings>) -> Result<BrowserName, ConfigError> {
    BrowserName::new(browser)
        .ok()
        .filter(|name| browsers.contains_key(name))
        .ok_or_else(|| ConfigError::UnknownBrowser {
            job: job.to_string(),
            browser: browser.to_string(),
        })
}

impl RawBrowser {
    fn into_settings(self, name: &BrowserName) -> Result<BrowserSettings, ConfigError> {
        let base = format!("browsers.{name}");
        if self.image.is_empty() {
            return Err(invalid(&format!("{base}.image"), "must not be empty"));
        }
        let viewport = self.viewport.unwrap_or([1280, 800]);
        for (axis, edge) in ["width", "height"].into_iter().zip(viewport) {
            if !(320..=7680).contains(&edge) {
                return Err(invalid(&format!("{base}.viewport"), &format!("{axis} must be between 320 and 7680")));
            }
        }
        let cpus = self.cpus.unwrap_or(2);
        if !(1..=16).contains(&cpus) {
            return Err(invalid(&format!("{base}.cpus"), "must be between 1 and 16"));
        }
        let memory_mib = self.memory_mib.unwrap_or(4096);
        if !(1024..=65536).contains(&memory_mib) {
            return Err(invalid(&format!("{base}.memory_mib"), "must be between 1024 and 65536"));
        }
        Ok(BrowserSettings { image: self.image, viewport, cpus, memory_mib })
    }
}

impl RawJob {
    fn into_job(self, name: &SessionName, browsers: &BTreeMap<BrowserName, BrowserSettings>) -> Result<Job, ConfigError> {
        match self {
            RawJob::Grab(grab) => {
                let browser = referenced_browser(name, &grab.browser, browsers)?;
                let profile = profile_name(name, grab.profile.as_deref())?;
                let kind = JobKind::Grab(grab.into_grab(name)?);
                Ok(Job { name: name.clone(), browser, profile, kind })
            }
            RawJob::Exec(exec) => {
                let browser = referenced_browser(name, &exec.browser, browsers)?;
                let profile = profile_name(name, exec.profile.as_deref())?;
                let kind = JobKind::Exec(exec.into_exec(name)?);
                Ok(Job { name: name.clone(), browser, profile, kind })
            }
        }
    }
}

impl RawGrab {
    fn into_grab(self, name: &SessionName) -> Result<GrabJob, ConfigError> {
        let base = format!("jobs.{name}");
        if self.urls.is_empty() {
            return Err(invalid(&format!("{base}.urls"), "must not be empty"));
        }
        for (index, url_text) in self.urls.iter().enumerate() {
            url(&format!("{base}.urls[{index}]"), url_text)?;
        }
        if let Some(jobs) = self.jobs
            && !(1..=64).contains(&jobs)
        {
            return Err(invalid(&format!("{base}.jobs"), "must be between 1 and 64"));
        }
        let max_runtime = self
            .max_runtime
            .map(|text| duration(&format!("{base}.max_runtime"), &text))
            .transpose()?;
        Ok(GrabJob { urls: self.urls, max_pages: self.max_pages, jobs: self.jobs, robots: self.robots, max_runtime })
    }
}

impl RawExec {
    fn into_exec(self, name: &SessionName) -> Result<ExecJob, ConfigError> {
        let base = format!("jobs.{name}");
        if self.argv.is_empty() {
            return Err(invalid(&format!("{base}.argv"), "must not be empty"));
        }
        if self.argv[0].is_empty() {
            return Err(invalid(&format!("{base}.argv[0]"), "must not be empty"));
        }
        let cwd = self
            .cwd
            .as_deref()
            .map(|text| relative_path(&format!("{base}.cwd"), text))
            .transpose()?;
        let timeout = self
            .timeout
            .map(|text| duration(&format!("{base}.timeout"), &text))
            .transpose()?;
        Ok(ExecJob { argv: self.argv, cwd, timeout })
    }
}
