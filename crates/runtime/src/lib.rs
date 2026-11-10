use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use miniserde::{Deserialize, json};

/// Readiness stages in the order an appliance reaches them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Machine,
    Guest,
    Seat,
    Compositor,
    Browser,
    Sandbox,
    Cdp,
    Display,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Machine => "machine",
            Stage::Guest => "guest",
            Stage::Seat => "seat",
            Stage::Compositor => "compositor",
            Stage::Browser => "browser",
            Stage::Sandbox => "sandbox",
            Stage::Cdp => "cdp",
            Stage::Display => "display",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSupport {
    Interactive,
    ViewOnly,
    Headless,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub input: InputSupport,
    pub browser: String,
}

/// A stable short directory name for a machine's Unix sockets.
pub fn machine_socket_key(machine: &str) -> String {
    let hash = machine.bytes().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3));
    format!("{hash:016x}")
}

/// The `Browser` field of `/json/version` on a host loopback port, if CDP answers there.
pub fn cdp_browser(port: u16) -> Option<String> {
    let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    version_browser(stream, &format!("127.0.0.1:{port}"))
}

/// The `Browser` field of `/json/version` on a Unix socket, if CDP answers there.
pub fn cdp_socket_browser(socket: &Path) -> Option<String> {
    let stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    version_browser(stream, "localhost")
}

fn version_browser(mut stream: impl Read + Write, host: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Version {
        #[serde(rename = "Browser")]
        browser: String,
    }
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: {host}\r\n\r\n").ok()?;
    let mut response = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        response.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&response);
        let (head, body) = text.split_once("\r\n\r\n")?;
        let length: usize = head
            .lines()
            .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))?
            .parse()
            .ok()?;
        if body.len() >= length {
            return json::from_str::<Version>(&body[..length]).ok().map(|v| v.browser);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::machine_socket_key;

    #[test]
    fn socket_keys_remain_stable_across_restarts() {
        assert_eq!(machine_socket_key("bctl-debug-1"), "8cdb0e09353032ca");
    }
}
