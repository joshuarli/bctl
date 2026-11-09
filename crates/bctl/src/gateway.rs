use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use async_lock::Mutex as AsyncMutex;
use async_net::{TcpListener, TcpStream};
use bctl_core::SessionName;
use bctl_rfb::{ClientFilter, InputPolicy};
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use h12tiny::io::HyperIo;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture, FnExecutor};
use h12tiny::server::conn::auto;
use h12tiny::util::{BoxBody, boxed_body, bytes_body};
use h12tiny::web::{Request, Router, WebSocketFrame, WebSocketOpCode, WebSocketPayload, WebSocketUpgrade};
use http::{HeaderValue, Method, Response, StatusCode, header};
use miniserde::{Deserialize, Serialize, json};

use crate::daemon::Daemon;

mod runs;

const LOGIN_TOKEN_LIFETIME: Duration = Duration::from_secs(120);
const COOKIE: &str = "bctl_session";

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

#[derive(Default)]
pub struct Gateway {
    port: Option<u16>,
    login_tokens: HashMap<String, Instant>,
    sessions: HashSet<String>,
    /// The dashboard session holding each browser session's input lease.
    controllers: HashMap<SessionName, String>,
    /// Unguessable CDP capabilities, each bound to one browser session until revoked.
    capabilities: Vec<(String, SessionName)>,
}

impl Gateway {
    fn capability_session(&self, token: &str) -> Option<SessionName> {
        let mut found = None;
        for (candidate, session) in &self.capabilities {
            if constant_time_eq(candidate.as_bytes(), token.as_bytes()) {
                found = Some(session.clone());
            }
        }
        found
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

pub type SharedGateway = Mutex<Gateway>;

fn random_hex<const N: usize>() -> io::Result<String> {
    let mut bytes = [0u8; N];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn random_token() -> io::Result<String> {
    random_hex::<32>()
}

fn executor() -> BoxExecutor {
    BoxExecutor::new(FnExecutor::new(|future: BoxSendFuture| {
        thread::spawn(move || futures_lite::future::block_on(future));
    }))
}

fn listener_port(daemon: &Arc<Daemon>, gateway: &mut Gateway) -> io::Result<u16> {
    if let Some(port) = gateway.port {
        return Ok(port);
    }
    let listener = futures_lite::future::block_on(TcpListener::bind("127.0.0.1:0"))?;
    let port = listener.local_addr()?.port();
    let daemon = Arc::clone(daemon);
    thread::spawn(move || futures_lite::future::block_on(accept(listener, daemon, port)));
    gateway.port = Some(port);
    Ok(port)
}

/// Starts the dashboard listener on first use and returns a single-use login URL.
pub fn serve(daemon: &Arc<Daemon>) -> io::Result<String> {
    let mut gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
    let port = listener_port(daemon, &mut gateway)?;
    let token = random_token()?;
    let now = Instant::now();
    gateway.login_tokens.retain(|_, issued| now.duration_since(*issued) < LOGIN_TOKEN_LIFETIME);
    gateway.login_tokens.insert(token.clone(), now);
    Ok(format!("http://127.0.0.1:{port}/login?token={token}"))
}

/// Mints a CDP control URL for a session. The capability is the subdomain, so clients that
/// only keep a URL's host and port (Playwright, Puppeteer, Shadowdriver) still present it.
pub fn mint_cdp_url(daemon: &Arc<Daemon>, session: &SessionName) -> io::Result<String> {
    let mut gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
    let port = listener_port(daemon, &mut gateway)?;
    let token = random_hex::<24>()?;
    gateway.capabilities.push((token.clone(), session.clone()));
    Ok(format!("http://{token}.localhost:{port}"))
}

pub fn revoke_cdp_url(daemon: &Daemon, url: &str) {
    let Some(token) = url.strip_prefix("http://").and_then(|host| host.split_once('.')).map(|(token, _)| token) else { return };
    daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).capabilities.retain(|(candidate, _)| !constant_time_eq(candidate.as_bytes(), token.as_bytes()));
}

pub fn revoke_cdp(daemon: &Daemon, session: &SessionName) {
    daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).capabilities.retain(|(_, bound)| bound != session);
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
    if let Some(host) = header_str(&request, header::HOST).map(str::to_string)
        && let Some(token) = host.strip_suffix(&format!(".localhost:{port}"))
    {
        return handle_cdp(daemon, &host, token, &mut request);
    }
    let path = request.uri().path().to_string();
    if request.method() == Method::GET && path == "/login" {
        return login(daemon, &request);
    }

    let viewer = cookie(&request, COOKIE).map(str::to_string).unwrap_or_default();
    let authenticated = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).sessions.contains(&viewer);
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
        (Method::GET, ["api", "sessions", name, "runs"]) => runs::session_runs(daemon, name),
        (Method::GET, ["api", "runs", id]) => runs::run(daemon, id),
        (Method::GET, ["api", "runs", id, "log"]) => runs::log(daemon, id, request.uri().query().unwrap_or("")),
        (Method::GET, ["api", "runs", id, "artifacts"]) => runs::artifacts(daemon, id),
        (Method::GET, ["api", "runs", id, "artifacts", rest @ ..]) => runs::download(daemon, id, rest),
        (Method::POST, ["api", "runs", id, "resume"]) => runs::resume(daemon, id, &viewer),
        (Method::GET, ["api", "sessions", name, "control"]) => control(daemon, name, &viewer, ControlRequest::Inspect),
        (Method::POST, ["api", "sessions", name, "control"]) => control(daemon, name, &viewer, ControlRequest::Acquire),
        (Method::POST, ["api", "sessions", name, "control", "release"]) => control(daemon, name, &viewer, ControlRequest::Release),
        (Method::POST, ["api", "sessions", name, "down"]) => match SessionName::new(name) {
            Ok(name) => match stop_session(daemon, &name) {
                Ok(()) => respond(StatusCode::NO_CONTENT, "text/plain", ""),
                Err(e) => text(StatusCode::CONFLICT, &e),
            },
            Err(e) => text(StatusCode::BAD_REQUEST, &e.to_string()),
        },
        (Method::GET, ["api", "sessions", name, "vnc"]) if upgrading => proxy_vnc(daemon, name, &mut request, viewer),
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

pub(crate) fn cdp_get(socket: &Path, path: &str) -> Result<String, String> {
    let mut stream = StdUnixStream::connect(socket).map_err(|e| format!("CDP: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
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

fn session_cdp_socket(daemon: &Daemon, name: &str) -> Result<PathBuf, String> {
    let name = SessionName::new(name).map_err(|e| e.to_string())?;
    daemon.cdp_socket(&name).ok_or_else(|| format!("session {name} is not running"))
}

fn tabs(daemon: &Daemon, name: &str) -> Result<Vec<Tab>, String> {
    let body = cdp_get(&session_cdp_socket(daemon, name)?, "/json/list")?;
    let targets: Vec<CdpTarget> = json::from_str(&body).map_err(|_| "CDP returned an unexpected target list".to_string())?;
    Ok(targets.into_iter().filter(|t| t.kind == "page").map(|t| Tab { id: t.id, title: t.title, url: t.url }).collect())
}

fn activate(daemon: &Daemon, name: &str, id: &str) -> Result<(), String> {
    if !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("invalid tab id".into());
    }
    cdp_get(&session_cdp_socket(daemon, name)?, &format!("/json/activate/{id}")).map(drop)
}

fn handle_cdp(daemon: &Arc<Daemon>, host: &str, token: &str, request: &mut Request) -> Response<BoxBody> {
    if request.headers().contains_key(header::ORIGIN) {
        return text(StatusCode::FORBIDDEN, "Browser-originated CDP requests are refused.");
    }
    let (session, person_in_control) = {
        let gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
        let session = gateway.capability_session(token);
        let held = session.as_ref().is_some_and(|session| gateway.controllers.contains_key(session));
        (session, held)
    };
    let Some(session) = session else {
        return text(StatusCode::UNAUTHORIZED, "This CDP URL is invalid or revoked. Run `bctl cdp-url SESSION`.");
    };
    if person_in_control {
        return text(StatusCode::CONFLICT, "A person has taken control of this browser; new CDP attachments are refused until they release it.");
    }
    let Some(cdp_socket) = daemon.cdp_socket(&session) else {
        return text(StatusCode::CONFLICT, "That session is not running.");
    };
    let path = request.uri().path().trim_end_matches('/').to_string();
    let upgrading = request.headers().contains_key(header::UPGRADE);
    match (request.method(), path.as_str()) {
        (&Method::GET, "/json/version" | "/json/list" | "/json") if !upgrading => {
            match cdp_get(&cdp_socket, &path).and_then(|body| rewrite_targets(&body, host)) {
                Ok(body) => respond(StatusCode::OK, "application/json", body),
                Err(e) => text(StatusCode::BAD_GATEWAY, &e),
            }
        }
        (&Method::GET, _) if upgrading && is_devtools_path(&path) => proxy_cdp_socket(daemon, token, &cdp_socket, &path, request),
        _ => text(StatusCode::NOT_FOUND, "Not found."),
    }
}

fn is_devtools_path(path: &str) -> bool {
    let id = path.strip_prefix("/devtools/browser/").or_else(|| path.strip_prefix("/devtools/page/"));
    id.is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

fn rewrite_targets(body: &str, host: &str) -> Result<String, String> {
    fn rewrite(value: &mut json::Value, host: &str) -> Result<(), String> {
        match value {
            json::Value::Array(items) => items.iter_mut().try_for_each(|item| rewrite(item, host)),
            json::Value::Object(fields) => {
                fields.remove("devtoolsFrontendUrl");
                if let Some(json::Value::String(url)) = fields.get_mut("webSocketDebuggerUrl") {
                    let path = url.strip_prefix("ws://").and_then(|rest| rest.find('/').map(|i| &rest[i..]));
                    *url = format!("ws://{host}{}", path.ok_or("CDP returned an unexpected debugger URL")?);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    let mut value: json::Value = json::from_str(body).map_err(|_| "CDP returned unexpected JSON".to_string())?;
    rewrite(&mut value, host)?;
    Ok(json::to_string(&value))
}

fn guest_websocket(cdp_socket: &Path, path: &str, key: &str) -> Result<(StdUnixStream, String), String> {
    let mut stream = StdUnixStream::connect(cdp_socket).map_err(|e| format!("CDP: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 || stream.read(&mut byte).map_err(|e| format!("CDP: {e}"))? == 0 {
            return Err("CDP closed the connection during the websocket handshake".into());
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    if !head.lines().next().is_some_and(|status| status.split_whitespace().nth(1) == Some("101")) {
        return Err("CDP refused the websocket upgrade".into());
    }
    let accept = head
        .lines()
        .find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("sec-websocket-accept")))
        .map(|(_, value)| value.trim().to_string())
        .ok_or("CDP sent no Sec-WebSocket-Accept")?;
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
    Ok((stream, accept))
}

fn proxy_cdp_socket(daemon: &Arc<Daemon>, token: &str, cdp_socket: &Path, path: &str, request: &mut Request) -> Response<BoxBody> {
    let key = header_str(request, header::SEC_WEBSOCKET_KEY).unwrap_or("");
    if header_str(request, header::SEC_WEBSOCKET_VERSION) != Some("13") || key.len() != 24 || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)) {
        return text(StatusCode::BAD_REQUEST, "Not a WebSocket upgrade.");
    }
    let (guest, accept) = match guest_websocket(cdp_socket, path, key) {
        Ok(guest) => guest,
        Err(e) => return text(StatusCode::BAD_GATEWAY, &e),
    };
    let Ok(watched) = guest.try_clone() else {
        return text(StatusCode::BAD_GATEWAY, "Could not watch the CDP connection.");
    };
    let Ok(guest) = async_net::unix::UnixStream::try_from(guest) else {
        return text(StatusCode::BAD_GATEWAY, "Could not adopt the CDP connection.");
    };
    let on_upgrade = h12tiny::server::upgrade::on(request);
    let mut response = respond(StatusCode::SWITCHING_PROTOCOLS, "text/plain", "");
    let headers = response.headers_mut();
    headers.remove(header::CONTENT_TYPE);
    headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    match HeaderValue::from_str(&accept) {
        Ok(accept) => headers.insert(header::SEC_WEBSOCKET_ACCEPT, accept),
        Err(_) => return text(StatusCode::BAD_GATEWAY, "CDP sent a malformed Sec-WebSocket-Accept."),
    };

    let finished = Arc::new(AtomicBool::new(false));
    let (daemon, token) = (Arc::clone(daemon), token.to_string());
    let done = Arc::clone(&finished);
    thread::spawn(move || {
        while !done.load(Ordering::Acquire) {
            let live = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).capability_session(&token).is_some();
            if !live {
                let _ = watched.shutdown(std::net::Shutdown::Both);
            }
            thread::sleep(Duration::from_millis(250));
        }
    });
    thread::spawn(move || {
        futures_lite::future::block_on(async move {
            if let Ok(upgraded) = on_upgrade.await {
                let (mut client_read, mut client_write) = futures_lite::io::split(HyperIo::new(upgraded));
                let (mut guest_read, mut guest_write) = (guest.clone(), guest);
                futures_lite::future::race(
                    futures_lite::io::copy(&mut client_read, &mut guest_write),
                    futures_lite::io::copy(&mut guest_read, &mut client_write),
                )
                .await
                .ok();
            }
            finished.store(true, Ordering::Release);
        });
    });
    response
}

fn proxy_vnc(daemon: &Arc<Daemon>, name: &str, request: &mut Request, viewer: String) -> Response<BoxBody> {
    let Ok(session) = SessionName::new(name) else {
        return text(StatusCode::BAD_REQUEST, "Invalid session name.");
    };
    let Some((vnc_port, interactive)) = daemon.vnc_target(&session) else {
        return text(StatusCode::CONFLICT, "That session has no display right now.");
    };
    let upgrade = match WebSocketUpgrade::try_from_request(request) {
        Ok(upgrade) => upgrade,
        Err(e) => return text(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let response = upgrade.response();
    let daemon = Arc::clone(daemon);
    thread::spawn(move || {
        futures_lite::future::block_on(async move {
            let policy = || {
                let holds = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).controllers.get(&session) == Some(&viewer);
                if interactive && holds { InputPolicy::Forward } else { InputPolicy::Drop }
            };
            if let (Ok(vnc), Ok(connection)) = (TcpStream::connect(("127.0.0.1", vnc_port)).await, upgrade.into_connection().await) {
                relay(connection, vnc, policy).await;
            }
            let mut gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
            if gateway.controllers.get(&session) == Some(&viewer) {
                gateway.controllers.remove(&session);
            }
        });
    });
    response
}

async fn relay(connection: h12tiny::web::WebSocketConnection, vnc: TcpStream, policy: impl Fn() -> InputPolicy) {
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
            if filter.push(&frame.payload, policy(), &mut out).is_err() || vnc_write.write_all(&out).await.is_err() {
                break;
            }
        }
        let _ = vnc_write.close().await;
    };

    futures_lite::future::zip(to_viewer, to_guest).await;
}


#[derive(Clone, Copy)]
enum ControlRequest {
    Inspect,
    Acquire,
    Release,
}

#[derive(Serialize)]
struct ControlState {
    held: bool,
    mine: bool,
}

/// The input lease: one dashboard session at a time may send keyboard and pointer input to a browser session.
fn control(daemon: &Daemon, name: &str, viewer: &str, request: ControlRequest) -> Response<BoxBody> {
    let Ok(session) = SessionName::new(name) else {
        return text(StatusCode::BAD_REQUEST, "Invalid session name.");
    };
    let mut gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
    let holder = gateway.controllers.get(&session).cloned();
    match request {
        ControlRequest::Inspect => {}
        ControlRequest::Acquire => match holder.as_deref() {
            Some(other) if other != viewer => return text(StatusCode::CONFLICT, "Another viewer has control of this browser."),
            _ => {
                drop(gateway);
                if daemon.vnc_target(&session).is_none_or(|(_, interactive)| !interactive) {
                    return text(StatusCode::CONFLICT, "This runtime is view-only: it has no input devices.");
                }
                gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
                if gateway.controllers.get(&session).is_some_and(|h| h != viewer) {
                    return text(StatusCode::CONFLICT, "Another viewer has control of this browser.");
                }
                gateway.controllers.insert(session.clone(), viewer.to_string());
            }
        },
        ControlRequest::Release => {
            if holder.as_deref() == Some(viewer) {
                gateway.controllers.remove(&session);
            }
        }
    }
    let holder = gateway.controllers.get(&session);
    json_response(&ControlState { held: holder.is_some(), mine: holder.is_some_and(|h| h == viewer) })
}

pub(crate) fn person_in_control(daemon: &Daemon, session: &SessionName) -> bool {
    daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).controllers.contains_key(session)
}

fn release_control(daemon: &Daemon, session: &SessionName) {
    daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).controllers.remove(session);
}

fn stop_session(daemon: &Daemon, session: &SessionName) -> Result<(), String> {
    release_control(daemon, session);
    daemon.down_session(session)
}
