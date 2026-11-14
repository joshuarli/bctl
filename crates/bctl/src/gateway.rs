use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Ipv6Addr;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
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

use crate::authority::{AttachRefused, Attachment, Authority, Contested, Control, Controller, Standing, TakeRefused, Viewer};
use crate::daemon::Daemon;
use crate::protocol::Request as DaemonRequest;

mod runs;

const LOGIN_TOKEN_LIFETIME: Duration = Duration::from_secs(120);
const COOKIE: &str = "bctl_session";
const MAX_IDLE_WORKERS: usize = 16;
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct PoolState {
    queue: VecDeque<Job>,
    idle: usize,
}

#[derive(Default)]
struct WorkerPool {
    state: Mutex<PoolState>,
    available: Condvar,
}

impl WorkerPool {
    fn spawn(self: &Arc<Self>, job: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.queue.push_back(Box::new(job));
        if state.idle >= state.queue.len() {
            self.available.notify_one();
            return;
        }
        drop(state);
        let pool = Arc::clone(self);
        thread::spawn(move || pool.run());
    }

    fn run(&self) {
        while let Some(job) = self.next_job() {
            job();
        }
    }

    fn next_job(&self) -> Option<Job> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = state.queue.pop_front() {
            return Some(job);
        }
        if state.idle >= MAX_IDLE_WORKERS {
            return None;
        }
        loop {
            state.idle += 1;
            let (guard, timeout) = self.available.wait_timeout(state, WORKER_IDLE_TIMEOUT).unwrap_or_else(|p| p.into_inner());
            state = guard;
            state.idle = state.idle.saturating_sub(1);
            if let Some(job) = state.queue.pop_front() {
                return Some(job);
            }
            if timeout.timed_out() || state.idle >= MAX_IDLE_WORKERS {
                return None;
            }
        }
    }
}

struct WatchedCdp {
    token: String,
    stream: Arc<StdUnixStream>,
    standing: Standing,
    finished: Arc<AtomicBool>,
}

#[derive(Default)]
struct WatchedState {
    epoch: u64,
    items: Vec<Arc<WatchedCdp>>,
}

#[derive(Default)]
struct WatchedSet {
    state: Mutex<WatchedState>,
    changed: Condvar,
}

impl WatchedSet {
    fn watch(&self, entry: Arc<WatchedCdp>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.items.push(entry);
        state.epoch = state.epoch.wrapping_add(1);
        self.changed.notify_all();
    }

    fn notify(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.epoch = state.epoch.wrapping_add(1);
        self.changed.notify_all();
    }
}

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

#[derive(Default)]
pub struct Gateway {
    port: Option<u16>,
    login_tokens: HashMap<String, Instant>,
    sessions: HashSet<String>,
    /// Unguessable CDP capabilities, each bound to one browser session until revoked.
    capabilities: Vec<(String, SessionName)>,
    pool: Arc<WorkerPool>,
    watched: Arc<WatchedSet>,
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

fn executor(pool: Arc<WorkerPool>) -> BoxExecutor {
    BoxExecutor::new(FnExecutor::new(move |future: BoxSendFuture| {
        pool.spawn(move || futures_lite::future::block_on(future));
    }))
}

fn listener_port(daemon: &Arc<Daemon>, gateway: &mut Gateway) -> io::Result<u16> {
    if let Some(port) = gateway.port {
        return Ok(port);
    }
    let listener = futures_lite::future::block_on(TcpListener::bind("127.0.0.1:0"))?;
    let port = listener.local_addr()?.port();
    let v6_listener = futures_lite::future::block_on(TcpListener::bind((Ipv6Addr::LOCALHOST, port))).ok();
    let daemon_for_accept = Arc::clone(daemon);
    let pool = Arc::clone(&gateway.pool);
    thread::spawn(move || {
        let router = Router::<()>::new().fallback(move |request: Request| {
            let daemon = Arc::clone(&daemon_for_accept);
            async move { handle(&daemon, port, request) }
        });
        let serve = |bound: TcpListener| accept(bound, router.clone(), Arc::clone(&pool));
        futures_lite::future::block_on(async move {
            match v6_listener {
                Some(v6) => {
                    futures_lite::future::zip(serve(listener), serve(v6)).await;
                }
                None => serve(listener).await,
            }
        });
    });
    spawn_cdp_watcher(Arc::clone(daemon), Arc::clone(&gateway.watched));
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

pub(crate) fn cdp_url_controller(url: &str) -> Option<Controller> {
    cdp_url_token(url).map(Controller::new)
}

fn cdp_url_token(url: &str) -> Option<&str> {
    url.strip_prefix("http://").and_then(|host| host.split_once('.')).map(|(token, _)| token)
}

pub fn revoke_cdp_url(daemon: &Daemon, url: &str) {
    let Some(token) = cdp_url_token(url) else { return };
    let watched = {
        let mut gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
        gateway.capabilities.retain(|(candidate, _)| !constant_time_eq(candidate.as_bytes(), token.as_bytes()));
        Arc::clone(&gateway.watched)
    };
    watched.notify();
}

pub fn revoke_cdp(daemon: &Daemon, session: &SessionName) {
    let watched = {
        let mut gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
        gateway.capabilities.retain(|(_, bound)| bound != session);
        Arc::clone(&gateway.watched)
    };
    watched.notify();
}

fn spawn_cdp_watcher(daemon: Arc<Daemon>, watched: Arc<WatchedSet>) {
    thread::spawn(move || loop {
        let (checked_epoch, snapshot) = {
            let mut state = watched.state.lock().unwrap_or_else(|p| p.into_inner());
            state.items.retain(|entry| !entry.finished.load(Ordering::Acquire));
            if state.items.is_empty() {
                let _guard = watched.changed.wait(state).unwrap_or_else(|p| p.into_inner());
                continue;
            }
            (state.epoch, state.items.clone())
        };
        for entry in snapshot {
            let live = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).capability_session(&entry.token).is_some();
            if !live || entry.standing.evicted() {
                let _ = entry.stream.shutdown(std::net::Shutdown::Both);
                entry.finished.store(true, Ordering::Release);
            }
        }
        let state = watched.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.epoch == checked_epoch && !state.items.is_empty() {
            let _ = watched.changed.wait_timeout(state, WATCH_POLL_INTERVAL);
        }
    });
}

async fn accept(listener: TcpListener, router: Router<()>, pool: Arc<WorkerPool>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let _ = stream.set_nodelay(true);
        let router = router.clone();
        let worker_pool = Arc::clone(&pool);
        pool.spawn(move || {
            let builder = auto::Builder::new(executor(worker_pool)).http1_only();
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
        (Method::POST, ["api", "sessions", name, "control"]) => control(daemon, name, &viewer, ControlRequest::Take),
        (Method::POST, ["api", "sessions", name, "control", "resume"]) => control(daemon, name, &viewer, ControlRequest::Resume),
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

fn parse_content_length(head: &[u8]) -> Option<usize> {
    let head = std::str::from_utf8(head).ok()?;
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
    })
}

pub(crate) fn cdp_get(socket: &Path, path: &str) -> Result<String, String> {
    let mut stream = StdUnixStream::connect(socket).map_err(|e| format!("CDP: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    let mut chunk = [0; 8192];
    let mut header_end = None;
    let mut content_length = None;
    loop {
        let n = stream.read(&mut chunk).map_err(|e| format!("CDP: {e}"))?;
        let prev_len = response.len();
        response.extend_from_slice(&chunk[..n]);
        if header_end.is_none() {
            let search_from = prev_len.saturating_sub(3);
            if let Some(pos) = response[search_from..].windows(4).position(|w| w == b"\r\n\r\n") {
                let end = search_from + pos + 4;
                content_length = parse_content_length(&response[..end - 4]);
                header_end = Some(end);
            }
        }
        if let Some(end) = header_end
            && (n == 0 || content_length.is_some_and(|len| response.len() - end >= len))
        {
            let body_end = content_length.map_or(response.len(), |len| (end + len).min(response.len()));
            return Ok(String::from_utf8_lossy(&response[end..body_end]).into_owned());
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
    let session = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).capability_session(token);
    let Some(session) = session else {
        return text(StatusCode::UNAUTHORIZED, "This CDP URL is invalid or revoked. Run `bctl cdp-url SESSION`.");
    };
    let controller = Controller::new(token);
    if let Err(refused) = daemon.authority.of(&session, |control| control.admits(&controller)) {
        return attach_refused(refused);
    }
    let Some(cdp_socket) = daemon.cdp_socket(&session) else {
        return text(StatusCode::CONFLICT, "That session is not running.");
    };
    let path = request.uri().path().trim_end_matches('/').to_string();
    let upgrading = request.headers().contains_key(header::UPGRADE);
    match (request.method(), path.as_str()) {
        (&Method::GET, "/json/version") if !upgrading => {
            let body = if let Some(cached) = daemon.cached_version_json(&session) {
                Ok(cached)
            } else {
                let fresh = cdp_get(&cdp_socket, "/json/version");
                if let Ok(ref raw) = fresh {
                    daemon.cache_version_json(&session, &cdp_socket, raw);
                }
                fresh
            };
            match body.and_then(|body| rewrite_targets(&body, host)) {
                Ok(body) => respond(StatusCode::OK, "application/json", body),
                Err(e) => text(StatusCode::BAD_GATEWAY, &e),
            }
        }
        (&Method::GET, "/json/list" | "/json") if !upgrading => {
            match cdp_get(&cdp_socket, &path).and_then(|body| rewrite_targets(&body, host)) {
                Ok(body) => respond(StatusCode::OK, "application/json", body),
                Err(e) => text(StatusCode::BAD_GATEWAY, &e),
            }
        }
        (&Method::GET, _) if upgrading && is_devtools_path(&path) => match Attachment::open(daemon, &session, controller) {
            Ok(attachment) => proxy_cdp_socket(daemon, token, attachment, &cdp_socket, &path, request),
            Err(refused) => attach_refused(refused),
        },
        _ => text(StatusCode::NOT_FOUND, "Not found."),
    }
}

fn attach_refused(refused: AttachRefused) -> Response<BoxBody> {
    match refused {
        AttachRefused::Person => text(StatusCode::CONFLICT, "A person has control of this browser, or is taking it; new CDP attachments are refused until automation resumes."),
        AttachRefused::OtherController => text(
            StatusCode::CONFLICT,
            "Another CDP controller is attached to this browser; new attachments through other CDP URLs are refused until its connections close.",
        ),
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

fn guest_websocket(cdp_socket: &Path, path: &str, key: &str) -> Result<(StdUnixStream, String, Vec<u8>), String> {
    let mut stream = StdUnixStream::connect(cdp_socket).map_err(|e| format!("CDP: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| e.to_string())?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        let search_from = buf.len().saturating_sub(3);
        let n = stream.read(&mut chunk).map_err(|e| format!("CDP: {e}"))?;
        if n == 0 {
            return Err("CDP closed the connection during the websocket handshake".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf[search_from..].windows(4).position(|w| w == b"\r\n\r\n") {
            break search_from + pos + 4;
        }
        if buf.len() > 8192 {
            return Err("CDP closed the connection during the websocket handshake".into());
        }
    };
    let leftover = buf.split_off(header_end);
    let head = String::from_utf8_lossy(&buf);
    if !head.lines().next().is_some_and(|status| status.split_whitespace().nth(1) == Some("101")) {
        return Err("CDP refused the websocket upgrade".into());
    }
    let accept = head
        .lines()
        .find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("sec-websocket-accept")))
        .map(|(_, value)| value.trim().to_string())
        .ok_or("CDP sent no Sec-WebSocket-Accept")?;
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
    Ok((stream, accept, leftover))
}

fn proxy_cdp_socket(daemon: &Arc<Daemon>, token: &str, attachment: Attachment, cdp_socket: &Path, path: &str, request: &mut Request) -> Response<BoxBody> {
    let key = header_str(request, header::SEC_WEBSOCKET_KEY).unwrap_or("");
    if header_str(request, header::SEC_WEBSOCKET_VERSION) != Some("13") || key.len() != 24 || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)) {
        return text(StatusCode::BAD_REQUEST, "Not a WebSocket upgrade.");
    }
    let (guest, accept, leftover) = match guest_websocket(cdp_socket, path, key) {
        Ok(guest) => guest,
        Err(e) => return text(StatusCode::BAD_GATEWAY, &e),
    };
    let Ok(watched_stream) = guest.try_clone() else {
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

    let (pool, watched) = {
        let gateway = daemon.gateway.lock().unwrap_or_else(|p| p.into_inner());
        (Arc::clone(&gateway.pool), Arc::clone(&gateway.watched))
    };
    let entry = Arc::new(WatchedCdp {
        token: token.to_string(),
        stream: Arc::new(watched_stream),
        standing: attachment.standing(),
        finished: Arc::new(AtomicBool::new(false)),
    });
    watched.watch(Arc::clone(&entry));
    pool.spawn(move || {
        futures_lite::future::block_on(async move {
            let _attachment = attachment;
            if let Ok(upgraded) = on_upgrade.await {
                let (mut client_read, mut client_write) = futures_lite::io::split(HyperIo::new(upgraded));
                let (mut guest_read, mut guest_write) = (guest.clone(), guest);
                if leftover.is_empty() || client_write.write_all(&leftover).await.is_ok() {
                    futures_lite::future::race(
                        futures_lite::io::copy(&mut client_read, &mut guest_write),
                        futures_lite::io::copy(&mut guest_read, &mut client_write),
                    )
                    .await
                    .ok();
                }
            }
            entry.finished.store(true, Ordering::Release);
            watched.notify();
        });
    });
    response
}

fn proxy_vnc(daemon: &Arc<Daemon>, name: &str, request: &mut Request, viewer: String) -> Response<BoxBody> {
    let viewer = Viewer::new(&viewer);
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
    let pool = Arc::clone(&daemon.gateway.lock().unwrap_or_else(|p| p.into_inner()).pool);
    let daemon = Arc::clone(daemon);
    pool.spawn(move || {
        futures_lite::future::block_on(async move {
            let policy = || {
                let holds = daemon.authority.of(&session, |control| *control.authority() == Authority::Human(viewer.clone()));
                if interactive && holds { InputPolicy::Forward } else { InputPolicy::Drop }
            };
            if let (Ok(vnc), Ok(connection)) = (TcpStream::connect(("127.0.0.1", vnc_port)).await, upgrade.into_connection().await) {
                let _ = vnc.set_nodelay(true);
                relay(connection, vnc, policy).await;
            }
            daemon.authority.of(&session, |control| control.abandon(&viewer));
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
    Take,
    Resume,
}

#[derive(Serialize)]
struct ControlState {
    /// `automation`, `yielding`, or `human`.
    authority: String,
    /// Whether this viewer is the one taking or holding control.
    mine: bool,
    /// Native commands begun under automation that have not ended; `yielding` lasts until this is 0.
    settling: u64,
}

fn control_state(control: &Control, viewer: &Viewer) -> ControlState {
    ControlState {
        authority: control.authority().as_str().to_string(),
        mine: control.authority().viewer() == Some(viewer),
        settling: control.settling() as u64,
    }
}

/// The session's authority as a dashboard viewer sees it. Taking control yields first: the RFB input lease is
/// this viewer's only once the authority is `human`. Resuming hands control back to automation and resumes a
/// run waiting for a person.
fn control(daemon: &Arc<Daemon>, name: &str, viewer: &str, request: ControlRequest) -> Response<BoxBody> {
    let Ok(session) = SessionName::new(name) else {
        return text(StatusCode::BAD_REQUEST, "Invalid session name.");
    };
    let viewer = Viewer::new(viewer);
    let contested = || text(StatusCode::CONFLICT, "Another viewer has control of this browser, or is taking it.");
    match request {
        ControlRequest::Inspect => {}
        ControlRequest::Take => {
            if daemon.vnc_target(&session).is_none_or(|(_, interactive)| !interactive) {
                return text(StatusCode::CONFLICT, "This runtime is view-only: it has no input devices.");
            }
            match daemon.authority.of(&session, |control| control.take(&viewer)) {
                Ok(()) => {}
                Err(TakeRefused::Contested) => return contested(),
                Err(TakeRefused::ControllerAttached { connections }) => {
                    return text(
                        StatusCode::CONFLICT,
                        &format!("An external CDP controller is attached ({connections} open connections). Close it, or end its access with `bctl cdp-revoke {session}`, then take control."),
                    );
                }
            }
        }
        ControlRequest::Resume => {
            if let Err(Contested) = daemon.authority.of(&session, |control| control.resume(&viewer)) {
                return contested();
            }
            if let Some(run) = daemon.run_needing_interaction(&session)
                && let Err(refusal) = daemon.handle(&DaemonRequest { op: "resume".into(), run: Some(run), ..DaemonRequest::empty() })
            {
                let message = refusal.0.error.map(|e| e.message).unwrap_or_default();
                return text(StatusCode::CONFLICT, &format!("Automation resumed, but run {run} did not: {message}"));
            }
        }
    }
    json_response(&daemon.authority.of(&session, |control| control_state(control, &viewer)))
}

fn stop_session(daemon: &Daemon, session: &SessionName) -> Result<(), String> {
    daemon.down_session(session)
}
