mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::{Fixture, Home, assert_ok, dashboard_viewer, eventually, http, status_of, up_with_fixtures};
use miniserde::{Deserialize, json};

const PAGE: &str = r#"<!doctype html><title>Authority</title>
<button onclick="events.push('pressed')">Press</button>
<script>window.events = []</script>"#;

fn site(_path: &str, _port: u16) -> String {
    PAGE.to_string()
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
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n6_taking_control_yields_to_a_native_wait_which_ends_taken_over_and_resuming_keeps_the_browser() {
    let fixture = Fixture::start(site);
    let home = Home::new("n5-a");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n5-a");
    let tab = reply(&home, &["tab", "new", s, &fixture.url("/")]).tabs.and_then(|t| t.selected).expect("a new tab is selected");
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
