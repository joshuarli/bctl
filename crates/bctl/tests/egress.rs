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
fn the_guest_reaches_the_public_internet_but_not_private_or_loopback_addresses() {
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
