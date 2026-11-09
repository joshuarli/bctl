mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use bctl_cdp::protocol::{Command as CdpCommand, Effect};
use bctl_cdp::Scope;
use common::{
    Home, Session, assert_ok, browser_answers, dashboard_viewer, eventually, http, image, open_vnc_viewer, outside_client, port_open, process_alive,
    status_of,
};
use futures_lite::future::block_on;
use miniserde::{Deserialize, Serialize, json};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Deserialize, Debug)]
struct Attempt {
    pid: Option<u32>,
}

#[derive(Deserialize, Debug)]
struct Run {
    id: i64,
    state: String,
    outcome: Option<String>,
    detail: Option<String>,
    output_dir: String,
    attempts: Vec<Attempt>,
}

#[derive(Deserialize)]
struct Runs {
    error: Option<common::Error>,
    runs: Vec<Run>,
}

#[derive(Deserialize)]
struct ErrorReply {
    error: Option<common::Error>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct ControlState {
    authority: String,
    mine: bool,
    settling: u64,
}

#[derive(Deserialize)]
struct TabsReply {
    error: Option<common::Error>,
    tabs: Option<Tabs>,
}

#[derive(Deserialize)]
struct Tabs {
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    id: String,
}

#[derive(Serialize)]
struct CloseTarget {
    #[serde(rename = "targetId")]
    target_id: String,
}

#[derive(Deserialize)]
struct Closed {
    success: bool,
}

impl CdpCommand for CloseTarget {
    const METHOD: &'static str = "Target.closeTarget";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Closed;
}

fn write_config(home: &Home, session: &str, job: &str, kind: &str) -> PathBuf {
    let image = image();
    let body = match kind {
        "sleep" => format!(
            r#"version = 1
[browsers.default]
image = "{image}"

[jobs.{job}]
kind = "exec"
browser = "default"
profile = "{session}"
argv = ["sleep", "600"]
"#
        ),
        "fill" => format!(
            r#"version = 1
[browsers.default]
image = "{image}"

[jobs.{job}]
kind = "exec"
browser = "default"
profile = "{session}"
argv = ["sh", "-c", 'i=0; while [ "$i" -lt 700 ]; do head -c 1048576 /dev/zero >> "$BCTL_OUTPUT_DIR/fill.bin"; i=$((i+1)); done']
"#
        ),
        "ok" => format!(
            r#"version = 1
[browsers.default]
image = "{image}"

[jobs.{job}]
kind = "exec"
browser = "default"
profile = "{session}"
argv = ["true"]
"#
        ),
        other => panic!("unknown config kind {other}"),
    };
    let path = home.path.join("bctl.toml");
    std::fs::write(&path, body).unwrap();
    path
}

fn runs(home: &Home, args: &[&str]) -> Runs {
    let mut args = args.to_vec();
    args.push("--json");
    let output = home.bctl(&args);
    json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {args:?}: {output:?}"))
}

fn one_run(home: &Home, id: i64) -> Run {
    let mut response = runs(home, &["runs", &id.to_string()]);
    assert!(response.error.is_none(), "bctl runs {id} failed: {:?}", response.error.map(|e| e.code));
    response.runs.pop().unwrap_or_else(|| panic!("bctl runs {id} returned nothing"))
}

fn started(home: &Home, session: &str, job: &str, config: &Path) -> Run {
    let mut response = runs(home, &["run", job, "--session", session, "--config", config.to_str().unwrap()]);
    assert!(response.error.is_none(), "bctl run {job} failed: {:?}", response.error.map(|e| e.code));
    response.runs.pop().unwrap_or_else(|| panic!("bctl run {job} returned no run"))
}

fn wait_run(home: &Home, id: i64, what: &str, timeout: Duration, done: impl Fn(&Run) -> bool) -> Run {
    let deadline = Instant::now() + timeout;
    loop {
        let current = one_run(home, id);
        if done(&current) {
            return current;
        }
        assert!(Instant::now() < deadline, "run {id} never became {what}; last {current:?}");
        thread::sleep(Duration::from_millis(250));
    }
}

fn ended(home: &Home, id: i64, timeout: Duration) -> Run {
    wait_run(home, id, "ended", timeout, |run| !matches!(run.state.as_str(), "starting" | "running" | "needs-interaction" | "paused"))
}

fn session(home: &Home, name: &str) -> Session {
    status_of(&home.json(&["status"]), name).clone()
}

fn wait_session(home: &Home, name: &str, what: &str, done: impl Fn(&Session) -> bool) -> Session {
    eventually(what, || {
        let current = session(home, name);
        done(&current).then_some(current)
    })
}

fn control(port: u16, address: &str, headers: &str, session: &str, method: &str, path: &str) -> (u16, String) {
    let (status, response) = http(port, address, &format!("{method} /api/sessions/{session}/control{path}"), headers);
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default();
    (status, body)
}

fn control_state(body: &str) -> ControlState {
    json::from_str(body).unwrap_or_else(|_| panic!("{body}"))
}

fn assert_control(port: u16, address: &str, headers: &str, session: &str, authority: &str) -> ControlState {
    let (status, body) = control(port, address, headers, session, "GET", "");
    assert_eq!(status, 200, "{body}");
    let state = control_state(&body);
    assert_eq!(state.authority, authority, "{state:?}");
    state
}

fn kill_daemon(home: &Home) {
    let daemon = home.daemon_pid();
    assert!(!daemon.is_empty(), "no daemon holds the lock");
    assert!(Command::new("kill").args(["-9", &daemon]).status().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let pid: u32 = daemon.parse().unwrap();
    while process_alive(pid) {
        assert!(Instant::now() < deadline, "daemon {daemon} survived kill -9");
        thread::sleep(Duration::from_millis(50));
    }
}

fn tabs(home: &Home, session: &str) -> Tabs {
    let output = home.bctl(&["tab", "list", session, "--json"]);
    let reply: TabsReply = json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl tab list: {output:?}"));
    assert!(reply.error.is_none(), "bctl tab list failed: {:?}", reply.error.map(|e| e.code));
    reply.tabs.unwrap_or_else(|| panic!("bctl tab list returned no tabs"))
}

fn assert_cleaned(home: &Home, session: &str, vnc_port: Option<u16>) {
    eventually("the VM to be removed", || home.bctl_machines().is_empty().then_some(()));
    if let Some(port) = vnc_port {
        eventually("the VNC port to close", || (!port_open(port)).then_some(()));
    }
    if let Ok(record) = home.store_session(session) {
        assert_eq!(record.machine, None, "failed session {session} still names a machine");
        assert_eq!(record.vnc_port, None, "failed session {session} still names a port");
    }
}

impl Home {
    fn store_session(&self, name: &str) -> Result<Session, ()> {
        let response = self.json(&["status"]);
        response.sessions.into_iter().find(|s| s.name == name).ok_or(())
    }
}

fn browser_process() -> &'static str {
    if image().starts_with("helium") || image().starts_with("ref-scratch") { "helium" } else { "chromium" }
}

fn compositor_process() -> &'static str {
    if image() == "ref-alpine-chromium" { "sway" } else { "tinyland" }
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn killing_the_guest_browser_fails_the_run_and_the_session_and_leaves_no_vm_port_or_lease() {
    let _serial = serial();
    let home = Home::new("h3-browser");
    let session_name = format!("{}-browser", home.prefix());
    let s = session_name.as_str();
    let config = write_config(&home, s, "idle", "sleep");
    let run = started(&home, s, "idle", &config);
    wait_run(&home, run.id, "running", Duration::from_secs(120), |r| r.state == "running");
    let record = session(&home, s);
    let machine = record.machine.clone().unwrap();
    let vnc_port = record.vnc_port.unwrap();
    let leader = one_run(&home, run.id).attempts[0].pid.unwrap();
    let (port, address, as_viewer) = dashboard_viewer(&home);
    assert_eq!(control(port, &address, &as_viewer, s, "POST", "").0, 200, "a viewer takes control");
    assert_control(port, &address, &as_viewer, s, "human");

    home.kill_guest_process(&machine, browser_process());
    let failed = ended(&home, run.id, Duration::from_secs(60));
    assert_eq!((failed.state.as_str(), failed.outcome.as_deref()), ("failed", Some("browser-crashed")), "{failed:?}");
    let detail = failed.detail.unwrap_or_default();
    assert!(detail.contains("browser exited unexpectedly") || detail.contains("stopped unexpectedly"), "{detail}");
    assert!(!process_alive(leader), "the run's process group outlived the failed session");

    let failed_session = wait_session(&home, s, "the session to fail", |record| record.state == "failed");
    let failure = failed_session.failure.unwrap_or_default();
    assert!(failure.contains("browser exited unexpectedly"), "{failure}");
    assert_eq!(failed_session.generation, record.generation, "the failure belongs to the session's live generation");
    assert_control(port, &address, &as_viewer, s, "automation");
    assert_cleaned(&home, s, Some(vnc_port));
    assert_ok(&home.bctl(&["down", s]));
    assert!(home.bctl_machines().is_empty());
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn killing_the_guest_compositor_fails_the_run_and_the_session_without_leaving_a_vm_port_or_lease() {
    let _serial = serial();
    let home = Home::new("h3-compositor");
    let session_name = format!("{}-compositor", home.prefix());
    let s = session_name.as_str();
    let config = write_config(&home, s, "idle", "sleep");
    let run = started(&home, s, "idle", &config);
    wait_run(&home, run.id, "running", Duration::from_secs(120), |r| r.state == "running");
    let record = session(&home, s);
    let machine = record.machine.clone().unwrap();
    let vnc_port = record.vnc_port.unwrap();
    let leader = one_run(&home, run.id).attempts[0].pid.unwrap();
    let (port, address, as_viewer) = dashboard_viewer(&home);
    assert_eq!(control(port, &address, &as_viewer, s, "POST", "").0, 200);
    assert_control(port, &address, &as_viewer, s, "human");

    home.kill_guest_process(&machine, compositor_process());
    let failed = ended(&home, run.id, Duration::from_secs(60));
    assert_eq!((failed.state.as_str(), failed.outcome.as_deref()), ("failed", Some("browser-crashed")), "{failed:?}");
    let detail = failed.detail.unwrap_or_default();
    assert!(detail.contains("compositor exited unexpectedly") || detail.contains("stopped unexpectedly"), "{detail}");
    assert!(!process_alive(leader), "the run's process group outlived the failed session");

    let failed_session = wait_session(&home, s, "the session to fail", |record| record.state == "failed");
    let failure = failed_session.failure.unwrap_or_default();
    assert!(failure.contains("compositor exited unexpectedly"), "{failure}");
    assert_control(port, &address, &as_viewer, s, "automation");
    assert_cleaned(&home, s, Some(vnc_port));
    assert_ok(&home.bctl(&["down", s]));
    assert!(home.bctl_machines().is_empty());
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn killing_the_vm_process_fails_the_run_and_the_session_and_leaves_no_vm_port_or_lease() {
    let _serial = serial();
    let home = Home::new("h3-vm");
    let session_name = format!("{}-vm", home.prefix());
    let s = session_name.as_str();
    let config = write_config(&home, s, "idle", "sleep");
    let run = started(&home, s, "idle", &config);
    wait_run(&home, run.id, "running", Duration::from_secs(120), |r| r.state == "running");
    let record = session(&home, s);
    let machine = record.machine.clone().unwrap();
    let vnc_port = record.vnc_port.unwrap();
    let leader = one_run(&home, run.id).attempts[0].pid.unwrap();
    let vmm = home.machine_status(&machine).pid.expect("a running machine names its VMM pid");
    assert!(vmm > 0, "refusing to signal pid 0");
    let (port, address, as_viewer) = dashboard_viewer(&home);
    assert_eq!(control(port, &address, &as_viewer, s, "POST", "").0, 200);

    assert!(Command::new("kill").args(["-9", &vmm.to_string()]).status().unwrap().success());
    let failed = ended(&home, run.id, Duration::from_secs(60));
    assert_eq!((failed.state.as_str(), failed.outcome.as_deref()), ("failed", Some("browser-crashed")), "{failed:?}");
    let detail = failed.detail.unwrap_or_default();
    assert!(detail.contains("stopped unexpectedly") || detail.contains("stopped answering CDP"), "{detail}");
    assert!(!process_alive(leader), "the run's process group outlived the failed session");

    let failed_session = wait_session(&home, s, "the session to fail", |record| record.state == "failed");
    assert!(failed_session.failure.is_some_and(|f| f.contains("stopped unexpectedly") || f.contains("stopped answering CDP")));
    assert_control(port, &address, &as_viewer, s, "automation");
    assert_cleaned(&home, s, Some(vnc_port));
    assert_ok(&home.bctl(&["down", s]));
    assert!(home.bctl_machines().is_empty());
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn killing_the_daemon_during_a_run_interrupts_it_and_the_adopted_vm_is_later_released() {
    let _serial = serial();
    let home = Home::new("h3-daemon");
    let session_name = format!("{}-daemon", home.prefix());
    let s = session_name.as_str();
    let config = write_config(&home, s, "idle", "sleep");
    let run = started(&home, s, "idle", &config);
    wait_run(&home, run.id, "running", Duration::from_secs(120), |r| r.state == "running");
    let before = session(&home, s);
    let vnc_port = before.vnc_port.unwrap();
    let leader = one_run(&home, run.id).attempts[0].pid.unwrap();

    kill_daemon(&home);
    let interrupted = wait_run(&home, run.id, "interrupted", Duration::from_secs(60), |r| {
        matches!(r.state.as_str(), "interrupted" | "cancelled" | "failed")
    });
    assert_eq!((interrupted.state.as_str(), interrupted.outcome.as_deref()), ("interrupted", Some("interrupted")), "{interrupted:?}");
    assert!(!process_alive(leader), "the restarted daemon left the run's process group running");

    let adopted = session(&home, s);
    assert_eq!((adopted.state.as_str(), adopted.generation), ("ready", before.generation), "the restart kept the running VM");
    assert!(browser_answers(&home, s), "the adopted VM's browser still answers CDP");
    let (port, address, as_viewer) = dashboard_viewer(&home);
    assert_control(port, &address, &as_viewer, s, "automation");

    assert_ok(&home.bctl(&["down", s]));
    assert_cleaned(&home, s, Some(vnc_port));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn filling_the_guest_disk_during_a_run_reaches_named_states_and_leaves_no_leak() {
    let _serial = serial();
    let home = Home::new("h3-guest-disk");
    let session_name = format!("{}-guest-disk", home.prefix());
    let s = session_name.as_str();
    let config = write_config(&home, s, "idle", "sleep");
    let run = started(&home, s, "idle", &config);
    wait_run(&home, run.id, "running", Duration::from_secs(120), |r| r.state == "running");
    let record = session(&home, s);
    let machine = record.machine.clone().unwrap();
    let vnc_port = record.vnc_port.unwrap();
    let leader = one_run(&home, run.id).attempts[0].pid.unwrap();

    let filled = home.guest_shell(&machine, "/tmp/busybox dd if=/dev/zero of=/tmp/fill.bin bs=1M 2>&1; echo dd_status=$?; /tmp/busybox df -h /");
    let text = format!("{}{}", String::from_utf8_lossy(&filled.stdout), String::from_utf8_lossy(&filled.stderr));
    assert!(text.contains("No space left on device"), "the guest disk never filled: {text}");
    assert!(text.contains("100%"), "the guest filesystem is not full: {text}");

    let after_fill = one_run(&home, run.id);
    assert_eq!(after_fill.state, "running", "a full guest disk left the run in no named state: {after_fill:?}");
    let session_after = session(&home, s);
    assert_eq!(session_after.state, "ready", "a full guest disk failed the session instead of naming a survived state");
    assert!(browser_answers(&home, s), "the appliance stopped answering CDP with a full writable disk");
    let (port, address, as_viewer) = dashboard_viewer(&home);
    assert_control(port, &address, &as_viewer, s, "automation");

    assert_ok(&home.bctl(&["cancel", &run.id.to_string()]));
    let cancelled = ended(&home, run.id, Duration::from_secs(60));
    assert_eq!((cancelled.state.as_str(), cancelled.outcome.as_deref()), ("cancelled", Some("cancelled")), "{cancelled:?}");
    assert!(!process_alive(leader));

    assert_ok(&home.bctl(&["down", s]));
    assert_cleaned(&home, s, Some(vnc_port));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn a_run_that_fills_its_output_bound_ends_as_a_job_error_and_the_session_survives() {
    let _serial = serial();
    let home = Home::new("h3-disk");
    let session_name = format!("{}-disk", home.prefix());
    let s = session_name.as_str();
    let fill = write_config(&home, s, "fill", "fill");
    let run = started(&home, s, "fill", &fill);
    let leader = one_run(&home, run.id).attempts[0].pid.unwrap();

    let failed = wait_run(&home, run.id, "stopped by its output bound", Duration::from_secs(180), |r| {
        r.state == "failed"
    });
    assert_eq!(failed.outcome.as_deref(), Some("job-error"), "{failed:?}");
    let detail = failed.detail.unwrap_or_default();
    assert!(detail.contains("MiB bound") && detail.contains("output"), "{detail}");
    assert!(!process_alive(leader), "the job's process group outlived its failed run");
    let written = std::fs::metadata(Path::new(&failed.output_dir).join("fill.bin")).map(|m| m.len()).unwrap_or(0);
    assert!(written > 0, "the injection never wrote to the run's output");

    let record = session(&home, s);
    assert_eq!(record.state, "ready", "the browser is untouched by a full run output");
    assert!(browser_answers(&home, s));
    let ok_config = write_config(&home, s, "ok", "ok");
    let ok = started(&home, s, "ok", &ok_config);
    assert_eq!(ended(&home, ok.id, Duration::from_secs(60)).outcome.as_deref(), Some("succeeded"));

    let vnc_port = record.vnc_port;
    assert_ok(&home.bctl(&["down", s]));
    assert_cleaned(&home, s, vnc_port);
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn dropping_the_viewer_releases_the_input_lease_and_the_run_keeps_its_named_end() {
    let _serial = serial();
    let home = Home::new("h3-viewer");
    let session_name = format!("{}-viewer", home.prefix());
    let s = session_name.as_str();
    let config = write_config(&home, s, "idle", "sleep");
    let run = started(&home, s, "idle", &config);
    wait_run(&home, run.id, "running", Duration::from_secs(120), |r| r.state == "running");
    let record = session(&home, s);
    let vnc_port = record.vnc_port.unwrap();
    let leader = one_run(&home, run.id).attempts[0].pid.unwrap();

    let (port, address, first) = dashboard_viewer(&home);
    let (_, _, second) = dashboard_viewer(&home);
    assert_eq!(control(port, &address, &first, s, "POST", "").0, 200, "the first viewer takes control");
    assert_eq!(control(port, &address, &second, s, "POST", "").0, 409, "input is exclusive while the first viewer holds it");

    let viewer = open_vnc_viewer(port, &address, &first, s);
    thread::sleep(Duration::from_millis(500));
    drop(viewer);

    let taken = eventually("the lease to return to automation", || {
        let (status, body) = control(port, &address, &second, s, "POST", "");
        (status == 200 && control_state(&body).authority == "human").then_some(control_state(&body))
    });
    assert!(taken.mine && taken.settling == 0, "{taken:?}");
    assert_eq!(control(port, &address, &second, s, "POST", "/resume").0, 200);
    assert_control(port, &address, &second, s, "automation");
    assert!(control(port, &address, &first, s, "GET", "").0 == 200, "the dropped viewer's cookie still reads state");

    assert_ok(&home.bctl(&["cancel", &run.id.to_string()]));
    let cancelled = ended(&home, run.id, Duration::from_secs(60));
    assert_eq!((cancelled.state.as_str(), cancelled.outcome.as_deref()), ("cancelled", Some("cancelled")), "{cancelled:?}");
    assert!(!process_alive(leader));

    assert_ok(&home.bctl(&["down", s]));
    assert_cleaned(&home, s, Some(vnc_port));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn closing_the_last_tab_exits_the_browser_and_names_the_session_failed() {
    let _serial = serial();
    let home = Home::new("h3-last-tab");
    let session_name = format!("{}-last-tab", home.prefix());
    let s = session_name.as_str();
    let config = write_config(&home, s, "idle", "sleep");
    let run = started(&home, s, "idle", &config);
    wait_run(&home, run.id, "running", Duration::from_secs(120), |r| r.state == "running");
    let record = session(&home, s);
    let vnc_port = record.vnc_port.unwrap();
    let leader = one_run(&home, run.id).attempts[0].pid.unwrap();

    let listing = tabs(&home, s);
    assert_eq!(listing.targets.len(), 1, "the browser starts with one page target");
    let target = listing.targets[0].id.clone();
    let refused = home.bctl(&["tab", "close", s, &target, "--json"]);
    let refusal: ErrorReply = json::from_str(&String::from_utf8_lossy(&refused.stdout)).unwrap();
    assert_eq!(refusal.error.map(|e| e.code).as_deref(), Some("LAST_TARGET"), "bctl never closes the last target itself");

    let (port, address, as_viewer) = dashboard_viewer(&home);
    let outside = outside_client(&home, s);
    let closed = block_on(outside.call(Scope::Browser, &CloseTarget { target_id: target }, common::soon())).unwrap();
    assert!(closed.success, "the outside CDP client closed the only page");

    let failed = ended(&home, run.id, Duration::from_secs(60));
    assert_eq!((failed.state.as_str(), failed.outcome.as_deref()), ("failed", Some("browser-crashed")), "{failed:?}");
    let detail = failed.detail.unwrap_or_default();
    assert!(detail.contains("browser exited unexpectedly") || detail.contains("stopped unexpectedly"), "{detail}");
    assert!(!process_alive(leader));

    let failed_session = wait_session(&home, s, "the session to fail", |record| record.state == "failed");
    let failure = failed_session.failure.unwrap_or_default();
    assert!(failure.contains("browser exited unexpectedly"), "{failure}");
    assert_control(port, &address, &as_viewer, s, "automation");
    assert_cleaned(&home, s, Some(vnc_port));
    assert_ok(&home.bctl(&["down", s]));
    assert!(home.bctl_machines().is_empty());
}
