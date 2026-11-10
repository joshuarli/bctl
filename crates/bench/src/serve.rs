use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;

use crate::host::Fallible;

pub struct Server {
    pub port: u16,
}

pub fn start(speedometer: PathBuf) -> Fallible<Server> {
    let listener = TcpListener::bind("0.0.0.0:0").map_err(|e| format!("cannot listen: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let speedometer = speedometer.canonicalize().map_err(|e| format!("{}: {e}", speedometer.display()))?;
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let speedometer = speedometer.clone();
            thread::spawn(move || {
                if let Err(e) = serve(stream, &speedometer) {
                    eprintln!("bctl-bench serve: connection ended with {:?}: {e}", e.kind());
                }
            });
        }
    });
    Ok(Server { port })
}

struct Reply {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

fn serve(stream: TcpStream, speedometer: &Path) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let target = line.split(' ').nth(1).unwrap_or("/").to_string();
        let mut close = false;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            if header.trim().is_empty() {
                break;
            }
            close |= header.to_ascii_lowercase().starts_with("connection: close");
        }
        let path = target.split(['?', '#']).next().unwrap_or("/");
        let reply = route(path, speedometer);
        if reply.status != "200 OK" {
            eprintln!("bctl-bench serve: {} {path}", reply.status);
        }
        write!(
            writer,
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n",
            reply.status,
            reply.content_type,
            reply.body.len()
        )?;
        writer.write_all(&reply.body)?;
        if close {
            return Ok(());
        }
    }
}

fn not_found() -> Reply {
    Reply { status: "404 Not Found", content_type: "text/plain", body: b"not found".to_vec() }
}

fn route(path: &str, root: &Path) -> Reply {
    let Some(rest) = path.strip_prefix("/speedometer/") else { return not_found() };
    let rest = if rest.is_empty() || rest.ends_with('/') { format!("{rest}index.html") } else { rest.to_string() };
    match root.join(rest).canonicalize() {
        Ok(file) if file.starts_with(root) && file.is_file() => match std::fs::read(&file) {
            Ok(body) => Reply { status: "200 OK", content_type: content_type(&file), body },
            Err(_) => not_found(),
        },
        _ => not_found(),
    }
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html",
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}
