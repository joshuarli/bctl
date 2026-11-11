use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::layout;

const TIMEOUT: Duration = Duration::from_secs(2);

/// Asks the browser to quit through CDP `Browser.close`, the path that commits pending profile writes.
pub fn close_browser() -> io::Result<()> {
    let path = browser_path()?;
    let mut stream = connect()?;
    let key = "YmN0bC1ndWVzdC1jbG9zZQ==";
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n",
        address()
    );
    stream.write_all(request.as_bytes())?;
    let head = read_head(&mut stream)?;
    if !head.starts_with("HTTP/1.1 101") {
        return Err(io::Error::other(format!("CDP refused the websocket: {}", head.lines().next().unwrap_or(""))));
    }
    stream.write_all(&text_frame(br#"{"id":1,"method":"Browser.close"}"#))
}

fn address() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, layout::BROWSER_CDP_PORT))
}

fn connect() -> io::Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&address(), TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    Ok(stream)
}

fn browser_path() -> io::Result<String> {
    let mut stream = connect()?;
    stream.write_all(format!("GET /json/version HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", address()).as_bytes())?;
    let head = read_head(&mut stream)?;
    let length = content_length(&head).ok_or_else(|| io::Error::other("/json/version has no Content-Length"))?;
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    debugger_path(&String::from_utf8_lossy(&body)).ok_or_else(|| io::Error::other("no webSocketDebuggerUrl in /json/version"))
}

fn debugger_path(response: &str) -> Option<String> {
    let url = response.split("\"webSocketDebuggerUrl\"").nth(1)?.split('"').nth(1)?;
    let path = url.strip_prefix("ws://")?.split_once('/')?.1;
    Some(format!("/{path}"))
}

fn content_length(head: &str) -> Option<usize> {
    head.lines().find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))?.1.trim().parse().ok())
}

fn read_head(stream: &mut TcpStream) -> io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
        if head.len() > 4096 {
            return Err(io::Error::other("websocket response head is too long"));
        }
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

fn text_frame(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() < 126, "a short frame carries at most 125 bytes");
    let mask = [0x62, 0x63, 0x74, 0x6c];
    let mut frame = vec![0x81, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, byte)| byte ^ mask[i % 4]));
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_debugger_path_comes_from_the_version_reply() {
        let body = "{\"Browser\": \"Helium\", \"webSocketDebuggerUrl\": \"ws://127.0.0.1:9221/devtools/browser/abc-123\"}";
        assert_eq!(debugger_path(body).as_deref(), Some("/devtools/browser/abc-123"));
        assert_eq!(debugger_path("{}"), None);
        assert_eq!(content_length("HTTP/1.1 200 OK\r\nContent-Length: 42\r\n\r\n"), Some(42));
        assert_eq!(content_length("HTTP/1.1 200 OK\r\n\r\n"), None);
    }

    #[test]
    fn a_text_frame_is_masked_with_its_length_in_one_byte() {
        let frame = text_frame(b"ab");
        assert_eq!(frame[..2], [0x81, 0x82]);
        let mask = &frame[2..6];
        assert_eq!([frame[6] ^ mask[0], frame[7] ^ mask[1]], *b"ab");
    }
}
