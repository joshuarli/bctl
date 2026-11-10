use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

use bctl_core::{Endpoints, Generation, MachineName, MachineState, SessionName};
use bctl_runtime::InputSupport;
use bctl_runtime_smolvm::{LaunchSource, LaunchSpec, RuntimeError, SmolVm};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn cdp_socket(session: &SessionName, generation: u64) -> PathBuf {
    let dir = workspace().join(format!(".work/tmp/cdp-{session}-{generation}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("s")
}

fn spec(session: &SessionName, generation: u64) -> LaunchSpec {
    LaunchSpec {
        machine: MachineName { session: session.clone(), generation: Generation(generation) },
        source: LaunchSource::Image(workspace().join(".work/images/ref-alpine-chromium.tar")),
        profile_dir: None,
        endpoints: Endpoints { vnc_port: free_port() },
        cdp_socket: cdp_socket(session, generation),
        display: (1280, 800),
        memory_mib: 4096,
        cpus: 2,
        allow_private: Vec::new(),
    }
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn a_session_starts_becomes_ready_stops_and_restarts_under_a_new_generation() {
    let runtime = SmolVm::new(workspace().join(".work/runtime/smolvm/smolvm"));
    let session = SessionName::new(&format!("lifecycle-{}", std::process::id())).unwrap();

    for generation in [1, 2] {
        let spec = spec(&session, generation);
        runtime.launch(&spec).unwrap();
        let ready = runtime.wait_ready(&spec.machine, spec.endpoints, &spec.cdp_socket, Duration::from_secs(120), |_| {});
        let listed = runtime.list().unwrap();
        runtime.remove(&spec.machine).unwrap();

        let ready = ready.unwrap();
        assert_eq!(ready.input, InputSupport::Interactive);
        assert!(ready.browser.starts_with("Chrome/"), "{}", ready.browser);
        assert!(listed.iter().any(|m| m.name == spec.machine.to_string() && m.state == MachineState::Running));
        assert!(!runtime.list().unwrap().iter().any(|m| m.name == spec.machine.to_string()));
    }
}

#[test]
#[ignore = "needs runtime/build.sh"]
fn removing_a_machine_that_does_not_exist_is_not_an_error() {
    let runtime = SmolVm::new(workspace().join(".work/runtime/smolvm/smolvm"));
    let machine = MachineName::parse("bctl-never-started-1").unwrap();
    assert_eq!(runtime.remove(&machine), Ok(()));
}

#[test]
#[ignore = "needs runtime/build.sh"]
fn a_failed_launch_reports_smolvms_error_code() {
    let runtime = SmolVm::new(workspace().join(".work/runtime/smolvm/smolvm"));
    let mut spec = spec(&SessionName::new("missing-image").unwrap(), 1);
    spec.source = LaunchSource::Image(workspace().join(".work/images/does-not-exist.tar"));
    match runtime.launch(&spec) {
        Err(RuntimeError::Smolvm { operation, code, .. }) => {
            assert_eq!(operation, "machine create");
            assert_ne!(code, "UNKNOWN");
        }
        other => panic!("expected a smolvm error, got {other:?}"),
    }
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn a_paused_session_resumes_ready_and_a_paused_machine_can_be_removed() {
    let runtime = SmolVm::new(workspace().join(".work/runtime/smolvm/smolvm"));
    let session = SessionName::new(&format!("pause-{}", std::process::id())).unwrap();
    let spec = spec(&session, 1);
    let name = spec.machine.to_string();
    let running = |runtime: &SmolVm| runtime.list().unwrap().into_iter().find(|m| m.name == name).map(|m| m.state);
    runtime.launch(&spec).unwrap();
    let outcome = (|| {
        runtime.wait_ready(&spec.machine, spec.endpoints, &spec.cdp_socket, Duration::from_secs(120), |_| {})?;
        runtime.pause(&spec.machine)?;
        let paused = running(&runtime);
        runtime.resume(&spec)?;
        let resumed = runtime.wait_ready(&spec.machine, spec.endpoints, &spec.cdp_socket, Duration::from_secs(30), |_| {})?;
        runtime.pause(&spec.machine)?;
        Ok::<_, RuntimeError>((paused, resumed))
    })();
    let removed = runtime.remove(&spec.machine);

    let (paused, resumed) = outcome.unwrap();
    assert_eq!(paused, Some(MachineState::Paused));
    assert_eq!(resumed.input, InputSupport::Interactive);
    assert_eq!(removed, Ok(()));
    assert_eq!(running(&runtime), None);
}
