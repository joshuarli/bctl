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
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
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
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
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
