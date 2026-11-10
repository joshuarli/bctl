use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bctl_runtime::{InputSupport, cdp_socket_browser};
use bctl_runtime_nanos::{LaunchSpec, Nanos, RuntimeError, Toolchain};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn which(name: &str) -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| candidate.is_file()))
        .unwrap_or_else(|| panic!("{name} is not on PATH"))
}

fn nanos(root: &Path) -> Nanos {
    let runtime = repo().join(".work/runtime/nanos");
    let kernel = runtime.join("hydrogen/nanos/output/platform/pc/bin/kernel.img");
    assert!(kernel.is_file(), "run runtime/build-nanos.sh first");
    Nanos::new(
        root,
        Toolchain {
            supervisor: PathBuf::from(env!("CARGO_BIN_EXE_bctl-nanos-vm")),
            supervisor_args: Vec::new(),
            qemu: which("qemu-system-x86_64"),
            qemu_img: which("qemu-img"),
            kernel,
            vsock_backend: runtime.join("bin/vhost-device-vsock"),
        },
    )
}

fn image() -> PathBuf {
    let image = std::env::var_os("BCTL_NANOS_IMAGE").map(PathBuf::from).unwrap_or_else(|| repo().join(".work/images/nanos-helium.img"));
    assert!(image.is_file(), "build the image with images/nanos-helium/build.sh or set BCTL_NANOS_IMAGE");
    image
}

fn scratch(name: &str) -> PathBuf {
    let dir = repo().join(".work/tmp").join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("machines")).unwrap();
    fs::create_dir_all(dir.join("cdp")).unwrap();
    fs::set_permissions(dir.join("cdp"), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

#[test]
#[ignore = "boots Nanos VMs: needs /dev/kvm, QEMU, runtime/build-nanos.sh and the nanos-helium image"]
fn launch_ready_and_remove_ten_times() {
    let dir = scratch("nanos-lifecycle");
    let runtime = nanos(&dir.join("machines"));
    let image = image();
    let rounds: usize = std::env::var("BCTL_NANOS_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let mut ready_ms = Vec::new();
    for round in 0..rounds {
        let machine = format!("lifecycle-{round}");
        let cdp_socket = dir.join("cdp").join(format!("{round}.sock"));
        let spec = LaunchSpec { machine: machine.clone(), image: image.clone(), cdp_socket: cdp_socket.clone(), memory_mib: 2048, cpus: 2, allow_private: Vec::new() };
        let start = Instant::now();
        runtime.launch(&spec).unwrap_or_else(|e| panic!("round {round}: {e}"));
        let mut stages = Vec::new();
        let ready = runtime.wait_ready(&machine, &cdp_socket, Duration::from_secs(90), |stage| {
            if stages.last() != Some(&stage) {
                stages.push(stage);
            }
        });
        let ready = ready.unwrap_or_else(|e| panic!("round {round}: {e}; stages {stages:?}; logs:\n{}", runtime.logs(&machine, 40).unwrap()));
        ready_ms.push(start.elapsed().as_millis());
        assert!(ready.browser.starts_with("Helium/"), "round {round}: browser {}", ready.browser);
        assert_eq!(ready.input, InputSupport::Headless);
        assert_eq!(runtime.list().unwrap(), vec![machine.clone()]);
        runtime.remove(&machine).unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert!(runtime.list().unwrap().is_empty(), "round {round}: a machine is still listed after remove");
        assert!(!runtime.machine_dir(&machine).exists(), "round {round}: the machine directory outlived remove");
        assert!(cdp_socket_browser(&cdp_socket).is_none(), "round {round}: CDP still answers after remove");
    }
    ready_ms.sort_unstable();
    eprintln!("launch to ready, ms: {ready_ms:?}; median {}", ready_ms[ready_ms.len() / 2]);
}

#[test]
#[ignore = "boots a Nanos VM: needs /dev/kvm, QEMU, runtime/build-nanos.sh and the nanos-helium image"]
fn a_machine_name_cannot_be_launched_twice_and_remove_is_idempotent() {
    let dir = scratch("nanos-twice");
    let runtime = nanos(&dir.join("machines"));
    let cdp_socket = dir.join("cdp/s");
    let spec = LaunchSpec { machine: "twice".into(), image: image(), cdp_socket: cdp_socket.clone(), memory_mib: 2048, cpus: 2, allow_private: Vec::new() };
    runtime.launch(&spec).unwrap();
    assert!(matches!(runtime.launch(&spec), Err(RuntimeError::Supervisor { operation: "launch", .. })));
    runtime.wait_ready("twice", &cdp_socket, Duration::from_secs(90), |_| {}).unwrap();
    runtime.remove("twice").unwrap();
    runtime.remove("twice").unwrap();
}

#[test]
#[ignore = "boots a Nanos VM: needs /dev/kvm, QEMU, runtime/build-nanos.sh and the nanos-helium image"]
fn the_guest_check_answers_every_time() {
    let dir = scratch("nanos-check");
    let runtime = nanos(&dir.join("machines"));
    let cdp_socket = dir.join("cdp/s");
    let spec = LaunchSpec { machine: "check".into(), image: image(), cdp_socket: cdp_socket.clone(), memory_mib: 2048, cpus: 2, allow_private: Vec::new() };
    runtime.launch(&spec).unwrap();
    runtime.wait_ready("check", &cdp_socket, Duration::from_secs(90), |_| {}).unwrap();
    let mut failures = Vec::new();
    let mut slowest = Duration::ZERO;
    for round in 0..300 {
        let start = Instant::now();
        if let Err(error) = runtime.check("check") {
            failures.push(format!("{round}: {error}"));
        }
        slowest = slowest.max(start.elapsed());
    }
    let logs = runtime.logs("check", 20).unwrap();
    runtime.remove("check").unwrap();
    assert!(failures.is_empty(), "{} of 300 checks failed, slowest {slowest:?}: {failures:?}\n{logs}", failures.len());
    eprintln!("300 checks, slowest {slowest:?}");
}
