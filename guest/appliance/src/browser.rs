use std::fs::{self, OpenOptions};
use std::io::{self, Write};
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

const BUNDLED_UBLOCK_ORIGIN: &str = "blockjmkbacgjkknlgpkjjiijinjdanf";
const DISABLE_REASON_USER_ACTION: u8 = 1;

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
    disable_bundled_ublock_origin(&layout::profile_dir())?;
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
    if display == Display::Headless {
        arguments.push(format!("--proxy-server=socks5://127.0.0.1:{}", layout::EGRESS_PROXY_PORT));
    }
    arguments.extend([
        format!("--remote-debugging-port={}", layout::BROWSER_CDP_PORT),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        start_url.into(),
    ]);
    arguments
}

/// Helium bundles uBlock Origin, whose first-run filter-list compile costs seconds of CPU and holds every
/// request until it finishes. A profile the browser has never opened gets the extension pre-disabled, as if the
/// user had turned it off; a profile the browser already wrote keeps its own choice.
fn disable_bundled_ublock_origin(profile: &Path) -> Result<(), String> {
    let default = profile.join("Default");
    fs::create_dir_all(&default).map_err(|e| format!("{}: {e}", default.display()))?;
    let path = default.join("Preferences");
    let mut preferences = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    write!(preferences, r#"{{"extensions":{{"settings":{{"{BUNDLED_UBLOCK_ORIGIN}":{{"disable_reasons":[{DISABLE_REASON_USER_ACTION}]}}}}}}}}"#)
        .map_err(|e| format!("{}: {e}", path.display()))
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
        assert!(args.contains(&format!("--proxy-server=socks5://127.0.0.1:{}", layout::EGRESS_PROXY_PORT)));
    }

    #[test]
    fn a_new_profile_starts_with_bundled_ublock_origin_disabled() {
        let profile = scratch("new-profile");
        disable_bundled_ublock_origin(&profile).unwrap();
        let written = fs::read_to_string(profile.join("Default/Preferences")).unwrap();
        assert_eq!(written, r#"{"extensions":{"settings":{"blockjmkbacgjkknlgpkjjiijinjdanf":{"disable_reasons":[1]}}}}"#);
    }

    #[test]
    fn a_profile_the_browser_already_wrote_keeps_its_preferences() {
        let profile = scratch("used-profile");
        fs::create_dir_all(profile.join("Default")).unwrap();
        fs::write(profile.join("Default/Preferences"), "{}").unwrap();
        disable_bundled_ublock_origin(&profile).unwrap();
        assert_eq!(fs::read_to_string(profile.join("Default/Preferences")).unwrap(), "{}");
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("browser-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn wayland_keeps_the_headed_surface() {
        let args = arguments(Display::Wayland, Path::new("/profile"), "about:blank");
        assert!(args.contains(&"--ozone-platform=wayland".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--headless")));
    }
}
