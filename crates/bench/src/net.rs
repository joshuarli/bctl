use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::host::Fallible;

pub trait Duplex: Read + Write {}
impl<T: Read + Write> Duplex for T {}

pub type Stream = Box<dyn Duplex>;

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const HANDSHAKE_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];

pub fn tcp(host: &str, port: u16) -> Fallible<Stream> {
    let stream = TcpStream::connect((host, port)).map_err(|e| format!("connect {host}:{port}: {e}"))?;
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).map_err(|e| e.to_string())?;
    Ok(Box::new(stream))
}

pub fn unix(path: &Path) -> Fallible<Stream> {
    let stream = UnixStream::connect(path).map_err(|e| format!("connect {}: {e}", path.display()))?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).map_err(|e| e.to_string())?;
    Ok(Box::new(stream))
}

pub struct Http {
    pub status: u16,
    pub headers: String,
    pub body: Vec<u8>,
}

impl Http {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn read_head(stream: &mut Stream) -> Fallible<(u16, String)> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).map_err(|e| format!("reading HTTP head: {e}"))?;
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let (status_line, headers) = head.split_once("\r\n").ok_or("empty HTTP head")?;
    let status = status_line.split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| format!("bad status line {status_line:?}"))?;
    Ok((status, headers.to_string()))
}

pub fn request(stream: &mut Stream, method: &str, host: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Fallible<Http> {
    let mut message = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
    for (key, value) in headers {
        message.push_str(&format!("{key}: {value}\r\n"));
    }
    message.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(message.as_bytes()).and_then(|()| stream.write_all(body)).map_err(|e| format!("sending {method} {path}: {e}"))?;
    let (status, headers) = read_head(stream)?;
    let mut http = Http { status, headers, body: Vec::new() };
    let length: usize = http.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    http.body = vec![0; length];
    stream.read_exact(&mut http.body).map_err(|e| format!("reading body of {method} {path}: {e}"))?;
    Ok(http)
}

pub struct WebSocket {
    stream: Stream,
    inbox: Vec<u8>,
}

impl WebSocket {
    pub fn upgrade(mut stream: Stream, host: &str, path: &str, headers: &[(&str, &str)]) -> Fallible<WebSocket> {
        let mut message = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {HANDSHAKE_KEY}\r\nSec-WebSocket-Version: 13\r\n"
        );
        for (key, value) in headers {
            message.push_str(&format!("{key}: {value}\r\n"));
        }
        message.push_str("\r\n");
        stream.write_all(message.as_bytes()).map_err(|e| format!("websocket upgrade of {path}: {e}"))?;
        let (status, _) = read_head(&mut stream)?;
        if status != 101 {
            return Err(format!("websocket upgrade of {path} answered {status}"));
        }
        Ok(WebSocket { stream, inbox: Vec::new() })
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            n if n < 126 => frame.push(0x80 | n as u8),
            n if n <= usize::from(u16::MAX) => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&MASK);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ MASK[i % 4]));
        self.stream.write_all(&frame)
    }

    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        self.send(0x1, text.as_bytes())
    }

    pub fn send_binary(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.send(0x2, bytes)
    }

    pub fn receive(&mut self) -> io::Result<Vec<u8>> {
        let mut message = Vec::new();
        loop {
            let mut head = [0u8; 2];
            self.stream.read_exact(&mut head)?;
            let (fin, opcode) = (head[0] & 0x80 != 0, head[0] & 0x0f);
            let mut length = u64::from(head[1] & 0x7f);
            if length == 126 {
                let mut extended = [0u8; 2];
                self.stream.read_exact(&mut extended)?;
                length = u64::from(u16::from_be_bytes(extended));
            } else if length == 127 {
                let mut extended = [0u8; 8];
                self.stream.read_exact(&mut extended)?;
                length = u64::from_be_bytes(extended);
            }
            let mut payload = vec![0; length as usize];
            self.stream.read_exact(&mut payload)?;
            match opcode {
                0x0..=0x2 => {
                    message.extend_from_slice(&payload);
                    if fin {
                        return Ok(message);
                    }
                }
                0x8 => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "websocket closed by peer")),
                0x9 => self.send(0xa, &payload)?,
                _ => {}
            }
        }
    }
}

impl Read for WebSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.inbox.is_empty() {
            self.inbox = self.receive()?;
        }
        let n = buf.len().min(self.inbox.len());
        buf[..n].copy_from_slice(&self.inbox[..n]);
        self.inbox.drain(..n);
        Ok(n)
    }
}

impl Write for WebSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.send_binary(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
