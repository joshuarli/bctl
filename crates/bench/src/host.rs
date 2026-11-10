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

#[cfg(not(target_os = "linux"))]
pub fn host() -> Fallible<Host> {
    let sysctl = |key: &str| output_of("sysctl", &["-n", key]);
    Ok(Host {
        model: sysctl("hw.model")?,
        chip: sysctl("machdep.cpu.brand_string")?,
        cpus: sysctl("hw.ncpu")?.parse().map_err(|_| "hw.ncpu is not a number")?,
        memory_gib: sysctl("hw.memsize")?.parse::<u64>().map_err(|_| "hw.memsize is not a number")? >> 30,
        os: output_of("sw_vers", &["-productVersion"])? + " (" + &output_of("sw_vers", &["-buildVersion"])? + ")",
        load_average_start: sysctl("vm.loadavg")?,
    })
}

#[cfg(target_os = "linux")]
pub fn host() -> Fallible<Host> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").map_err(|e| format!("/proc/cpuinfo: {e}"))?;
    let chip = cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name")?.split_once(':').map(|(_, value)| value.trim().to_string()))
        .ok_or("/proc/cpuinfo has no model name")?;
    let meminfo = std::fs::read_to_string("/proc/meminfo").map_err(|e| format!("/proc/meminfo: {e}"))?;
    let mem_kib: u64 = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:")?.split_whitespace().next()?.parse().ok())
        .ok_or("/proc/meminfo has no MemTotal")?;
    let os_release = std::fs::read_to_string("/etc/os-release").map_err(|e| format!("/etc/os-release: {e}"))?;
    let pretty = os_release
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME=").map(|value| value.trim_matches('"').to_string()))
        .ok_or("/etc/os-release has no PRETTY_NAME")?;
    Ok(Host {
        model: std::fs::read_to_string("/sys/devices/virtual/dmi/id/product_name").map(|value| value.trim().to_string()).unwrap_or_default(),
        chip,
        cpus: std::thread::available_parallelism().map_err(|e| e.to_string())?.get() as u32,
        memory_gib: mem_kib / (1024 * 1024),
        os: format!("{} ({})", pretty, output_of("uname", &["-r"])?),
        load_average_start: std::fs::read_to_string("/proc/loadavg").map_err(|e| format!("/proc/loadavg: {e}"))?,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    SmolVm,
    Nanos,
}

impl Runtime {
    pub fn current() -> Fallible<Runtime> {
        match std::env::var("BCTL_RUNTIME").as_deref() {
            Ok("smolvm") => Ok(Runtime::SmolVm),
            Ok("nanos") => Ok(Runtime::Nanos),
            Ok(other) => Err(format!("BCTL_RUNTIME {other:?} is neither smolvm nor nanos")),
            Err(_) if cfg!(target_os = "macos") => Ok(Runtime::SmolVm),
            Err(_) => Ok(Runtime::Nanos),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Runtime::SmolVm => "smolvm",
            Runtime::Nanos => "nanos",
        }
    }

    pub fn image(self) -> &'static str {
        match self {
            Runtime::SmolVm => "helium",
            Runtime::Nanos => "nanos-helium",
        }
    }

    pub fn version(self) -> Fallible<String> {
        match self {
            Runtime::SmolVm => output_of(root().join(".work/runtime/smolvm/smolvm").to_str().expect("utf-8 path"), &["--version"]),
            Runtime::Nanos if cfg!(target_os = "macos") => Ok("libkrun/HVF".into()),
            Runtime::Nanos => {
                let commit = std::fs::read_to_string(root().join(".work/runtime/nanos/hydrogen/COMMIT"))
                    .map_err(|e| format!(".work/runtime/nanos/hydrogen/COMMIT: {e}"))?;
                let qemu = output_of("qemu-system-x86_64", &["--version"])?;
                let qemu = qemu.lines().next().unwrap_or_default();
                Ok(format!("hydrogen {}, {qemu}", commit.trim()))
            }
        }
    }

    pub fn image_identity(self, name: &str) -> Fallible<ImageIdentity> {
        match self {
            Runtime::SmolVm => {
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
            Runtime::Nanos => {
                let image = root().join(format!(".work/images/{name}.img"));
                let bytes = std::fs::metadata(&image).map_err(|e| format!("{}: {e}", image.display()))?.len();
                let path = image.to_str().expect("utf-8 path");
                let sum = if cfg!(target_os = "macos") { output_of("shasum", &["-a", "256", path])? } else { output_of("sha256sum", &[path])? };
                let digest = sum.split_whitespace().next().ok_or_else(|| format!("sha256sum printed {sum:?}"))?;
                Ok(ImageIdentity { name: name.into(), bytes, config_digest: format!("sha256:{digest}") })
            }
        }
    }
}

pub struct QuietGate {
    budget: Duration,
    waited: Cell<Duration>,
    gave_up: Cell<bool>,
}

#[cfg(test)]
mod quiet_gate_tests {
    use super::*;

    #[test]
    fn busy_host_does_not_start_a_measurement() {
        let started = Cell::new(false);
        let checked_after = Cell::new(false);
        let result = measure_when_quiet(false, || { checked_after.set(true); true }, || {
            started.set(true);
            Ok(())
        }).unwrap();
        assert!(result.is_none());
        assert!(!started.get());
        assert!(!checked_after.get());
    }

    #[test]
    fn a_failed_measurement_still_checks_for_overlapping_load() {
        let checked_after = Cell::new(false);
        let result: Fallible<Option<((), Duration, Verdict)>> = measure_when_quiet(true, || {
            checked_after.set(true);
            true
        }, || Err("measurement failed".into()));
        assert!(result.is_err());
        assert!(checked_after.get());
    }

    #[test]
    fn load_after_a_measurement_marks_it_discarded() {
        let result = measure_when_quiet(true, || false, || Ok(42)).unwrap().unwrap();
        assert_eq!(result.0, 42);
        assert_eq!(result.2, Verdict::OverlappedLoad);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    OverlappedLoad,
}

fn measure_when_quiet<T>(quiet: bool, check_after: impl FnOnce() -> bool, action: impl FnOnce() -> Fallible<T>) -> Fallible<Option<(T, Duration, Verdict)>> {
    if !quiet {
        return Ok(None);
    }
    let started = Instant::now();
    let value = action();
    let elapsed = started.elapsed();
    let verdict = if check_after() { Verdict::Clean } else { Verdict::OverlappedLoad };
    Ok(Some((value?, elapsed, verdict)))
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

    pub fn run<T>(&self, action: impl FnOnce() -> Fallible<T>) -> Fallible<Option<(T, Duration, Verdict)>> {
        measure_when_quiet(self.wait_quiet(), || Self::ready(&["-1"]), action)
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

#[cfg(not(target_os = "linux"))]
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

#[cfg(target_os = "linux")]
pub fn process_stats(pid: u32) -> Fallible<ProcessStats> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).map_err(|e| format!("/proc/{pid}/status: {e}"))?;
    let rss_kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:")?.split_whitespace().next()?.parse().ok())
        .ok_or_else(|| format!("/proc/{pid}/status has no VmRSS"))?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| format!("/proc/{pid}/stat: {e}"))?;
    let fields: Vec<&str> = stat.rsplit_once(')').ok_or_else(|| format!("/proc/{pid}/stat has no command"))?.1.split_whitespace().collect();
    let utime: u64 = fields.get(11).and_then(|v| v.parse().ok()).ok_or_else(|| format!("/proc/{pid}/stat has no utime"))?;
    let stime: u64 = fields.get(12).and_then(|v| v.parse().ok()).ok_or_else(|| format!("/proc/{pid}/stat has no stime"))?;
    Ok(ProcessStats { rss_kib, cpu_seconds: (utime + stime) as f64 / 100.0 })
}

#[cfg(not(target_os = "linux"))]
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

#[cfg(target_os = "linux")]
pub fn pids_matching(needle: &str) -> Fallible<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(|e| format!("/proc: {e}"))?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else { continue };
        let command: Vec<String> = cmdline.split(|byte| *byte == 0).map(|arg| String::from_utf8_lossy(arg).into_owned()).collect();
        if command.join(" ").contains(needle) {
            pids.push(pid);
        }
    }
    Ok(pids)
}

#[cfg(not(target_os = "linux"))]
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

#[cfg(target_os = "linux")]
pub fn footprint_mib(pid: u32) -> Fallible<f64> {
    let path = format!("/proc/{pid}/smaps_rollup");
    let rollup = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let pss: u64 = rollup
        .lines()
        .find_map(|line| line.strip_prefix("Pss:")?.split_whitespace().next()?.parse().ok())
        .ok_or_else(|| format!("{path} has no Pss"))?;
    Ok(pss as f64 / 1024.0)
}
