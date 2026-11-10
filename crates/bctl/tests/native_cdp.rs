mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use bctl_cdp::protocol::{AttachToTarget, CdpEvent, DetachFromTarget, DetachedFromTarget, GetTargets, GetVersion};
use bctl_cdp::{Endpoint, EventFilter, EventScope, Generation, Scope, Termination};
use common::{Home, assert_ok, status_of};
use futures_lite::future::block_on;
use miniserde::{Deserialize, json};

#[derive(Deserialize)]
struct Version {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

fn browser_endpoint(control_url: &str) -> Endpoint {
    let host = control_url.strip_prefix("http://").unwrap();
    let port: u16 = host.rsplit(':').next().unwrap().parse().unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or_else(|| panic!("{response}"));
    Endpoint::parse(&json::from_str::<Version>(body).unwrap_or_else(|_| panic!("{response}")).web_socket_debugger_url).unwrap()
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn the_cdp_transport_drives_a_ready_session() {
    let home = Home::new("n1-");
    let session = format!("{}-s", home.prefix());
    assert_ok(&home.up(&session, "n1-p"));
    let generation = status_of(&home.json(&["status"]), &session).generation;
    let minted = home.bctl(&["cdp-url", &session]);
    assert_ok(&minted);
    let endpoint = browser_endpoint(String::from_utf8_lossy(&minted.stdout).trim());

    let (connection, driver) = block_on(bctl_cdp::connect(&endpoint, Generation::new(generation))).unwrap();
    let driving = std::thread::spawn(move || block_on(driver));
    let deadline = || Instant::now() + Duration::from_secs(10);
    let version = block_on(connection.call(Scope::Browser, &GetVersion {}, deadline())).unwrap();
    assert!(version.product.starts_with("Helium/") || version.product.starts_with("Chrome/"), "{}", version.product);
    let targets = block_on(connection.call(Scope::Browser, &GetTargets {}, deadline())).unwrap();
    let page = targets.target_infos.into_iter().find(|t| t.kind == "page").expect("a ready session shows a page");

    let mut detached = connection.subscribe(EventFilter::new(&[DetachedFromTarget::METHOD], EventScope::Browser)).unwrap();
    let attached = block_on(connection.call(Scope::Browser, &AttachToTarget::new(page.target_id.clone()), deadline())).unwrap();
    block_on(connection.call(Scope::Browser, &DetachFromTarget { session_id: attached.session_id.clone() }, deadline())).unwrap();
    let event = block_on(detached.next()).unwrap().decode::<DetachedFromTarget>().unwrap();
    assert_eq!(event.session_id, attached.session_id);

    assert_eq!(block_on(connection.shutdown()), Termination::Shutdown);
    assert_eq!(driving.join().unwrap(), Termination::Shutdown);
    assert_ok(&home.bctl(&["down", &session]));
}
