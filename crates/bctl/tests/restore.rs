mod common;

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use bctl_rfb::{Buttons, Client};
use bctl_runtime_smolvm::{SmolVm, TemplateSpec};
use common::{Home, assert_ok, status_of};
use miniserde::{Deserialize, json};

const PAGE: &str = r#"<!doctype html><title>X1 template</title>
<style>
html, body { margin: 0; min-height: 100%; background: #cfe8ff }
#k { position: absolute; left: 100px; top: 150px; width: 900px; height: 400px; font-size: 32px; border: 4px solid #224466 }
</style>
<input id=k autofocus>
"#;

/// A data URL keeps the page local: a restored template has no guest network.
fn page_url() -> String {
    let mut encoded = String::new();
    for byte in PAGE.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("data:text/html,{encoded}")
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[derive(Deserialize)]
struct Action {
    value: Option<json::Value>,
}

#[derive(Deserialize)]
struct PageReply {
    action: Option<Action>,
}

#[derive(Deserialize)]
struct EvalReply {
    error: Option<common::Error>,
    page: Option<PageReply>,
}

fn eval(home: &Home, session: &str, expression: &str) -> Result<String, String> {
    let output = home.bctl(&["page", "eval", session, expression, "--json"]);
    let reply: EvalReply = json::from_str(&String::from_utf8_lossy(&output.stdout)).map_err(|_| format!("{output:?}"))?;
    if let Some(error) = reply.error {
        return Err(error.code);
    }
    match reply.page.and_then(|page| page.action).and_then(|action| action.value) {
        Some(json::Value::String(value)) => Ok(value),
        other => Err(json::to_string(&other.unwrap_or(json::Value::Null))),
    }
}

fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Result<T, String>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match probe() {
            Ok(value) => return value,
            Err(error) if Instant::now() >= deadline => panic!("timed out waiting for {what}: {error}"),
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn template_spec(tag: &str) -> TemplateSpec {
    let root = workspace();
    let scratch = root.join(".work/tmp");
    let cdp_dir = scratch.join(format!("x1-tpl-cdp-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&cdp_dir).unwrap();
    TemplateSpec {
        machine: format!("bctl-x1-template-{}-{tag}", std::process::id()),
        image: root.join(".work/images").join(format!("{}.tar", common::image())),
        checkpoint: scratch.join(format!("x1-template-{}-{tag}.checkpoint", std::process::id())),
        staging_dir: scratch.join(format!("x1-template-staging-{}-{tag}", std::process::id())),
        display: (1280, 800),
        memory_mib: 4096,
        cpus: 2,
        cdp_socket: cdp_dir.join("s"),
    }
}

fn guest_check(smolvm: &Path, machine: &str) -> String {
    let output = std::process::Command::new(smolvm)
        .args(["machine", "exec", "--name", machine, "--", "bctl-guest", "check"])
        .output()
        .expect("run bctl-guest check");
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the helium image in .work/images"]
fn a_session_restored_from_a_pre_browser_template_reaches_ready_with_the_sandbox_intact() {
    let template = template_spec("restore");
    let runtime = SmolVm::new(workspace().join(".work/runtime/smolvm/smolvm"));
    runtime.create_template(&template).unwrap_or_else(|e| panic!("create template: {e}"));

    let fixture = page_url();
    let home = Home::new("x1r");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    let template_env = template.checkpoint.to_string_lossy().into_owned();
    let up = home.bctl_env(
        &["up", s, "--profile", "x1r", "--image", &common::image()],
        &[("BCTL_TEMPLATE_CHECKPOINT", &template_env)],
    );
    assert_ok(&up);

    let response = home.json(&["inspect", s]);
    let inspected = status_of(&response, s);
    assert_eq!(inspected.state, "ready");
    let machine = inspected.machine.clone().expect("a ready session names its machine");
    let vnc_port = inspected.vnc_port.expect("a ready session has a VNC port");
    assert!(!runtime.list().unwrap().iter().any(|m| m.name == template.machine), "the template source is deleted after capture");

    assert_ok(&home.bctl(&["tab", "new", s, &fixture]));
    let title = wait_for("the fixture page", || {
        let output = home.bctl(&["page", "get", s, "title", "--json"]);
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() && text.contains("X1 template") { Ok(text) } else { Err(text) }
    });
    assert!(title.contains("X1 template"), "{title}");

    let mut vnc = Client::connect(("127.0.0.1", vnc_port), Duration::from_secs(10)).unwrap();
    let frame: bctl_rfb::Frame = vnc.refresh().unwrap().clone();
    assert_eq!((frame.width(), frame.height()), (1280, 800));
    let black = bctl_rfb::Rgb(0, 0, 0);
    let drawn = (0..frame.height()).step_by(8).flat_map(|y| (0..frame.width()).step_by(8).map(move |x| (x, y))).filter(|&(x, y)| frame.pixel(x, y) != black).count();
    assert!(drawn > 1000, "the restored session's frame is not drawn ({drawn} sampled pixels)");

    vnc.click(640, 500).unwrap();
    vnc.type_text("x1").unwrap();
    let typed = wait_for("RFB input to reach the page", || eval(&home, s, "document.getElementById('k').value").map_err(|e| e.to_string()));
    assert_eq!(typed, "x1", "B1-style RFB input reaches the restored page");
    vnc.pointer(640, 500, Buttons::NONE).unwrap();

    let check = guest_check(&runtime.binary(), &machine);
    assert!(check.lines().any(|line| line == "sandbox ok"), "renderer sandbox: {check}");
    assert!(check.lines().any(|line| line == "browser ok"), "browser: {check}");
    assert!(check.lines().any(|line| line == "input ok"), "input: {check}");

    assert_ok(&home.bctl(&["down", s]));
}
