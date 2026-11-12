#![cfg(not(target_os = "macos"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use bctl_runtime_nanos::{Backend, LaunchSpec, Nanos, RuntimeError, Toolchain};

const MACHINE: &str = "m";

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
    /// `qemu` is the body of the fake QEMU; the vsock backend and virtiofsd fixtures publish their sockets, record
    /// whether the machine directory still exists while they run, and virtiofsd ignores TERM.
    fn new(name: &str, qemu: &str) -> Harness {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let dir = repo.join(".work/tmp").join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (bin, pids, cdp, files) = (dir.join("bin"), dir.join("pids"), dir.join("cdp"), dir.join("files"));
        for path in [&bin, &pids, &cdp, &files, &dir.join("machines")] {
            fs::create_dir_all(path).unwrap();
        }
        fs::set_permissions(&cdp, fs::Permissions::from_mode(0o700)).unwrap();
        let machine_dir = dir.join("machines").join(MACHINE);
        let record = format!(
            "echo $$ > '{p}'/$NAME.pid; echo $PPID > '{p}'/$NAME.ppid\n\
             beat() {{ if [ -d '{m}' ]; then printf present > '{p}'/$NAME.tmp; else printf gone > '{p}'/$NAME.tmp; fi; cat '{m}'/supervisor.lock > '{p}'/$NAME.lock 2>/dev/null; mv '{p}'/$NAME.tmp '{p}'/$NAME.last; }}",
            p = pids.display(),
            m = machine_dir.display(),
        );
        script(&bin.join("qemu-img"), "for last; do :; done\n: > \"$last\"");
        script(&bin.join("vhost-device-vsock"), &format!(
            "NAME=vhost\n{record}\nsock=${{2#*socket=}}; sock=${{sock%%,*}}; : > \"$sock\"\nwhile :; do beat; sleep 0.05; done"
        ));
        script(&bin.join("virtiofsd"), &format!(
            "NAME=virtiofsd\n{record}\ntrap '' TERM\n: > \"${{1#--socket-path=}}\"\nwhile :; do beat; sleep 0.05; done"
        ));
        let qemu = qemu.replace("@MACHINE@", &machine_dir.display().to_string()).replace("@TERM@", &pids.join("qemu.term").display().to_string());
        script(&bin.join("qemu"), &format!("NAME=qemu\n{record}\n{qemu}"));
        let supervisor = bin.join("supervisor");
        script(&supervisor, &format!("exec '{}' \"$@\" --start-timeout-ms 1500", env!("CARGO_BIN_EXE_bctl-nanos-vm")));
        fs::write(dir.join("image.img"), b"image").unwrap();
        let runtime = Nanos::new(dir.join("machines"), Toolchain {
            supervisor,
            supervisor_args: Vec::new(),
            backend: Backend::Qemu {
                qemu: bin.join("qemu"), qemu_img: bin.join("qemu-img"), vsock_backend: bin.join("vhost-device-vsock"),
                virtiofsd: bin.join("virtiofsd"),
            },
        });
        let spec = LaunchSpec {
            machine: MACHINE.into(), image: dir.join("image.img"), profile_dir: dir.join("profile"), files_dir: files,
            cdp_socket: cdp.join("s"), memory_mib: 64, cpus: 1, allow_private: Vec::new(),
        };
        Harness { dir, runtime, spec }
    }

    fn pids(&self, suffix: &str) -> Vec<(String, u32)> {
        let mut found = Vec::new();
        for entry in fs::read_dir(self.dir.join("pids")).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if let Some(stem) = name.strip_suffix(suffix) {
                let pid = fs::read_to_string(self.dir.join("pids").join(&name)).unwrap().trim().parse().unwrap();
                found.push((stem.to_string(), pid));
            }
        }
        found
    }

    fn last(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join("pids").join(format!("{name}.last"))).unwrap_or_default()
    }
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/kill").args(["-s", "0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success()
}

fn settle(pids: &[u32]) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let living: Vec<u32> = pids.iter().copied().filter(|&pid| alive(pid)).collect();
        if living.is_empty() || Instant::now() >= deadline {
            return living;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn reap_leftovers(pids: &[u32]) {
    for pid in pids {
        let _ = Command::new("/bin/kill").args(["-s", "KILL", &pid.to_string()]).status();
    }
}

#[test]
fn a_qemu_that_never_publishes_qmp_leaves_no_process_and_loses_its_directory_last() {
    let harness = Harness::new(
        "nanos-failed-start",
        "stopped() { sleep 1; if [ -d '@MACHINE@' ]; then printf present; else printf gone; fi > '@TERM@'; exit 0; }\n\
         trap stopped TERM\nwhile :; do beat; sleep 0.05; done",
    );
    let term_record = harness.dir.join("pids/qemu.term");

    let started = Instant::now();
    let error = harness.runtime.launch(&harness.spec).unwrap_err();
    let elapsed = started.elapsed();

    let names: Vec<String> = harness.pids(".pid").into_iter().map(|(name, _)| name).collect();
    let mut all: Vec<u32> = harness.pids(".pid").into_iter().chain(harness.pids(".ppid")).map(|(_, pid)| pid).collect();
    all.sort_unstable();
    all.dedup();
    let living = settle(&all);
    reap_leftovers(&living);
    let machine_dir = harness.runtime.machine_dir(MACHINE);
    let lock_holder = fs::read_to_string(harness.dir.join("pids/vhost.lock")).unwrap_or_default();
    let supervisors: Vec<String> = harness.pids(".ppid").into_iter().map(|(_, pid)| pid.to_string()).collect();
    let still_there = machine_dir.exists();
    let term_saw = fs::read_to_string(&term_record).unwrap_or_default();
    let beats: Vec<(String, String)> = ["qemu", "vhost", "virtiofsd"].iter().map(|n| (n.to_string(), harness.last(n))).collect();
    let dir = harness.dir.clone();
    let _ = fs::remove_dir_all(&dir);

    assert!(matches!(&error, RuntimeError::Supervisor { operation: "launch", message } if message.contains("did not report QEMU running")), "{error}");
    assert_eq!(names.len(), 3, "every fixture should have started: {names:?}");
    assert!(living.is_empty(), "processes outlived the failed launch: {living:?}");
    assert!(supervisors.iter().all(|pid| *pid == lock_holder.trim()), "the lock holder {lock_holder:?} is not the supervisor {supervisors:?}");
    assert!(!still_there, "the machine directory was not removed");
    assert_eq!(term_saw, "present", "qemu was stopped after its machine directory was removed");
    for (name, last) in beats {
        assert_eq!(last, "present", "{name} last saw the machine directory as {last:?}");
    }
    assert!(elapsed < Duration::from_secs(20), "the failed start took {elapsed:?}");
}

#[test]
fn a_qemu_that_exits_at_once_leaves_no_helper_running() {
    let harness = Harness::new("nanos-qemu-exits", "exit 1");
    let error = harness.runtime.launch(&harness.spec).unwrap_err();
    let all: Vec<u32> = harness.pids(".pid").into_iter().map(|(_, pid)| pid).collect();
    let living = settle(&all);
    reap_leftovers(&living);
    let still_there = harness.runtime.machine_dir(MACHINE).exists();
    let _ = fs::remove_dir_all(&harness.dir);
    assert!(matches!(error, RuntimeError::Supervisor { operation: "launch", .. }), "{error}");
    assert_eq!(all.len(), 3);
    assert!(living.is_empty(), "processes outlived the failed launch: {living:?}");
    assert!(!still_there);
}
