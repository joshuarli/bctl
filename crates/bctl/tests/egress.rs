mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, ToSocketAddrs, UdpSocket};
use std::process::Command;
use std::time::Duration;

use common::{Home, assert_ok, status_of};

fn host_private_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if ip.is_private() => Some(ip),
        _ => None,
    }
}

fn host_reaches_internet() -> bool {
    "example.com:80".to_socket_addrs().ok().and_then(|mut a| a.next()).is_some_and(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_secs(5)).is_ok())
}

fn guest_connects(home: &Home, machine: &str, command: &str) -> bool {
    let output = Command::new(home.path.join("runtime/smolvm/smolvm"))
        .args(["machine", "exec", "--name", machine, "--", "sh", "-c", &format!("{command} >/dev/null 2>&1; echo rc=$?")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("rc="), "machine exec failed: {stdout}{}", String::from_utf8_lossy(&output.stderr));
    stdout.contains("rc=0")
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn the_guest_network_obeys_the_runtime_egress_boundary() {
    if common::nanos() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = Command::new("python3")
            .arg(root.join("tests/runtime-nanos/network-isolation.py"))
            .env("BCTL_BIN", env!("CARGO_BIN_EXE_bctl"))
            .output()
            .unwrap();
        assert_ok(&output);
        return;
    }
    let home = Home::new("h2e");
    let session = format!("{}-s", home.prefix());
    assert_ok(&home.up(&session, "h2e"));
    let machine = status_of(&home.json(&["status"]), &session).machine.clone().unwrap();

    assert!(guest_connects(&home, &machine, "command -v nc"), "the guest image has no nc to probe with");
    let loopback = TcpListener::bind("127.0.0.1:0").unwrap();
    let loopback_port = loopback.local_addr().unwrap().port();
    assert!(!guest_connects(&home, &machine, &format!("echo | nc -w 3 127.0.0.1 {loopback_port}")), "the guest reached a host loopback service");

    let lan = TcpListener::bind("0.0.0.0:0").unwrap();
    let lan_port = lan.local_addr().unwrap().port();
    match host_private_address() {
        Some(ip) => {
            assert!(TcpListener::bind(SocketAddr::from((ip, 0))).is_ok());
            assert!(!guest_connects(&home, &machine, &format!("echo | nc -w 3 {ip} {lan_port}")), "the guest reached the host at {ip}");
        }
        None => eprintln!("skipped: the host has no RFC 1918 address to listen on"),
    }
    for private in ["10.0.0.1", "172.16.0.1", "192.168.1.1", "169.254.169.254"] {
        assert!(!guest_connects(&home, &machine, &format!("echo | nc -w 3 {private} 80")), "the guest connected to {private}");
    }

    if host_reaches_internet() {
        assert!(guest_connects(&home, &machine, "wget -qO- -T 10 http://example.com"), "the guest cannot reach the public internet");
    } else {
        eprintln!("skipped: the host has no internet");
    }
}

fn goto(home: &Home, session: &str, url: &str) -> Result<(), String> {
    for _ in 0..5 {
        let output = home.bctl(&["page", "goto", session, url, "--json"]);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            return Ok(());
        }
        if !stdout.contains("ERR_ABORTED") {
            return Err(stdout);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Err(format!("{url} kept aborting"))
}

fn count_requests(listener: TcpListener) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut stream = stream;
            let _ = std::io::Write::write_all(&mut stream, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    hits
}

#[test]
#[ignore = "boots macOS Nanos VMs to check browser loopback proxy routing"]
fn the_nanos_browser_obeys_explicit_host_loopback_permissions() {
    if !common::nanos() || !cfg!(target_os = "macos") { return; }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = count_requests(listener);
    let home = Home::new("h2-loopback");
    let denied = format!("{}-denied", home.prefix());
    assert_ok(&home.up(&denied, "denied"));
    assert_ok(&home.bctl(&["tab", "new", &denied, "about:blank"]));
    assert!(goto(&home, &denied, &format!("http://127.0.0.1:{port}/")).is_err());
    assert_ok(&home.bctl(&["down", &denied]));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);

    let allowed = format!("{}-allowed", home.prefix());
    assert_ok(&home.bctl(&["up", &allowed, "--profile", "allowed", "--image", &common::image(), "--allow-private", "127.0.0.1/32"]));
    assert_ok(&home.bctl(&["tab", "new", &allowed, "about:blank"]));
    let ipv4 = goto(&home, &allowed, &format!("http://127.0.0.1:{port}/"));
    let mapped = goto(&home, &allowed, &format!("http://[::ffff:127.0.0.1]:{port}/"));
    assert_ok(&home.bctl(&["down", &allowed]));
    assert!(ipv4.is_ok(), "explicit IPv4 permission was not routed through egress: {ipv4:?}");
    assert!(mapped.is_ok(), "mapped IPv4 permission was not routed through egress: {mapped:?}");
    assert!(hits.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}

#[test]
#[ignore = "boots real VMs: needs the runtime and the BCTL_TEST_IMAGE image (default for the runtime) in .work/images"]
fn the_browser_reaches_the_public_internet_but_not_the_host_or_private_addresses() {
    let home = Home::new("h2b");
    let session = format!("{}-s", home.prefix());
    assert_ok(&home.up(&session, "h2b"));
    assert_ok(&home.bctl(&["tab", "new", &session, "about:blank"]));

    let loopback = TcpListener::bind("127.0.0.1:0").unwrap();
    let loopback_port = loopback.local_addr().unwrap().port();
    let loopback_hits = count_requests(loopback);
    assert!(goto(&home, &session, &format!("http://100.96.0.1:{loopback_port}/")).is_err(), "the browser reached host loopback without an allowed range");

    let lan = TcpListener::bind("0.0.0.0:0").unwrap();
    let lan_port = lan.local_addr().unwrap().port();
    let lan_hits = count_requests(lan);
    match host_private_address() {
        Some(ip) => assert!(goto(&home, &session, &format!("http://{ip}:{lan_port}/")).is_err(), "the browser reached the host at {ip}"),
        None => eprintln!("skipped: the host has no RFC 1918 address to listen on"),
    }
    for private in ["10.0.0.1", "172.16.0.1", "192.168.1.1", "169.254.169.254", "[fd00::1]"] {
        assert!(goto(&home, &session, &format!("http://{private}/")).is_err(), "the browser reached {private}");
    }
    assert_eq!(loopback_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "a request reached the host loopback service");
    assert_eq!(lan_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "a request reached the host LAN service");

    if host_reaches_internet() {
        goto(&home, &session, "http://example.com/").unwrap_or_else(|error| panic!("the browser cannot reach the public internet: {error}"));
    } else {
        eprintln!("skipped: the host has no internet");
    }
}
