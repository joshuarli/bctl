use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use bctl_core::{Generation, MachineName, SessionName};
use bctl_runtime_smolvm::{GRACEFUL_STOP_SECS, Removal, RuntimeError, SmolVm};

const FAKE_SMOLVM: &str = r#"#!/bin/sh
ctl="$(dirname "$0")/ctl"
echo "$@" >> "$ctl/calls"
case "$1 $2" in
"machine ls")
    if [ -e "$ctl/paused" ]; then echo '[{"name":"bctl-a-1","state":"paused"}]'; else echo '[{"name":"bctl-a-1","state":"running"}]'; fi
    ;;
"machine stop")
    if [ -e "$ctl/stop-busy" ]; then
        echo '{"error":{"code":"BUSY","message":"guest did not confirm filesystem synchronization"}}' >&2
        exit 1
    fi
    if [ -e "$ctl/stop-gone" ]; then
        echo '{"error":{"code":"NOT_FOUND","message":"no such machine"}}' >&2
        exit 1
    fi
    ;;
"machine delete")
    if [ -e "$ctl/delete-busy" ]; then
        echo '{"error":{"code":"BUSY","message":"the VM would not stop"}}' >&2
        exit 1
    fi
    ;;
esac
exit 0
"#;

static SERIAL: Mutex<()> = Mutex::new(());

struct Fixture {
    runtime: SmolVm,
    dir: PathBuf,
    _serial: MutexGuard<'static, ()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn new(flags: &[&str]) -> Fixture {
        let serial = SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join(format!("bctl-graceful-remove-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("ctl")).unwrap();
        let binary = dir.join("smolvm");
        fs::write(&binary, FAKE_SMOLVM).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        for flag in flags {
            fs::write(dir.join("ctl").join(flag), b"").unwrap();
        }
        Fixture { runtime: SmolVm::new(binary), dir, _serial: serial }
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir.join("ctl/calls")).unwrap_or_default().lines().map(str::to_string).collect()
    }
}

fn machine() -> MachineName {
    MachineName { session: SessionName::new("a").unwrap(), generation: Generation(1) }
}

fn stop_call() -> String {
    format!("machine stop --name bctl-a-1 --timeout {GRACEFUL_STOP_SECS} --error-format json")
}

const _: () = assert!(GRACEFUL_STOP_SECS > 15 + 5 + 5, "the stop budget must exceed the appliance's worst-case shutdown");

const DELETE_CALL: &str = "machine delete --name bctl-a-1 -f --error-format json";

#[test]
fn a_running_machine_is_stopped_with_the_guest_budget_before_it_is_deleted() {
    let fixture = Fixture::new(&[]);
    assert_eq!(fixture.runtime.stop_and_remove(&machine()), Ok(Removal::Stopped));
    assert_eq!(fixture.calls(), ["machine ls --json --error-format json", &stop_call(), DELETE_CALL]);
}

#[test]
fn a_guest_that_cannot_be_stopped_is_deleted_by_force_and_reported_as_forced() {
    let fixture = Fixture::new(&["stop-busy"]);
    let removal = fixture.runtime.stop_and_remove(&machine()).unwrap();
    let Removal::Forced { stop_failure: RuntimeError::Smolvm { operation, code, .. } } = removal else {
        panic!("expected a forced removal, got {removal:?}");
    };
    assert_eq!((operation, code.as_str()), ("machine stop", "BUSY"));
    assert_eq!(fixture.calls(), ["machine ls --json --error-format json", &stop_call(), DELETE_CALL]);
}

#[test]
fn a_machine_that_is_already_gone_is_not_a_forced_removal() {
    let fixture = Fixture::new(&["stop-gone"]);
    assert_eq!(fixture.runtime.stop_and_remove(&machine()), Ok(Removal::Stopped));
    assert_eq!(fixture.calls().last().map(String::as_str), Some(DELETE_CALL));
}

#[test]
fn a_paused_machine_has_nothing_to_stop_and_is_deleted_directly() {
    let fixture = Fixture::new(&["paused"]);
    assert_eq!(fixture.runtime.stop_and_remove(&machine()), Ok(Removal::Stopped));
    assert_eq!(fixture.calls(), ["machine ls --json --error-format json", DELETE_CALL]);
}

#[test]
fn a_deletion_that_fails_is_an_error_whether_or_not_the_stop_succeeded() {
    for flags in [&["delete-busy"][..], &["stop-busy", "delete-busy"][..]] {
        let fixture = Fixture::new(flags);
        let error = fixture.runtime.stop_and_remove(&machine()).unwrap_err();
        assert!(matches!(&error, RuntimeError::Smolvm { operation: "machine delete", code, .. } if code == "BUSY"), "{error:?}");
    }
}

#[test]
fn plain_removal_stays_an_immediate_forced_delete() {
    let fixture = Fixture::new(&[]);
    fixture.runtime.remove(&machine()).unwrap();
    assert_eq!(fixture.calls(), [DELETE_CALL]);
}
