#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use bctl_runtime_nanos::{Backend, LaunchSpec, Nanos, RuntimeError, Toolchain};

struct Harness {
    dir: PathBuf,
    runtime: Nanos,
    spec: LaunchSpec,
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Harness {
    fn new(name: &str, runner: &str) -> Self {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let dir = repo.join(".work/tmp").join(format!("{name}-{}", std::process::id()));
        fs::create_dir_all(dir.join("machines")).unwrap();
        let supervisor = dir.join("supervisor");
        script(&supervisor, &format!("exec '{}' \"$@\" --start-timeout-ms 1500", env!("CARGO_BIN_EXE_bctl-nanos-vm")));
        let runner_path = dir.join("runner");
        script(&runner_path, &format!(
            "trap '' TERM\necho $$ > '{dir}/runner.pid'\necho $PPID > '{dir}/supervisor.pid'\n{runner}\nwhile :; do sleep 0.05; done",
            dir = dir.display(),
        ));
        let image = dir.join("image.img");
        fs::write(&image, b"image").unwrap();
        fs::write(image.with_extension("profile.img"), b"profile").unwrap();
        let runtime = Nanos::new(dir.join("machines"), Toolchain {
            supervisor, supervisor_args: Vec::new(),
            backend: Backend::Libkrun { runner: runner_path, kernel: dir.join("kernel") },
        });
        let spec = LaunchSpec {
            machine: "m".into(), image, profile_dir: dir.join("profile"), files_dir: dir.join("files"),
            cdp_socket: dir.join("cdp"), memory_mib: 64, cpus: 1, allow_private: Vec::new(),
        };
        Self { dir, runtime, spec }
    }

    fn pid(&self, name: &str) -> u32 {
        fs::read_to_string(self.dir.join(format!("{name}.pid"))).unwrap().trim().parse().unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Ok(group) = fs::read_to_string(self.dir.join("supervisor.pid")) {
            let _ = Command::new("/bin/kill").args(["-s", "KILL", "--", &format!("-{}", group.trim())])
                .stderr(Stdio::null()).status();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/kill").args(["-s", "0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success()
}

fn check_failed_start(harness: Harness) {
    let started = Instant::now();
    let error = harness.runtime.launch(&harness.spec).unwrap_err();
    let elapsed = started.elapsed();
    let runner = harness.pid("runner");
    let supervisor = harness.pid("supervisor");
    let deadline = Instant::now() + Duration::from_secs(3);
    while (alive(runner) || alive(supervisor)) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(matches!(error, RuntimeError::Supervisor { operation: "launch", .. }), "{error}");
    assert!(!alive(runner), "runner {runner} survived the failed launch: {error}");
    assert!(!alive(supervisor), "supervisor {supervisor} survived the failed launch: {error}");
    assert!(!harness.runtime.machine_dir("m").exists());
    assert!(elapsed < Duration::from_secs(20), "failed launch took {elapsed:?}");
}

#[test]
fn a_runner_that_ignores_term_does_not_outlive_a_startup_timeout() {
    check_failed_start(Harness::new("nanos-mac-timeout", ""));
}

#[test]
fn a_runner_does_not_outlive_a_supervisor_that_exits_before_readiness() {
    check_failed_start(Harness::new("nanos-mac-exits", "kill -TERM $PPID"));
}
