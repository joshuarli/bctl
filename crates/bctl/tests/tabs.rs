mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use bctl_cdp::protocol::{AttachToTarget, Command as CdpCommand, Effect, Empty, TargetId};
use bctl_cdp::{Connection, Endpoint, Generation, Scope};
use common::{Home, assert_ok};
use futures_lite::future::block_on;
use miniserde::{Deserialize, Serialize, json};

#[derive(Deserialize, Debug)]
struct Tab {
    id: String,
}

#[derive(Deserialize, Debug)]
struct Tabs {
    selected: Option<String>,
    targets: Vec<Tab>,
}

#[derive(Deserialize, Debug)]
struct Error {
    code: String,
    target: Option<String>,
    targets: Option<Vec<Tab>>,
}

#[derive(Deserialize, Debug)]
struct Reply {
    error: Option<Error>,
    tabs: Option<Tabs>,
}

fn tab(home: &Home, args: &[&str]) -> Reply {
    let mut argv = vec!["tab"];
    argv.extend_from_slice(args);
    argv.push("--json");
    let output = home.bctl(&argv);
    json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {argv:?}: {output:?}"))
}

fn listed(home: &Home, args: &[&str]) -> Tabs {
    let reply = tab(home, args);
    match (reply.error, reply.tabs) {
        (None, Some(tabs)) => tabs,
        (error, _) => panic!("bctl tab {args:?} failed: {error:?}"),
    }
}

fn refused(home: &Home, args: &[&str]) -> Error {
    tab(home, args).error.unwrap_or_else(|| panic!("bctl tab {args:?} succeeded"))
}

fn ids(tabs: &[Tab]) -> Vec<&str> {
    let mut ids: Vec<&str> = tabs.iter().map(|t| t.id.as_str()).collect();
    ids.sort_unstable();
    ids
}

fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(200));
    }
}

#[derive(Deserialize)]
struct Version {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

/// A CDP client outside bctl's automation, standing in for a page or a person acting on the browser.
fn outside_client(home: &Home, session: &str) -> Connection {
    let control_url = home.cdp_url(session);
    let host = control_url.strip_prefix("http://").unwrap().to_string();
    let port: u16 = host.rsplit(':').next().unwrap().parse().unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or_else(|| panic!("{response}"));
    let endpoint = Endpoint::parse(&json::from_str::<Version>(body).unwrap().web_socket_debugger_url).unwrap();
    let (connection, driver) = block_on(bctl_cdp::connect(&endpoint, Generation::new(0))).unwrap();
    thread::spawn(move || block_on(driver));
    connection
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

#[derive(Serialize)]
struct CloseTarget {
    #[serde(rename = "targetId")]
    target_id: TargetId,
}

impl CdpCommand for CloseTarget {
    const METHOD: &'static str = "Target.closeTarget";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize)]
struct Evaluate {
    expression: &'static str,
    #[serde(rename = "userGesture")]
    user_gesture: bool,
}

impl CdpCommand for Evaluate {
    const METHOD: &'static str = "Runtime.evaluate";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

fn http(port: u16, host: &str, request_line: &str, extra: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "{request_line} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{extra}\r\n").unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, response)
}

/// Signs a dashboard viewer in and returns the headers that act as it, plus the dashboard's address.
fn dashboard_viewer(home: &Home) -> (u16, String, String) {
    let served = home.bctl(&["serve"]);
    assert_ok(&served);
    let login = String::from_utf8_lossy(&served.stdout).trim().to_string();
    let address = login.strip_prefix("http://").unwrap().split('/').next().unwrap().to_string();
    let port: u16 = address.rsplit(':').next().unwrap().parse().unwrap();
    let (status, response) = http(port, &address, &format!("GET /{}", login.split_once(&format!("{address}/")).unwrap().1), "");
    assert_eq!(status, 303, "{response}");
    let cookie = response.lines().find_map(|l| l.strip_prefix("set-cookie: ")).unwrap().split(';').next().unwrap().to_string();
    (port, address.clone(), format!("Cookie: {cookie}\r\nOrigin: http://{address}\r\nContent-Length: 0\r\n"))
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n2_targets_are_listed_opened_selected_and_closed_without_the_binding_ever_moving_on_its_own() {
    let home = Home::new("n2-t");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    assert_ok(&home.up(s, "n2-t"));

    let initial = listed(&home, &["list", s]);
    assert_eq!(initial.selected, None, "nothing is selected until asked");
    let first = initial.targets.first().expect("a ready session shows a page").id.clone();

    let opened = listed(&home, &["new", s, "about:blank"]);
    let second = opened.selected.clone().expect("a new target is selected");
    assert_ne!(second, first);
    assert_eq!(ids(&opened.targets), ids(&listed(&home, &["list", s]).targets));
    assert!(opened.targets.iter().any(|t| t.id == first));

    assert_eq!(listed(&home, &["use", s, &first]).selected.as_deref(), Some(first.as_str()));
    assert_eq!(refused(&home, &["use", s, "0000"]).code, "NO_SUCH_TARGET");

    let outside = outside_client(&home, s);
    let attached = block_on(outside.call(Scope::Browser, &AttachToTarget::new(TargetId::new(&first)), soon())).unwrap();
    let popup_open = Evaluate { expression: "window.open('about:blank', '_blank') !== null", user_gesture: true };
    block_on(outside.call(Scope::Session(&attached.session_id), &popup_open, soon())).unwrap();
    let with_popup = eventually("the popup to appear", || Some(listed(&home, &["list", s])).filter(|t| t.targets.len() == 3));
    assert_eq!(with_popup.selected.as_deref(), Some(first.as_str()), "a popup must not move the binding");
    let popup = with_popup.targets.iter().map(|t| t.id.clone()).find(|id| *id != first && *id != second).unwrap();
    let closed = listed(&home, &["close", s, &popup]);
    assert_eq!((closed.selected.as_deref(), ids(&closed.targets)), (Some(first.as_str()), ids(&opened.targets)));

    block_on(outside.call(Scope::Browser, &CloseTarget { target_id: TargetId::new(&first) }, soon())).unwrap();
    let gone = eventually("the selected target to be gone", || tab(&home, &["list", s]).error);
    for gone in [gone, refused(&home, &["list", s])] {
        assert_eq!((gone.code.as_str(), gone.target.as_deref()), ("TARGET_GONE", Some(first.as_str())));
        assert_eq!(gone.targets.as_deref().map(ids), Some(vec![second.as_str()]), "recovery lists what remains and switches to none of it");
    }
    assert_eq!(refused(&home, &["close", s, &second]).code, "LAST_TARGET");
    assert_eq!(listed(&home, &["use", s, &second]).selected.as_deref(), Some(second.as_str()));
    assert_ok(&home.bctl(&["down", s]));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n2_the_binding_survives_a_daemon_kill_but_not_a_new_generation_and_waits_for_a_person_to_release_control() {
    let home = Home::new("n2-r");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    assert_ok(&home.up(s, "n2-r"));
    let selected = listed(&home, &["new", s]).selected.expect("a new target is selected");

    let pid = home.daemon_pid();
    assert!(Command::new("kill").args(["-9", &pid]).status().unwrap().success());
    eventually("the daemon to die", || home.daemon_pid().is_empty().then_some(()));
    let reattached = listed(&home, &["list", s]);
    assert_eq!(reattached.selected.as_deref(), Some(selected.as_str()), "a restarted daemon reattaches to the same target");

    let (port, address, as_viewer) = dashboard_viewer(&home);
    eventually("the restarted daemon to offer input control", || {
        (http(port, &address, &format!("POST /api/sessions/{s}/control"), &as_viewer).0 == 200).then_some(())
    });
    assert_eq!(refused(&home, &["new", s]).code, "HUMAN_IN_CONTROL");
    assert_eq!(refused(&home, &["close", s, &selected]).code, "HUMAN_IN_CONTROL");
    let during = listed(&home, &["list", s]);
    assert_eq!(ids(&during.targets), ids(&reattached.targets), "a refused command changes no page");
    let (status, response) = http(port, &address, &format!("POST /api/sessions/{s}/control/release"), &as_viewer);
    assert_eq!(status, 200, "{response}");
    let after = listed(&home, &["new", s]);
    assert_eq!(after.targets.len(), reattached.targets.len() + 1);

    assert_ok(&home.bctl(&["down", s]));
    assert_ok(&home.up(s, "n2-r"));
    let fresh = listed(&home, &["list", s]);
    assert_eq!(fresh.selected, None, "a new generation starts with nothing selected");
    let stale = after.selected.unwrap();
    assert!(!fresh.targets.iter().any(|t| t.id == stale));
    assert_eq!(refused(&home, &["use", s, &stale]).code, "NO_SUCH_TARGET");
    assert_ok(&home.bctl(&["down", s]));
}
