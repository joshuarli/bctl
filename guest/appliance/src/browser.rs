use std::fs::{self, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use crate::layout;

pub fn exec() -> Result<ExitCode, String> {
    let executable = fs::canonicalize(layout::BROWSER_LINK).map_err(|e| format!("{}: {e}", layout::BROWSER_LINK))?;
    let root = executable.parent().ok_or_else(|| format!("{} has no parent directory", executable.display()))?;
    let log_path = layout::log("browser.log");
    let log = OpenOptions::new().append(true).open(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let stderr = log.try_clone().map_err(|e| format!("{}: {e}", log_path.display()))?;
    let error = Command::new(&executable)
        .args(arguments(&layout::profile_dir(), &layout::start_url()))
        .envs(archive_environment(root))
        .stdout(log)
        .stderr(stderr)
        .exec();
    Err(format!("exec {}: {error}", executable.display()))
}

fn arguments(profile: &Path, start_url: &str) -> Vec<String> {
    vec![
        "--ozone-platform=wayland".into(),
        format!("--remote-debugging-port={}", layout::BROWSER_CDP_PORT),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--start-maximized".into(),
        start_url.into(),
    ]
}

fn archive_environment(root: &Path) -> [(&'static str, String); 3] {
    let fontconfig = root.join("fontconfig");
    [
        ("LD_LIBRARY_PATH", root.join("lib").display().to_string()),
        ("FONTCONFIG_FILE", fontconfig.join("fonts.conf").display().to_string()),
        ("FONTCONFIG_PATH", fontconfig.display().to_string()),
    ]
}
