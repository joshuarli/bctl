use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::report::{Host, ImageIdentity};

pub type Fallible<T> = Result<T, String>;

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root exists")
}

pub fn output_of(program: &str, args: &[&str]) -> Fallible<String> {
    let output = Command::new(program).args(args).output().map_err(|e| format!("cannot run {program}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{program} {args:?} failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn host() -> Fallible<Host> {
    let sysctl = |key: &str| output_of("sysctl", &["-n", key]);
    Ok(Host {
        model: sysctl("hw.model")?,
        chip: sysctl("machdep.cpu.brand_string")?,
        cpus: sysctl("hw.ncpu")?.parse().map_err(|_| "hw.ncpu is not a number")?,
        memory_gib: sysctl("hw.memsize")?.parse::<u64>().map_err(|_| "hw.memsize is not a number")? >> 30,
        macos: output_of("sw_vers", &["-productVersion"])? + " (" + &output_of("sw_vers", &["-buildVersion"])? + ")",
        load_average_start: sysctl("vm.loadavg")?,
    })
}

pub fn commit() -> Fallible<(String, bool)> {
    let root = root();
    let git = |args: &[&str]| {
        let mut full = vec!["-C", root.to_str().expect("utf-8 root")];
        full.extend_from_slice(args);
        output_of("git", &full)
    };
    Ok((git(&["rev-parse", "HEAD"])?, !git(&["status", "--porcelain", "--untracked-files=no"])?.is_empty()))
}

pub fn utc_now() -> Fallible<String> {
    output_of("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])
}

pub fn smolvm_version(smolvm: &Path) -> Fallible<String> {
    output_of(smolvm.to_str().expect("utf-8 path"), &["--version"])
}

pub fn image_identity(name: &str) -> Fallible<ImageIdentity> {
    let tar = root().join(".work/images").join(format!("{name}.tar"));
    let bytes = std::fs::metadata(&tar).map_err(|e| format!("{}: {e}", tar.display()))?.len();
    let manifest = output_of("tar", &["-xOf", tar.to_str().expect("utf-8 path"), "manifest.json"])?;
    let digest = manifest
        .split("blobs/sha256/")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .ok_or("image manifest names no config blob")?;
    Ok(ImageIdentity { name: name.into(), bytes, config_digest: digest.into() })
}

pub struct QuietGate {
    budget: Duration,
    waited: Cell<Duration>,
    gave_up: Cell<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    OverlappedLoad,
    HostBusy,
}

impl QuietGate {
    pub fn new(budget: Duration) -> QuietGate {
        QuietGate { budget, waited: Cell::new(Duration::ZERO), gave_up: Cell::new(false) }
    }

    fn ready(args: &[&str]) -> bool {
        Command::new("perf-gate-ready").args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }

    pub fn wait_quiet(&self) -> bool {
        let started = Instant::now();
        let mut pause = Duration::from_secs(5);
        loop {
            if Self::ready(&[]) {
                self.waited.set(self.waited.get() + started.elapsed());
                self.gave_up.set(false);
                return true;
            }
            if self.waited.get() + started.elapsed() >= self.budget {
                self.waited.set(self.waited.get() + started.elapsed());
                self.gave_up.set(true);
                return false;
            }
            thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_secs(60));
        }
    }

    pub fn run<T>(&self, action: impl FnOnce() -> Fallible<T>) -> Fallible<(T, Duration, Verdict)> {
        let quiet_before = self.wait_quiet();
        let started = Instant::now();
        let value = action()?;
        let elapsed = started.elapsed();
        let verdict = match (quiet_before, Self::ready(&["-1"])) {
            (false, _) => Verdict::HostBusy,
            (true, false) => Verdict::OverlappedLoad,
            (true, true) => Verdict::Clean,
        };
        Ok((value, elapsed, verdict))
    }

    pub fn gave_up(&self) -> bool {
        self.gave_up.get()
    }

    pub fn waited(&self) -> Duration {
        self.waited.get()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessStats {
    pub rss_kib: u64,
    pub cpu_seconds: f64,
}

pub fn process_stats(pid: u32) -> Fallible<ProcessStats> {
    let out = output_of("ps", &["-o", "rss=,time=", "-p", &pid.to_string()])?;
    let mut fields = out.split_whitespace();
    let rss_kib = fields.next().and_then(|v| v.parse().ok()).ok_or_else(|| format!("ps printed {out:?}"))?;
    let time = fields.next().ok_or_else(|| format!("ps printed {out:?}"))?;
    let mut cpu_seconds = 0.0;
    for part in time.split(':') {
        cpu_seconds = cpu_seconds * 60.0 + part.parse::<f64>().map_err(|_| format!("ps time {time:?}"))?;
    }
    Ok(ProcessStats { rss_kib, cpu_seconds })
}

pub fn pids_matching(needle: &str) -> Fallible<Vec<u32>> {
    let out = output_of("ps", &["-axo", "pid=,command="])?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(' ')?;
            command.contains(needle).then(|| pid.parse().ok()).flatten()
        })
        .collect())
}

pub fn footprint_mib(pid: u32) -> Fallible<f64> {
    let out = output_of("footprint", &["-p", &pid.to_string()])?;
    let rest = out.split("Footprint:").nth(1).ok_or_else(|| format!("footprint printed {out:?}"))?;
    let mut fields = rest.split_whitespace();
    let amount: f64 = fields.next().and_then(|v| v.parse().ok()).ok_or_else(|| format!("footprint printed {out:?}"))?;
    match fields.next() {
        Some("KB") => Ok(amount / 1024.0),
        Some("MB") => Ok(amount),
        Some("GB") => Ok(amount * 1024.0),
        other => Err(format!("footprint unit {other:?}")),
    }
}
