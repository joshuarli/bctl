#![allow(dead_code)]
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bctl_cdp::protocol::{AttachToTarget, Command as CdpCommand, Effect, SessionId, TargetId};
use bctl_cdp::{Connection, Endpoint, Generation, Scope};
use futures_lite::future::block_on;
use miniserde::{Deserialize, Serialize, json};

#[derive(Debug, Deserialize)]
pub struct Error {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize, Clone)]
pub struct Session {
    pub name: String,
    pub state: String,
    pub generation: u64,
    pub stage: Option<String>,
    pub failure: Option<String>,
    pub input: Option<String>,
    pub machine: Option<String>,
    pub vnc_port: Option<u16>,
}

#[derive(Deserialize)]
pub struct Response {
    pub error: Option<Error>,
    pub sessions: Vec<Session>,
}

#[derive(Deserialize)]
pub struct Machine {
    pub name: String,
}

#[derive(Deserialize)]
pub struct MachineStatus {
    pub state: String,
    pub pid: Option<u32>,
}

/// The runtime every VM test boots: `BCTL_TEST_RUNTIME`, else SmolVM on macOS and Nanos elsewhere.
pub fn runtime() -> String {
    std::env::var("BCTL_TEST_RUNTIME").unwrap_or_else(|_| if cfg!(target_os = "macos") { "smolvm" } else { "nanos" }.to_string())
}

pub fn nanos() -> bool {
    runtime() == "nanos"
}

/// The image every VM test boots: `BCTL_TEST_IMAGE`, or the runtime's default image.
pub fn image() -> String {
    std::env::var("BCTL_TEST_IMAGE").unwrap_or_else(|_| if nanos() { "nanos-helium" } else { "helium" }.to_string())
}

pub struct Home {
    pub path: PathBuf,
    tag: String,
}

impl Home {
    pub fn new(tag: &str) -> Home {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let path = workspace.join(format!(".work/tmp/bctl-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(path.join("runtime")).unwrap();
        std::fs::create_dir_all(path.join("images")).unwrap();
        let image = if nanos() {
            let name = image();
            let profile = format!("{name}.profile.img");
            std::os::unix::fs::symlink(workspace.join(".work/images").join(&profile), path.join("images").join(&profile)).unwrap();
            if cfg!(target_os = "macos") {
                std::os::unix::fs::symlink(workspace.join(".work/runtime/nanos-krun"), path.join("runtime/nanos")).unwrap();
            } else {
                let built = workspace.join(".work/runtime/nanos");
                std::fs::create_dir_all(path.join("runtime/nanos")).unwrap();
                std::os::unix::fs::symlink(built.join("bin/vhost-device-vsock"), path.join("runtime/nanos/vhost-device-vsock")).unwrap();
                let kernel = format!("{name}.kernel.img");
                std::os::unix::fs::symlink(workspace.join(".work/images").join(&kernel), path.join("images").join(&kernel)).unwrap();
            }
            format!("{name}.img")
        } else {
            std::os::unix::fs::symlink(workspace.join(".work/runtime/smolvm"), path.join("runtime/smolvm")).unwrap();
            format!("{}.tar", image())
        };
        std::os::unix::fs::symlink(workspace.join(".work/images").join(&image), path.join("images").join(&image)).unwrap();
        Home { path, tag: tag.to_string() }
    }

    pub fn bctl(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_bctl")).args(args).env("BCTL_HOME", &self.path).env("BCTL_RUNTIME", runtime()).output().unwrap()
    }

    pub fn json(&self, args: &[&str]) -> Response {
        let mut args = args.to_vec();
        args.push("--json");
        let output = self.bctl(&args);
        json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {args:?}: {output:?}"))
    }

    pub fn up(&self, session: &str, profile: &str) -> Output {
        self.bctl(&["up", session, "--profile", profile, "--image", &image()])
    }

    pub fn cdp_url(&self, session: &str) -> String {
        let output = self.bctl(&["cdp-url", session]);
        assert_ok(&output);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    pub fn prefix(&self) -> String {
        format!("{}{}", self.tag, std::process::id())
    }

    pub fn daemon_pid(&self) -> String {
        let output = Command::new("lsof").arg("-t").arg(self.path.join("daemon.lock")).output().unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    pub fn bctl_machines(&self) -> Vec<String> {
        let prefix = format!("bctl-{}-", self.prefix());
        if nanos() {
            let Ok(entries) = std::fs::read_dir(self.path.join("machines")) else { return Vec::new() };
            return entries.filter_map(|e| e.ok()?.file_name().into_string().ok()).filter(|n| n.starts_with(&prefix)).collect();
        }
        let smolvm = self.path.join("runtime/smolvm/smolvm");
        let output = Command::new(smolvm).args(["machine", "ls", "--json"]).output().unwrap();
        let machines: Vec<Machine> = json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
        machines.into_iter().map(|m| m.name).filter(|n| n.starts_with(&prefix)).collect()
    }

    pub fn smolvm(&self, args: &[&str]) -> Output {
        Command::new(self.path.join("runtime/smolvm/smolvm")).args(args).output().unwrap()
    }

    pub fn machine_status(&self, machine: &str) -> MachineStatus {
        let output = self.smolvm(&["machine", "status", "--name", machine, "--json"]);
        json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("machine status {machine}: {output:?}"))
    }

    pub fn machine_logs(&self, machine: &str) -> String {
        let output = self.smolvm(&["machine", "logs", "--name", machine, "--tail", "40"]);
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    pub fn guest_shell(&self, machine: &str, script: &str) -> Output {
        let busybox = self.path.join("runtime/smolvm/agent-rootfs/bin/busybox");
        assert!(busybox.is_file(), "no busybox at {} for guest failure injection", busybox.display());
        let copy = self.smolvm(&["machine", "cp", "--mode", "755", busybox.to_str().unwrap(), &format!("{machine}:/tmp/busybox")]);
        assert_ok(&copy);
        self.smolvm(&["machine", "exec", "--name", machine, "--", "/tmp/busybox", "sh", "-c", script])
    }

    pub fn kill_guest_process(&self, machine: &str, name: &str) {
        let output = self.guest_shell(
            machine,
            &format!("pids=$(/tmp/busybox pidof {name}); test -n \"$pids\" || exit 3; for p in $pids; do kill -9 \"$p\" 2>/dev/null || true; done"),
        );
        assert_ok(&output);
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        for session in self.json(&["status"]).sessions {
            self.bctl(&["down", &session.name]);
        }
        let pid = self.daemon_pid();
        if !pid.is_empty() {
            let _ = Command::new("kill").arg(&pid).status();
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn status_of<'a>(response: &'a Response, name: &str) -> &'a Session {
    response.sessions.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no session {name}"))
}

pub fn assert_ok(output: &Output) {
    assert!(output.status.success(), "{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[derive(Deserialize)]
struct Version {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

/// A CDP client outside bctl's automation, standing in for a page or a person acting on the browser.
pub fn outside_client(home: &Home, session: &str) -> Connection {
    let control_url = home.cdp_url(session);
    let host = control_url.strip_prefix("http://").unwrap().to_string();
    let port: u16 = host.rsplit(':').next().unwrap().parse().unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or_else(|| panic!("{response}"));
    let endpoint = Endpoint::parse(&json::from_str::<Version>(body).unwrap().web_socket_debugger_url).unwrap();
    let (connection, driver) = block_on(bctl_cdp::connect(&endpoint, Generation::new(0))).unwrap();
    thread::spawn(move || block_on(driver));
    connection
}

/// Attaches the outside client to a page target, for commands the page cannot tell from its own.
pub fn attach(outside: &Connection, target: &str) -> SessionId {
    block_on(outside.call(Scope::Browser, &AttachToTarget::new(TargetId::new(target)), soon())).unwrap().session_id
}

#[derive(Serialize)]
struct Evaluate {
    expression: String,
    #[serde(rename = "returnByValue")]
    return_by_value: bool,
}

#[derive(Deserialize)]
struct Evaluated {
    result: Remote,
}

#[derive(Deserialize)]
struct Remote {
    value: Option<json::Value>,
}

impl CdpCommand for Evaluate {
    const METHOD: &'static str = "Runtime.evaluate";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Evaluated;
}

/// Evaluates `expression` in the page's main world through the outside client, not through bctl.
pub fn evaluate_outside(outside: &Connection, session: &SessionId, expression: &str) -> json::Value {
    let command = Evaluate { expression: expression.to_string(), return_by_value: true };
    block_on(outside.call(Scope::Session(session), &command, soon())).unwrap().result.value.unwrap_or(json::Value::Null)
}

pub fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

pub fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(200));
    }
}

/// SmolVM's default guest link: the gateway, 100.96.0.1, is the host's loopback.
pub const HOST_RANGE: &str = "100.96.0.0/30";
pub const HOST_FROM_GUEST: &str = "100.96.0.1";
/// The same address as an IPv4-mapped IPv6 host: another site to Chromium, so its iframe gets its own
/// process and CDP target.
pub const OTHER_SITE_FROM_GUEST: &str = "[::ffff:100.96.0.1]";

/// A local HTTP server for fixture pages, reachable from the guest at [`HOST_FROM_GUEST`]. `site` maps a
/// request path and the server's port to an HTML body. It answers every method alike and keeps each request
/// line it was sent, so a page can report what it saw with a beacon.
pub struct Fixture {
    pub port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    pub fn start(site: fn(&str, u16) -> String) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let kept = requests.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let kept = kept.clone();
                thread::spawn(move || serve(stream, port, site, &kept));
            }
        });
        Fixture { port, requests }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{HOST_FROM_GUEST}:{}{path}", self.port)
    }

    /// Every request line the server has been sent, such as `POST /beacon?x`, in the order they came.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(mut stream: TcpStream, port: u16, site: fn(&str, u16) -> String, requests: &Mutex<Vec<String>>) {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&request);
    let Some((method, path)) = text.lines().next().and_then(|line| {
        let mut parts = line.split(' ');
        Some((parts.next()?, parts.next()?))
    }) else {
        return;
    };
    requests.lock().unwrap().push(format!("{method} {path}"));
    let body = site(path, port);
    let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(format!("{head}{body}").as_bytes());
}

/// Starts a session whose guest may reach the fixture server.
pub fn up_with_fixtures(home: &Home, session: &str, profile: &str) {
    assert_ok(&home.bctl(&["up", session, "--profile", profile, "--image", &image(), "--allow-private", HOST_RANGE]));
}

pub fn http(port: u16, host: &str, request_line: &str, extra: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "{request_line} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{extra}\r\n").unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, response)
}

pub fn process_alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().unwrap().success()
}

pub fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(500)).is_ok()
}

pub fn browser_answers(home: &Home, session: &str) -> bool {
    let output = home.bctl(&["cdp-url", session]);
    if !output.status.success() {
        return false;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let Some(host) = url.strip_prefix("http://") else { return false };
    let Some(port) = host.rsplit(':').next().and_then(|port| port.parse::<u16>().ok()) else { return false };
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else { return false };
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    if write!(stream, "GET /json/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response.starts_with("HTTP/1.1 200") && response.contains("\"Browser\"")
}

pub fn open_vnc_viewer(port: u16, address: &str, headers: &str, session: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        stream,
        "GET /api/sessions/{session}/vnc HTTP/1.1\r\nHost: {address}\r\n{headers}\
         Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        assert!(stream.read(&mut byte).unwrap() != 0, "the live view closed during its upgrade");
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    assert!(head.lines().next().is_some_and(|line| line.contains(" 101 ")), "{head}");
    stream
}

/// Signs a dashboard viewer in and returns the headers that act as it, plus the dashboard's address.
pub fn dashboard_viewer(home: &Home) -> (u16, String, String) {
    let served = home.bctl(&["serve"]);
    assert_ok(&served);
    let login = String::from_utf8_lossy(&served.stdout).trim().to_string();
    let address = login.strip_prefix("http://").unwrap().split('/').next().unwrap().to_string();
    let port: u16 = address.rsplit(':').next().unwrap().parse().unwrap();
    let (status, response) = http(port, &address, &format!("GET /{}", login.split_once(&format!("{address}/")).unwrap().1), "");
    assert_eq!(status, 303, "{response}");
    let cookie = response.lines().find_map(|l| l.strip_prefix("set-cookie: ")).unwrap().split(';').next().unwrap().to_string();
    (port, address.clone(), format!("Cookie: {cookie}\r\nOrigin: http://{address}\r\nContent-Length: 0\r\n"))
}
