mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use common::{Fixture, Home, assert_ok, dashboard_viewer, eventually, http, status_of, up_with_fixtures};
use miniserde::{Deserialize, json};

const PAGE: &str = r#"<!doctype html><title>Authority</title>
<button onclick="events.push('pressed')">Press</button>
<script>window.events = []</script>"#;

const PHASES: &str = r#"<!doctype html><title>Phases</title>
<style>
  body { margin: 0; overflow: hidden }
  button, input { display: block; margin: 4px; width: 120px }
  @keyframes move { from { margin-left: 0 } to { margin-left: 100px } }
  #spinning { animation: 1s linear infinite move }
  #offscreen { position: absolute; left: -500px; top: 0 }
  #cover { position: absolute; left: 0; top: 0; width: 300px; height: 300px }
</style>
<button id=hidden style="display: none">Hidden</button>
<button id=off disabled>Off</button>
<button id=spinning>Spinning</button>
<button id=offscreen>Offscreen</button>
<input id=readonly readonly value=fixed>
<button id=covered>Covered</button>
<button id=late disabled>Late</button>
<div id=cover></div>
<p id=count style="position: absolute; right: 0; bottom: 0">0</p>
<script>
window.seen = [];
for (const type of ['mousedown', 'mouseup', 'click', 'keydown', 'input']) {
  addEventListener(type, event => {
    seen.push({ what: type, on: event.target.id || event.target.localName });
    document.getElementById('count').textContent = String(seen.length);
  }, true);
}
let pending;
function watch() {
  fetch('/armed').then(r => r.text()).then(t => t === 'yes' ? enableLater(0) : setTimeout(watch, 2), () => setTimeout(watch, 2));
}
function reset() {
  clearTimeout(pending);
  watch();
  document.getElementById('late').disabled = true;
  if (!document.getElementById('cover')) document.body.append(Object.assign(document.createElement('div'), { id: 'cover' }));
  seen.length = 0;
  document.getElementById('count').textContent = '0';
  return 'reset';
}
function enableLater(after) {
  pending = setTimeout(() => { document.getElementById('cover').remove(); document.getElementById('late').disabled = false; }, after);
  return 'armed';
}
</script>"#;

/// Whether the `/phases` page, polling `/armed`, should enable `#late`: the test arms it without a CDP connection,
/// which would hold the session and refuse the takeover under test.
static ARMED: AtomicBool = AtomicBool::new(false);

fn site(path: &str, _port: u16) -> String {
    match path {
        "/phases" => PHASES.to_string(),
        "/armed" => if ARMED.load(Ordering::SeqCst) { "yes" } else { "no" }.to_string(),
        _ => PAGE.to_string(),
    }
}

#[derive(Deserialize, Debug)]
struct Seen {
    what: String,
    on: String,
}

#[derive(Deserialize, Debug)]
struct Error {
    code: String,
    message: String,
}

#[derive(Deserialize, Debug)]
struct ActionView {
    text: Option<String>,
    value: Option<json::Value>,
}

#[derive(Deserialize, Debug)]
struct PageView {
    action: Option<ActionView>,
}

#[derive(Deserialize, Debug)]
struct Tabs {
    selected: Option<String>,
}

#[derive(Deserialize, Debug)]
struct Reply {
    error: Option<Error>,
    page: Option<PageView>,
    tabs: Option<Tabs>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct ControlState {
    authority: String,
    mine: bool,
    settling: u64,
}

fn reply(home: &Home, args: &[&str]) -> Reply {
    let mut argv = args.to_vec();
    argv.push("--json");
    let output = home.bctl(&argv);
    json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {argv:?}: {output:?}"))
}

fn refused(home: &Home, args: &[&str]) -> Error {
    reply(home, args).error.unwrap_or_else(|| panic!("bctl {args:?} succeeded"))
}

fn acted(home: &Home, args: &[&str]) -> ActionView {
    match reply(home, args) {
        Reply { error: None, page: Some(PageView { action: Some(action) }), .. } => action,
        other => panic!("bctl {args:?}: {other:?}"),
    }
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn n6_taking_control_yields_to_a_native_wait_which_ends_taken_over_and_resuming_keeps_the_browser() {
    let fixture = Fixture::start(site);
    let home = Home::new("n5-a");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n5-a");
    let tab = reply(&home, &["tab", "new", s, &fixture.url("/")]).tabs.and_then(|t| t.selected).expect("a new tab is selected");
    acted(&home, &["page", "wait", s, "--url", &fixture.url("/")]);
    acted(&home, &["page", "wait", s, "--load", "load"]);
    acted(&home, &["page", "eval", s, "document.cookie = 'kept=yes; max-age=3600'"]);
    let generation = status_of(&home.json(&["inspect", s]), s).generation;

    let (port, address, as_viewer) = dashboard_viewer(&home);
    let control = |method: &str, path: &str| {
        let (status, response) = http(port, &address, &format!("{method} /api/sessions/{s}/control{path}"), &as_viewer);
        let body = response.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default();
        (status, body)
    };
    let state = |(status, body): (u16, String)| -> ControlState {
        assert_eq!(status, 200, "{body}");
        json::from_str(&body).unwrap_or_else(|_| panic!("{body}"))
    };
    assert_eq!(state(control("GET", "")), ControlState { authority: "automation".into(), mine: false, settling: 0 });

    let waiting = {
        let home_path = home.path.clone();
        let session = session.clone();
        thread::spawn(move || {
            let started = Instant::now();
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_bctl"))
                .args(["page", "wait", &session, "--css", "#never", "--timeout", "120000", "--json"])
                .env("BCTL_HOME", home_path)
                .output()
                .unwrap();
            (started.elapsed(), json::from_str::<Reply>(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("{output:?}")))
        })
    };
    eventually("the wait to be in flight", || (state(control("GET", "")).settling == 1).then_some(()));

    let taken = Instant::now();
    assert_eq!(state(control("POST", "")), ControlState { authority: "yielding".into(), mine: true, settling: 1 }, "a take waits for the native wait");
    let (waited, ended) = waiting.join().unwrap();
    let error = ended.error.expect("the wait does not succeed");
    assert_eq!(error.code, "TAKEN_OVER", "{error:?}");
    assert!(error.message.contains("a person took control of session"), "{}", error.message);
    assert!(waited < Duration::from_secs(30) && taken.elapsed() < Duration::from_secs(5), "the wait ended soon after the takeover, not at its timeout: {waited:?}");
    assert_eq!(eventually("the takeover to hand over", || Some(state(control("GET", ""))).filter(|c| c.authority == "human")), ControlState {
        authority: "human".into(),
        mine: true,
        settling: 0
    });

    for args in [
        vec!["page", "click", s, "--role", "button", "--name", "Press"],
        vec!["page", "goto", s, "about:blank"],
        vec!["page", "eval", s, "events.push('evaluated')"],
        vec!["tab", "new", s],
    ] {
        assert_eq!(refused(&home, &args).code, "HUMAN_IN_CONTROL", "{args:?}");
    }
    let batch = home.path.join("batch.json");
    std::fs::write(&batch, r#"[{"action": "get", "read": "title"}, {"action": "click", "role": "button", "name": "Press"}]"#).unwrap();
    assert_eq!(refused(&home, &["page", "batch", s, batch.to_str().unwrap()]).code, "HUMAN_IN_CONTROL", "a batch with any change is refused whole");
    assert_eq!(acted(&home, &["page", "get", s, "title"]).text.as_deref(), Some("Authority"), "reads go on while a person has control");
    let cdp_url = home.cdp_url(s);
    let capability = cdp_url.strip_prefix("http://").unwrap();
    let (status, refusal) = http(port, capability, "GET /json/version", "");
    assert_eq!(status, 409, "new CDP attachments are refused while a person has control: {refusal}");

    assert_eq!(state(control("POST", "/resume")), ControlState { authority: "automation".into(), mine: false, settling: 0 });
    assert_eq!(reply(&home, &["tab", "list", s]).tabs.and_then(|t| t.selected).as_deref(), Some(tab.as_str()), "the selected tab survives the takeover");
    assert_eq!(status_of(&home.json(&["inspect", s]), s).generation, generation, "the browser survives the takeover");
    let profiles = String::from_utf8_lossy(&home.bctl(&["profile", "list"]).stdout).into_owned();
    assert!(profiles.lines().any(|line| line.split_whitespace().eq(["n5-a", s])), "the session still holds its profile: {profiles}");
    acted(&home, &["page", "click", s, "--role", "button", "--name", "Press"]);
    let read = acted(&home, &["page", "eval", s, "[events.join('|'), document.cookie]"]);
    assert_eq!(read.value.map(|v| json::to_string(&v)).as_deref(), Some("[\"pressed\",\"kept=yes\"]"), "the page and its cookies are intact and act again");
    let (status, _) = http(port, capability, "GET /json/version", "");
    assert_eq!(status, 200, "CDP attachments are accepted again under automation");
    assert_ok(&home.bctl(&["down", s]));
}

fn bctl_in_background(home: &Home, args: Vec<String>) -> thread::JoinHandle<(Instant, Reply)> {
    let path = home.path.clone();
    thread::spawn(move || {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_bctl")).args(&args).arg("--json").env("BCTL_HOME", path).output().unwrap();
        (Instant::now(), json::from_str::<Reply>(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("{output:?}")))
    })
}

fn seen(home: &Home, session: &str) -> Vec<Seen> {
    let value = acted(home, &["page", "eval", session, "seen"]).value.expect("eval returns a value");
    json::from_str(&json::to_string(&value)).unwrap()
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn n7_a_takeover_ends_an_action_in_any_part_of_its_preparation_and_no_input_follows() {
    let fixture = Fixture::start(site);
    let home = Home::new("n7-a");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n7-a");
    reply(&home, &["tab", "new", s, &fixture.url("/phases")]).tabs.and_then(|t| t.selected).expect("a new tab is selected");
    acted(&home, &["page", "wait", s, "--url", &fixture.url("/phases")]);
    acted(&home, &["page", "wait", s, "--load", "load"]);
    let (port, address, as_viewer) = dashboard_viewer(&home);
    let control = |method: &str, path: &str| -> ControlState {
        let (status, response) = http(port, &address, &format!("{method} /api/sessions/{s}/control{path}"), &as_viewer);
        let body = response.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default();
        assert_eq!(status, 200, "{body}");
        json::from_str(&body).unwrap_or_else(|_| panic!("{body}"))
    };
    let batch = home.path.join("batch.json");
    std::fs::write(&batch, r##"[{"action": "eval", "expression": "1"}, {"action": "click", "css": "#never", "timeout_ms": 60000}]"##).unwrap();
    let click = |css: &str| ["page", "click", s, "--css", css, "--timeout", "60000"].map(str::to_string).to_vec();
    let phases: Vec<(&str, Vec<String>)> = vec![
        ("locator evaluation", click("#never")),
        ("visibility", click("#hidden")),
        ("enabled state", click("#off")),
        ("stability", click("#spinning")),
        ("scroll and hit test", click("#offscreen")),
        ("hit test", click("#covered")),
        ("editable state", ["page", "fill", s, "--css", "#readonly", "x", "--timeout", "60000"].map(str::to_string).to_vec()),
        ("batch preparation", ["page", "batch", s, batch.to_str().unwrap()].map(str::to_string).to_vec()),
    ];
    for (phase, args) in phases {
        let running = bctl_in_background(&home, args);
        eventually("the action to be in flight", || {
            assert!(!running.is_finished(), "{phase}: the action ended before the takeover");
            (control("GET", "").settling == 1).then_some(())
        });
        let taken = Instant::now();
        assert_eq!(control("POST", "").authority, "yielding", "{phase}: the takeover waits for the action");
        let (ended, outcome) = running.join().unwrap();
        let error = outcome.error.unwrap_or_else(|| panic!("{phase}: the action succeeded"));
        assert_eq!(error.code, "TAKEN_OVER", "{phase}: {error:?}");
        assert!(ended.duration_since(taken) < Duration::from_secs(2), "{phase}: the action ended {:?} after the takeover", ended.duration_since(taken));
        eventually("the takeover to hand over", || (control("GET", "").authority == "human").then_some(()));
        assert_eq!(control("POST", "/resume").authority, "automation");
        assert!(seen(&home, s).is_empty(), "{phase}: no input reached the page");
    }

    for delay in [0, 10, 20, 25, 30, 35, 40, 45, 50, 60, 80, 120] {
        ARMED.store(false, Ordering::SeqCst);
        acted(&home, &["page", "eval", s, "reset()"]);
        let running = bctl_in_background(&home, click("#late"));
        let started = Instant::now();
        while control("GET", "").settling != 1 {
            if running.is_finished() {
                panic!("the click ended before the takeover: {:?}", running.join().unwrap().1);
            }
            assert!(started.elapsed() < Duration::from_secs(20), "the click never started");
            thread::sleep(Duration::from_millis(20));
        }
        ARMED.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(delay));
        control("POST", "");
        let (_, outcome) = running.join().unwrap();
        eventually("the takeover to hand over", || (control("GET", "").authority == "human").then_some(()));
        let count = acted(&home, &["page", "get", s, "text", "--css", "#count"]).text.expect("the page counts its inputs");
        let seen_by_human: usize = count.parse().unwrap_or_else(|_| panic!("the input count reads {count}"));
        assert_eq!(control("POST", "/resume").authority, "automation");
        let record = seen(&home, s);
        eprintln!("n7 takeover race: taken {delay} ms after the click could proceed: {}", outcome.error.as_ref().map_or("clicked", |e| e.code.as_str()));
        match outcome.error {
            Some(error) => {
                assert_eq!(error.code, "TAKEN_OVER", "{error:?}");
                assert!(record.is_empty(), "a click stopped by the takeover sent nothing: {record:?}");
            }
            None => assert!(record.iter().any(|r| r.what == "click" && r.on == "late"), "{record:?}"),
        }
        assert_eq!(record.len(), seen_by_human, "no input arrived once the person had control: {record:?}, {seen_by_human} when control was handed over");
    }
    assert_ok(&home.bctl(&["down", s]));
}
