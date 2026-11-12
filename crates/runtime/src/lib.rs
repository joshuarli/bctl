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
/// Where the guest sees a session's files directory, which is also where the browser saves downloads.
pub const GUEST_FILES_DIR: &str = "/home/browser/Downloads";

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

const MAX_VERSION_HEAD: usize = 16 * 1024;
const MAX_VERSION_BODY: usize = 64 * 1024;

fn version_browser(mut stream: impl Read + Write, host: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Version {
        #[serde(rename = "Browser")]
        browser: String,
    }
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: {host}\r\n\r\n").ok()?;
    let mut response = Vec::new();
    let mut chunk = [0; 4096];
    let mut body: Option<(usize, usize)> = None;
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        response.extend_from_slice(&chunk[..n]);
        if body.is_none() {
            match response.windows(4).position(|window| window == b"\r\n\r\n") {
                Some(end) => {
                    let head = String::from_utf8_lossy(&response[..end]);
                    let length: usize = head
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))?
                        .parse()
                        .ok()?;
                    if length > MAX_VERSION_BODY {
                        return None;
                    }
                    body = Some((end + 4, length));
                }
                None if response.len() > MAX_VERSION_HEAD => return None,
                None => {}
            }
        }
        if let Some((start, length)) = body
            && response.len() >= start + length
        {
            let text = std::str::from_utf8(&response[start..start + length]).ok()?;
            return json::from_str::<Version>(text).ok().map(|v| v.browser);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{self, Read, Write};

    use super::{machine_socket_key, version_browser};

    struct Chunked(VecDeque<Vec<u8>>);

    impl Read for Chunked {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(mut next) = self.0.pop_front() else { return Ok(0) };
            let n = next.len().min(buf.len());
            buf[..n].copy_from_slice(&next[..n]);
            if n < next.len() {
                self.0.push_front(next.split_off(n));
            }
            Ok(n)
        }
    }

    impl Write for Chunked {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn response() -> Vec<u8> {
        let body = r#"{"Browser":"Chrome/156.0.1","Protocol-Version":"1.3","webSocketDebuggerUrl":"ws://localhost/devtools/browser/x"}"#;
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=UTF-8\r\ncontent-length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn socket_keys_remain_stable_across_restarts() {
        assert_eq!(machine_socket_key("bctl-debug-1"), "8cdb0e09353032ca");
    }

    #[test]
    fn a_version_response_split_at_every_byte_still_yields_the_browser() {
        let bytes = response();
        for split in 1..bytes.len() {
            let stream = Chunked(VecDeque::from([bytes[..split].to_vec(), bytes[split..].to_vec()]));
            assert_eq!(version_browser(stream, "localhost").as_deref(), Some("Chrome/156.0.1"), "split at byte {split}");
        }
        let stream = Chunked(bytes.iter().map(|b| vec![*b]).collect());
        assert_eq!(version_browser(stream, "localhost").as_deref(), Some("Chrome/156.0.1"), "one byte per read");
    }

    #[test]
    fn a_truncated_or_oversized_version_response_is_refused() {
        let bytes = response();
        let truncated = Chunked(VecDeque::from([bytes[..bytes.len() - 1].to_vec()]));
        assert_eq!(version_browser(truncated, "localhost"), None);
        let endless = Chunked(VecDeque::from([vec![b'x'; 1 << 20]]));
        assert_eq!(version_browser(endless, "localhost"), None);
        let huge = Chunked(VecDeque::from([b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n{".to_vec()]));
        assert_eq!(version_browser(huge, "localhost"), None);
    }
}
