use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

use bctl_core::{Endpoints, Generation, MachineName, SessionName};
use bctl_runtime_smolvm::{InputSupport, LaunchSpec, RuntimeError, SmolVm};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn spec(session: &SessionName, generation: u64) -> LaunchSpec {
    LaunchSpec {
        machine: MachineName { session: session.clone(), generation: Generation(generation) },
        image: workspace().join(".work/images/ref-alpine-chromium.tar"),
        endpoints: Endpoints { cdp_port: free_port(), vnc_port: free_port() },
        display: (1280, 800),
        memory_mib: 4096,
        cpus: 2,
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
        let ready = runtime.wait_ready(&spec.machine, spec.endpoints, Duration::from_secs(120));
        let listed = runtime.list().unwrap();
        runtime.remove(&spec.machine).unwrap();

        let ready = ready.unwrap();
        assert_eq!(ready.input, InputSupport::Interactive);
        assert!(ready.browser.starts_with("Chrome/"), "{}", ready.browser);
        assert!(listed.iter().any(|m| m.name == spec.machine.to_string() && m.running));
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
    spec.image = workspace().join(".work/images/does-not-exist.tar");
    match runtime.launch(&spec) {
        Err(RuntimeError::Smolvm { operation, code, .. }) => {
            assert_eq!(operation, "machine create");
            assert_ne!(code, "UNKNOWN");
        }
        other => panic!("expected a smolvm error, got {other:?}"),
    }
}
