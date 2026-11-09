use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::TcpStream as StdTcpStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use async_lock::Mutex as AsyncMutex;
use async_net::{TcpListener, TcpStream};
use bctl_core::SessionName;
use bctl_rfb::{ClientFilter, InputPolicy};
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture, FnExecutor};
use h12tiny::server::conn::auto;
use h12tiny::util::{BoxBody, boxed_body, bytes_body};
use h12tiny::web::{Request, Router, WebSocketFrame, WebSocketOpCode, WebSocketPayload, WebSocketUpgrade};
use http::{HeaderValue, Method, Response, StatusCode, header};
use miniserde::{Deserialize, Serialize, json};

use crate::daemon::Daemon;

const LOGIN_TOKEN_LIFETIME: Duration = Duration::from_secs(120);
const COOKIE: &str = "bctl_session";

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

#[derive(Default)]
pub struct Gateway {
    port: Option<u16>,
    login_tokens: HashMap<String, Instant>,
    sessions: HashSet<String>,
}

pub type SharedGateway = Mutex<Gateway>;

fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn executor() -> BoxExecutor {
    BoxExecutor::new(FnExecutor::new(|future: BoxSendFuture| {
        thread::spawn(move || futures_lite::future::block_on(future));
    }))
}

/// Starts the dashboard listener on first use and returns a single-use login URL.
pub fn serve(daemon: &Arc<Daemon>) -> io::Result<String> {
    let mut gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
    let port = match gateway.port {
        Some(port) => port,
        None => {
            let listener = futures_lite::future::block_on(TcpListener::bind("127.0.0.1:0"))?;
            let port = listener.local_addr()?.port();
            let daemon = Arc::clone(daemon);
            thread::spawn(move || futures_lite::future::block_on(accept(listener, daemon, port)));
            gateway.port = Some(port);
            port
        }
    };
    let token = random_token()?;
    let now = Instant::now();
    gateway.login_tokens.retain(|_, issued| now.duration_since(*issued) < LOGIN_TOKEN_LIFETIME);
    gateway.login_tokens.insert(token.clone(), now);
    Ok(format!("http://127.0.0.1:{port}/login?token={token}"))
}

async fn accept(listener: TcpListener, daemon: Arc<Daemon>, port: u16) {
    let router = Router::<()>::new().fallback(move |request: Request| {
        let daemon = Arc::clone(&daemon);
        async move { handle(&daemon, port, request) }
    });
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let router = router.clone();
        thread::spawn(move || {
            let builder = auto::Builder::new(executor()).http1_only();
            let _ = futures_lite::future::block_on(builder.serve_connection(FuturesIo::new(stream), router));
        });
    }
}

fn respond(status: StatusCode, content_type: &str, body: impl Into<bytes::Bytes>) -> Response<BoxBody> {
    let mut response = Response::new(boxed_body(bytes_body(body.into())));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(content_type).unwrap_or(HeaderValue::from_static("text/plain")));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; frame-ancestors 'none'"),
    );
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

fn text(status: StatusCode, message: &str) -> Response<BoxBody> {
    respond(status, "text/plain; charset=utf-8", message.to_string())
}

fn json_response<T: Serialize>(value: &T) -> Response<BoxBody> {
    respond(StatusCode::OK, "application/json", json::to_string(value))
}

fn header_str(request: &Request, name: header::HeaderName) -> Option<&str> {
    request.headers().get(name).and_then(|v| v.to_str().ok())
}

fn cookie<'r>(request: &'r Request, name: &str) -> Option<&'r str> {
    request
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn handle(daemon: &Arc<Daemon>, port: u16, mut request: Request) -> Response<BoxBody> {
    let path = request.uri().path().to_string();
    if request.method() == Method::GET && path == "/login" {
        return login(daemon, &request);
    }

    let authenticated = cookie(&request, COOKIE).is_some_and(|c| daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).sessions.contains(c));
    if !authenticated {
        return text(StatusCode::UNAUTHORIZED, "Not signed in. Run `bctl serve --open`.");
    }
    let origin = format!("http://127.0.0.1:{port}");
    let upgrading = request.headers().contains_key(header::UPGRADE);
    if (request.method() != Method::GET || upgrading) && header_str(&request, header::ORIGIN) != Some(origin.as_str()) {
        return text(StatusCode::FORBIDDEN, "Cross-origin request refused.");
    }

    let segments: Vec<String> = path.trim_start_matches('/').split('/').map(str::to_string).collect();
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    match (request.method().clone(), segments.as_slice()) {
        (Method::GET, ["api", "sessions"]) => json_response(&daemon.session_views()),
        (Method::GET, ["api", "sessions", name, "tabs"]) => match tabs(daemon, name) {
            Ok(tabs) => json_response(&tabs),
            Err(e) => text(StatusCode::BAD_GATEWAY, &e),
        },
        (Method::POST, ["api", "sessions", name, "tabs", id, "activate"]) => match activate(daemon, name, id) {
            Ok(()) => respond(StatusCode::NO_CONTENT, "text/plain", ""),
            Err(e) => text(StatusCode::BAD_GATEWAY, &e),
        },
        (Method::POST, ["api", "sessions", name, "down"]) => match SessionName::new(name) {
            Ok(name) => match daemon.down_session(&name) {
                Ok(()) => respond(StatusCode::NO_CONTENT, "text/plain", ""),
                Err(e) => text(StatusCode::CONFLICT, &e),
            },
            Err(e) => text(StatusCode::BAD_REQUEST, &e.to_string()),
        },
        (Method::GET, ["api", "sessions", name, "vnc"]) if upgrading => proxy_vnc(daemon, name, &mut request),
        (Method::GET, _) => match asset(&path) {
            Some((content_type, body)) => respond(StatusCode::OK, content_type, body),
            None => text(StatusCode::NOT_FOUND, "Not found."),
        },
        _ => text(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed."),
    }
}

fn login(daemon: &Daemon, request: &Request) -> Response<BoxBody> {
    let token = request.uri().query().unwrap_or("").split('&').find_map(|kv| kv.strip_prefix("token=")).unwrap_or("");
    let mut gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
    let valid = gateway.login_tokens.remove(token).is_some_and(|issued| issued.elapsed() < LOGIN_TOKEN_LIFETIME);
    if !valid {
        return text(StatusCode::UNAUTHORIZED, "This login link is invalid, used, or expired. Run `bctl serve` for a new one.");
    }
    let Ok(session) = random_token() else {
        return text(StatusCode::INTERNAL_SERVER_ERROR, "Could not create a session.");
    };
    gateway.sessions.insert(session.clone());
    let mut response = respond(StatusCode::SEE_OTHER, "text/plain", "");
    let cookie = format!("{COOKIE}={session}; HttpOnly; SameSite=Strict; Path=/");
    response.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).expect("hex cookie is a valid header"));
    let next = request
        .uri()
        .query()
        .unwrap_or("")
        .split('&')
        .find_map(|kv| kv.strip_prefix("next="))
        .filter(|n| n.strip_prefix("/sessions/").is_some_and(|s| SessionName::new(s).is_ok()))
        .unwrap_or("/");
    response.headers_mut().insert(header::LOCATION, HeaderValue::from_str(next).expect("a validated session path is a valid header"));
    response
}

/// Embedded dashboard files; any path that is not a file serves the app shell, which routes on the client.
fn asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    let lookup = |p: &str| ASSETS.iter().find(|(name, _, _)| *name == p).map(|(_, t, b)| (*t, *b));
    lookup(path).or_else(|| (!path.starts_with("/assets/")).then(|| lookup("/index.html")).flatten())
}

#[derive(Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub title: String,
    pub url: String,
}

#[derive(Deserialize)]
struct CdpTarget {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    title: String,
    url: String,
}

fn cdp_get(port: u16, path: &str) -> Result<String, String> {
    let mut stream = StdTcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("CDP: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let n = stream.read(&mut chunk).map_err(|e| format!("CDP: {e}"))?;
        response.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&response).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>()))
                .and_then(Result::ok);
            if n == 0 || length.is_some_and(|len| body.len() >= len) {
                return Ok(body.to_string());
            }
        }
        if n == 0 {
            return Err("CDP closed the connection".into());
        }
    }
}

fn session_cdp_port(daemon: &Daemon, name: &str) -> Result<u16, String> {
    let name = SessionName::new(name).map_err(|e| e.to_string())?;
    daemon.cdp_port(&name).ok_or_else(|| format!("session {name} is not running"))
}

fn tabs(daemon: &Daemon, name: &str) -> Result<Vec<Tab>, String> {
    let body = cdp_get(session_cdp_port(daemon, name)?, "/json/list")?;
    let targets: Vec<CdpTarget> = json::from_str(&body).map_err(|_| "CDP returned an unexpected target list".to_string())?;
    Ok(targets.into_iter().filter(|t| t.kind == "page").map(|t| Tab { id: t.id, title: t.title, url: t.url }).collect())
}

fn activate(daemon: &Daemon, name: &str, id: &str) -> Result<(), String> {
    if !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("invalid tab id".into());
    }
    cdp_get(session_cdp_port(daemon, name)?, &format!("/json/activate/{id}")).map(drop)
}

fn proxy_vnc(daemon: &Daemon, name: &str, request: &mut Request) -> Response<BoxBody> {
    let Ok(session) = SessionName::new(name) else {
        return text(StatusCode::BAD_REQUEST, "Invalid session name.");
    };
    let Some((vnc_port, policy)) = daemon.vnc_target(&session) else {
        return text(StatusCode::CONFLICT, "That session has no display right now.");
    };
    let upgrade = match WebSocketUpgrade::try_from_request(request) {
        Ok(upgrade) => upgrade,
        Err(e) => return text(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let response = upgrade.response();
    thread::spawn(move || {
        futures_lite::future::block_on(async move {
            let Ok(vnc) = TcpStream::connect(("127.0.0.1", vnc_port)).await else { return };
            let Ok(connection) = upgrade.into_connection().await else { return };
            relay(connection, vnc, policy).await;
        });
    });
    response
}

async fn relay(connection: h12tiny::web::WebSocketConnection, vnc: TcpStream, policy: InputPolicy) {
    let (mut reader, writer) = connection.split();
    let writer = Arc::new(AsyncMutex::new(writer));
    let (mut vnc_read, mut vnc_write) = (vnc.clone(), vnc);

    let to_viewer = {
        let writer = Arc::clone(&writer);
        async move {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = vnc_read.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let frame = WebSocketFrame::binary(WebSocketPayload::Owned(buf[..n].to_vec()));
                if writer.lock().await.write_frame(frame).await.is_err() {
                    break;
                }
            }
            let _ = writer.lock().await.write_frame(WebSocketFrame::close(1000, b"display closed")).await;
        }
    };

    let to_guest = async move {
        let mut filter = ClientFilter::new();
        let mut out = Vec::new();
        loop {
            let reply = Arc::clone(&writer);
            let mut send = move |frame| {
                let reply = Arc::clone(&reply);
                async move { reply.lock().await.write_frame(frame).await }
            };
            let frame = match reader.read_frame(&mut send).await {
                Ok(frame) => frame,
                Err(_) => break,
            };
            match frame.opcode {
                WebSocketOpCode::Binary => {}
                WebSocketOpCode::Close => break,
                _ => continue,
            }
            out.clear();
            if filter.push(&frame.payload, policy, &mut out).is_err() || vnc_write.write_all(&out).await.is_err() {
                break;
            }
        }
        let _ = vnc_write.close().await;
    };

    futures_lite::future::zip(to_viewer, to_guest).await;
}

pub fn input_policy(interactive: bool) -> InputPolicy {
    if interactive { InputPolicy::Forward } else { InputPolicy::Drop }
}
