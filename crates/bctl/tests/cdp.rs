mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use common::{Home, assert_ok};
use miniserde::json;
use shadowdriver::crawler::{ChromiumSessionOptions, CrawlerSession, TabOptions};

fn gateway_port(url: &str) -> u16 {
    url.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or_else(|| panic!("no port in {url}"))
}

fn http(port: u16, host: &str, request_line: &str, extra: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "{request_line} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{extra}\r\n").unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response.split_whitespace().nth(1).and_then(|s| s.parse().unwrap_or(0).into()).unwrap_or(0);
    (status, response)
}

const UPGRADE: &str = "Upgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n";

fn host_of(url: &str) -> &str {
    url.strip_prefix("http://").unwrap()
}

async fn evaluate(control_url: &str, expression: &str) -> Result<String, String> {
    let mut options = ChromiumSessionOptions::new(PathBuf::new(), PathBuf::new());
    options.control_url = Some(control_url.to_string());
    options.headless = false;
    options.stealth = false;
    options.identity_mode = shadowdriver::BrowserIdentityMode::BrowserNative;
    let session = CrawlerSession::ensure_chromium(options).await.map_err(|e| e.to_string())?;
    let tab = session
        .new_tab(TabOptions { preload_script: None, apply_stealth: false, viewport_width: 800, viewport_height: 600 })
        .await
        .map_err(|e| e.to_string())?;
    let params = json::from_str::<json::Value>(&format!(r#"{{"expression":{},"returnByValue":true}}"#, json::to_string(expression))).unwrap();
    let result = tab.command("Runtime.evaluate", params).await.map_err(|e| e.to_string());
    session.close(false).await;
    result.map(|value| json::to_string(&value))
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn cdp_is_reachable_only_through_a_live_capability() {
    let home = Home::new("h2");
    let session = format!("{}-s", home.prefix());
    assert_ok(&home.up(&session, "h2"));

    let minted = home.bctl(&["cdp-url", &session]);
    assert_ok(&minted);
    let url = String::from_utf8_lossy(&minted.stdout).trim().to_string();
    let port = gateway_port(&url);
    let host = host_of(&url).to_string();

    let guessed = format!("{}.localhost:{port}", "0".repeat(48));
    let (status, _) = http(port, &guessed, "GET /json/version", "");
    assert_eq!(status, 401, "a guessed capability must be refused over HTTP");
    let (status, _) = http(port, &guessed, "GET /devtools/browser/x", UPGRADE);
    assert_eq!(status, 401, "a guessed capability must be refused for the websocket");
    let (status, _) = http(port, &format!("127.0.0.1:{port}"), "GET /json/version", "");
    assert_eq!(status, 401, "the bare gateway address carries no capability");
    let (status, _) = http(port, &host, "GET /json/version", "Origin: http://evil.example\r\n");
    assert_eq!(status, 403, "a browser-originated request must be refused even with the capability");

    let (status, response) = http(port, &host, "GET /json/version", "");
    assert_eq!(status, 200, "{response}");
    let (status, _) = http(port, &host, "GET /json/version/", "");
    assert_eq!(status, 200, "Playwright asks for /json/version/ with a trailing slash");
    assert!(response.contains(&format!("ws://{host}/devtools/browser/")), "{response}");
    assert!(!response.contains("ws://127.0.0.1") && !response.contains("devtoolsFrontendUrl"), "the guest address leaked: {response}");

    let browser_path = response.split("ws://").nth(1).and_then(|r| r.find('/').map(|i| r[i..].split('"').next().unwrap().to_string())).unwrap();
    let mut live = TcpStream::connect(("127.0.0.1", port)).unwrap();
    live.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(live, "GET {browser_path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\n{UPGRADE}\r\n").unwrap();
    let mut head = [0u8; 12];
    live.read_exact(&mut head).unwrap();
    assert!(head.starts_with(b"HTTP/1.1 101"), "{}", String::from_utf8_lossy(&head));

    assert_ok(&home.bctl(&["cdp-revoke", &session]));
    let mut rest = Vec::new();
    let closed = live.read_to_end(&mut rest);
    assert!(
        closed.is_ok() || closed.as_ref().is_err_and(|e| !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
        "revoking must close live DevTools connections: {closed:?}"
    );
    let (status, _) = http(port, &host, "GET /json/version", "");
    assert_eq!(status, 401, "a revoked capability must fail over HTTP");
    let (status, _) = http(port, &host, "GET /devtools/browser/x", UPGRADE);
    assert_eq!(status, 401, "a revoked capability must fail for the websocket");
    assert!(async_io::block_on(evaluate(&url, "1")).is_err(), "a revoked capability must not attach");

    let reminted = home.bctl(&["cdp-url", &session]);
    assert_ok(&reminted);
    let fresh = String::from_utf8_lossy(&reminted.stdout).trim().to_string();
    assert_ne!(fresh, url);
    let result = async_io::block_on(evaluate(&fresh, "6 * 7")).expect("a client with the capability attaches");
    assert!(result.contains("42"), "{result}");
    assert_ok(&home.bctl(&["down", &session]));
    let (status, _) = http(port, host_of(&fresh), "GET /json/version", "");
    assert_eq!(status, 401, "stopping a session revokes its capabilities");
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn a_person_holding_the_input_lease_blocks_new_cdp_attachments() {
    let home = Home::new("h2l");
    let session = format!("{}-s", home.prefix());
    assert_ok(&home.up(&session, "h2l"));
    let minted = home.bctl(&["cdp-url", &session]);
    assert_ok(&minted);
    let url = String::from_utf8_lossy(&minted.stdout).trim().to_string();
    let port = gateway_port(&url);
    let host = host_of(&url).to_string();

    let (status, response) = http(port, &host, "GET /json/version", "");
    assert_eq!(status, 200, "{response}");
    let browser_path = response.split("ws://").nth(1).and_then(|r| r.find('/').map(|i| r[i..].split('"').next().unwrap().to_string())).unwrap();

    let served = home.bctl(&["serve"]);
    assert_ok(&served);
    let login = String::from_utf8_lossy(&served.stdout).trim().to_string();
    let login_path = login.split_once(&format!("127.0.0.1:{port}")).unwrap().1;
    let dashboard = format!("127.0.0.1:{port}");
    let (status, response) = http(port, &dashboard, &format!("GET {login_path}"), "");
    assert_eq!(status, 303, "{response}");
    let cookie = response.lines().find_map(|l| l.strip_prefix("set-cookie: ")).unwrap().split(';').next().unwrap().to_string();
    let as_viewer = format!("Cookie: {cookie}\r\nOrigin: http://{dashboard}\r\nContent-Length: 0\r\n");

    let (status, response) = http(port, &dashboard, &format!("POST /api/sessions/{session}/control"), &as_viewer);
    if common::nanos() {
        assert_eq!(status, 409, "a headless session refuses a takeover: {response}");
        assert!(response.contains("no input devices"), "{response}");
        let (status, response) = http(port, &host, "GET /json/version", "");
        assert_eq!(status, 200, "a refused takeover leaves CDP attachments open: {response}");
        return;
    }
    assert_eq!(status, 200, "{response}");
    let (status, response) = http(port, &host, "GET /json/version", "");
    assert_eq!(status, 409, "{response}");
    let (status, _) = http(port, &host, "GET /json/list", "");
    assert_eq!(status, 409);
    let (status, _) = http(port, &host, &format!("GET {browser_path}"), UPGRADE);
    assert_eq!(status, 409, "a websocket attach must be refused while a person is in control");

    let (status, response) = http(port, &dashboard, &format!("POST /api/sessions/{session}/control/resume"), &as_viewer);
    assert_eq!(status, 200, "{response}");
    let (status, response) = http(port, &host, "GET /json/version", "");
    assert_eq!(status, 200, "{response}");
}

/// Opens a CDP WebSocket through the gateway and keeps it open; returns the HTTP status and the socket.
fn open_socket(port: u16, host: &str, path: &str) -> (u16, TcpStream) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\n{UPGRADE}\r\n").unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
        head.push(byte[0]);
    }
    let status = String::from_utf8_lossy(&head).split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, stream)
}

/// Whether the gateway closed the socket within `wait`.
fn closed_within(stream: &mut TcpStream, wait: Duration) -> bool {
    stream.set_read_timeout(Some(wait)).unwrap();
    let mut buf = [0u8; 256];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return true,
            Ok(_) => continue,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return false,
            Err(_) => return true,
        }
    }
}

fn eventually_status(port: u16, host: &str, request_line: &str, extra: &str, want: u16) -> (u16, String) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (status, response) = http(port, host, request_line, extra);
        if status == want || std::time::Instant::now() > deadline {
            return (status, response);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn e2_one_controller_holds_the_session_across_its_sockets_until_the_last_closes() {
    let home = Home::new("e2c");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    assert_ok(&home.up(s, "e2c"));
    let generation = common::status_of(&home.json(&["inspect", s]), s).generation;
    let interactive = common::status_of(&home.json(&["inspect", s]), s).vnc_port.is_some();
    let (a, b) = (home.cdp_url(s), home.cdp_url(s));
    let port = gateway_port(&a);
    let (status, version) = http(port, host_of(&a), "GET /json/version", "");
    assert_eq!(status, 200, "{version}");
    let browser_path = version.split("ws://").nth(1).and_then(|r| r.find('/').map(|i| r[i..].split('"').next().unwrap().to_string())).unwrap();
    let (dashboard_port, dashboard, as_viewer) = common::dashboard_viewer(&home);
    let take = || http(dashboard_port, &dashboard, &format!("POST /api/sessions/{s}/control"), &as_viewer);

    let (first, first_socket) = open_socket(port, host_of(&a), &browser_path);
    let (second, second_socket) = open_socket(port, host_of(&a), &browser_path);
    assert_eq!((first, second), (101, 101), "two sockets through one CDP URL belong to one controller");
    let (status, refusal) = http(port, host_of(&b), "GET /json/version", "");
    assert_eq!(status, 409, "another CDP URL is a competing controller: {refusal}");
    assert!(refusal.contains("Another CDP controller is attached"), "{refusal}");
    assert_eq!(open_socket(port, host_of(&b), &browser_path).0, 409, "a competing websocket attach is refused");

    let (status, refusal) = take();
    assert_eq!(status, 409, "{refusal}");
    if interactive {
        assert!(refusal.contains("external CDP controller is attached (2 open connections)"), "{refusal}");
    }
    assert_ok(&home.bctl(&["tab", "new", s]));

    drop(second_socket);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(http(port, host_of(&b), "GET /json/version", "").0, 409, "one socket still holds the session");
    drop(first_socket);
    let (status, response) = eventually_status(port, host_of(&b), "GET /json/version", "", 200);
    assert_eq!(status, 200, "the controller released the session when its last socket closed: {response}");

    let (status, mut held) = open_socket(port, host_of(&b), &browser_path);
    assert_eq!(status, 101);
    let revoked = home.bctl(&["cdp-revoke", s]);
    assert_ok(&revoked);
    assert!(closed_within(&mut held, Duration::from_secs(3)), "revocation closes the controller's open socket");
    let c = home.cdp_url(s);
    let (status, response) = eventually_status(port, host_of(&c), "GET /json/version", "", 200);
    assert_eq!(status, 200, "a revoked controller no longer holds the session: {response}");
    if interactive {
        let (status, response) = take();
        assert_eq!(status, 200, "a person can take control once no controller is attached: {response}");
        let (status, response) = http(dashboard_port, &dashboard, &format!("POST /api/sessions/{s}/control/resume"), &as_viewer);
        assert_eq!(status, 200, "{response}");
    }

    let (status, _held) = open_socket(port, host_of(&c), &browser_path);
    assert_eq!(status, 101);
    let daemon = home.daemon_pid();
    assert!(std::process::Command::new("kill").args(["-9", &daemon]).status().unwrap().success());
    let after = common::status_of(&home.json(&["inspect", s]), s).clone();
    assert_eq!((after.state.as_str(), after.generation), ("ready", generation), "the browser outlives the controller and the daemon");
    let d = home.cdp_url(s);
    let restarted = gateway_port(&d);
    let (status, response) = eventually_status(restarted, host_of(&d), "GET /json/version", "", 200);
    assert_eq!(status, 200, "no attachment survives a daemon restart: {response}");
    assert_eq!(open_socket(restarted, host_of(&d), &browser_path).0, 101);
    assert_ok(&home.bctl(&["tab", "list", s]));
    assert_ok(&home.bctl(&["down", s]));
}
