use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;

use crate::host::Fallible;

const FIXTURE_SCRIPT_BYTES: usize = 300 * 1024;
const FIXTURE_STYLE_BYTES: usize = 60 * 1024;
const FIXTURE_IMAGES: usize = 12;
const FIXTURE_IMAGE_BYTES: usize = 20 * 1024;

pub struct Server {
    pub port: u16,
}

pub fn start(speedometer: Option<PathBuf>) -> Fallible<Server> {
    let listener = TcpListener::bind("0.0.0.0:0").map_err(|e| format!("cannot listen: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let speedometer = speedometer.map(|dir| dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))).transpose()?;
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let speedometer = speedometer.clone();
            thread::spawn(move || {
                let _ = serve(stream, speedometer.as_deref());
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

fn serve(stream: TcpStream, speedometer: Option<&Path>) -> std::io::Result<()> {
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

fn route(path: &str, speedometer: Option<&Path>) -> Reply {
    if let Some(rest) = path.strip_prefix("/speedometer/") {
        let Some(root) = speedometer else { return not_found() };
        let rest = if rest.is_empty() || rest.ends_with('/') { format!("{rest}index.html") } else { rest.to_string() };
        return match root.join(rest).canonicalize() {
            Ok(file) if file.starts_with(root) && file.is_file() => match std::fs::read(&file) {
                Ok(body) => Reply { status: "200 OK", content_type: content_type(&file), body },
                Err(_) => not_found(),
            },
            _ => not_found(),
        };
    }
    match path {
        "/fixture/page.html" => Reply { status: "200 OK", content_type: "text/html", body: fixture_page().into_bytes() },
        "/fixture/app.js" => Reply { status: "200 OK", content_type: "text/javascript", body: padded(FIXTURE_SCRIPT_BYTES, "const rows = document.getElementById('rows'); for (let i = 0; i < 2000; i++) { const row = document.createElement('div'); row.className = 'row'; row.textContent = 'row ' + i; rows.appendChild(row); }\n") },
        "/fixture/style.css" => Reply { status: "200 OK", content_type: "text/css", body: padded(FIXTURE_STYLE_BYTES, ".row { padding: 2px 8px; border-bottom: 1px solid #ddd; font: 14px sans-serif; }\n") },
        _ if path.starts_with("/fixture/image-") => Reply { status: "200 OK", content_type: "image/svg+xml", body: image(path) },
        _ => not_found(),
    }
}

fn fixture_page() -> String {
    let images: String = (0..FIXTURE_IMAGES).map(|i| format!("<img src=\"/fixture/image-{i}.svg\" width=\"80\" height=\"60\">")).collect();
    format!("<!doctype html><meta charset=\"utf-8\"><title>bctl-bench fixture</title><link rel=\"stylesheet\" href=\"/fixture/style.css\"><div id=\"rows\"></div>{images}<script src=\"/fixture/app.js\"></script>")
}

fn padded(bytes: usize, unit: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(bytes + unit.len());
    let mut n = 0;
    while body.len() < bytes {
        body.extend_from_slice(format!("/* padding {n} */\n").as_bytes());
        n += 1;
    }
    body.extend_from_slice(unit.as_bytes());
    body
}

fn image(path: &str) -> Vec<u8> {
    let mut svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"80\" height=\"60\"><!-- {path} -->");
    let mut n = 0;
    while svg.len() < FIXTURE_IMAGE_BYTES {
        svg.push_str(&format!("<rect x=\"{}\" y=\"{}\" width=\"4\" height=\"4\" fill=\"#{:06x}\"/>", n % 76, (n / 3) % 56, n * 2654435761u64 as usize % 0xffffff));
        n += 1;
    }
    svg.push_str("</svg>");
    svg.into_bytes()
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
