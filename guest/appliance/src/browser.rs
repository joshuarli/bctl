use std::fs::{self, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use crate::layout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Wayland,
    Headless,
}

pub const DISPLAY_ENV: &str = "BCTL_BROWSER_DISPLAY";

impl Display {
    pub fn as_str(self) -> &'static str {
        match self {
            Display::Wayland => "wayland",
            Display::Headless => "headless",
        }
    }

    fn from_env() -> Result<Display, String> {
        match std::env::var(DISPLAY_ENV).as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("wayland") => Ok(Display::Wayland),
            Ok("headless") => Ok(Display::Headless),
            Ok(other) => Err(format!("{DISPLAY_ENV} {other:?} is neither wayland nor headless")),
            Err(std::env::VarError::NotUnicode(value)) => Err(format!("{DISPLAY_ENV} {value:?} is not unicode")),
        }
    }
}

pub fn exec() -> Result<ExitCode, String> {
    let display = Display::from_env()?;
    let executable = fs::canonicalize(layout::BROWSER_LINK).map_err(|e| format!("{}: {e}", layout::BROWSER_LINK))?;
    let root = executable.parent().ok_or_else(|| format!("{} has no parent directory", executable.display()))?;
    let log_path = layout::log("browser.log");
    let log = OpenOptions::new().append(true).open(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let stderr = log.try_clone().map_err(|e| format!("{}: {e}", log_path.display()))?;
    let error = Command::new(&executable)
        .args(arguments(display, &layout::profile_dir(), &layout::start_url()))
        .envs(archive_environment(root))
        .stdout(log)
        .stderr(stderr)
        .exec();
    Err(format!("exec {}: {error}", executable.display()))
}

fn arguments(display: Display, profile: &Path, start_url: &str) -> Vec<String> {
    let surface: [&str; 2] = match display {
        Display::Wayland => ["--ozone-platform=wayland", "--start-maximized"],
        Display::Headless => ["--headless=new", "--disable-gpu"],
    };
    let mut arguments: Vec<String> = surface.iter().map(|a| a.to_string()).collect();
    arguments.extend([
        format!("--remote-debugging-port={}", layout::BROWSER_CDP_PORT),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        start_url.into(),
    ]);
    arguments
}

fn archive_environment(root: &Path) -> [(&'static str, String); 3] {
    let fontconfig = root.join("fontconfig");
    [
        ("LD_LIBRARY_PATH", root.join("lib").display().to_string()),
        ("FONTCONFIG_FILE", fontconfig.join("fonts.conf").display().to_string()),
        ("FONTCONFIG_PATH", fontconfig.display().to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_drops_the_wayland_surface_and_keeps_cdp_and_the_profile() {
        let args = arguments(Display::Headless, Path::new("/profile"), "about:blank");
        assert!(args.contains(&"--headless=new".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--ozone-platform")));
        assert!(args.contains(&format!("--remote-debugging-port={}", layout::BROWSER_CDP_PORT)));
        assert!(args.contains(&"--user-data-dir=/profile".to_string()));
        assert!(!args.iter().any(|a| a == "--no-sandbox" || a == "--single-process" || a == "--no-zygote"));
    }

    #[test]
    fn wayland_keeps_the_headed_surface() {
        let args = arguments(Display::Wayland, Path::new("/profile"), "about:blank");
        assert!(args.contains(&"--ozone-platform=wayland".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--headless")));
    }
}
