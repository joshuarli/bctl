#![allow(dead_code)]
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use bctl_cdp::{Connection, Endpoint, Generation};
use futures_lite::future::block_on;
use miniserde::{Deserialize, json};

#[derive(Deserialize)]
pub struct Error {
    pub code: String,
}

#[derive(Deserialize)]
pub struct Session {
    pub name: String,
    pub state: String,
    pub generation: u64,
    pub stage: Option<String>,
    pub input: Option<String>,
    pub machine: Option<String>,
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
        std::os::unix::fs::symlink(workspace.join(".work/runtime/smolvm"), path.join("runtime/smolvm")).unwrap();
        std::os::unix::fs::symlink(workspace.join(".work/images/ref-alpine-chromium.tar"), path.join("images/ref-alpine-chromium.tar")).unwrap();
        Home { path, tag: tag.to_string() }
    }

    pub fn bctl(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_bctl")).args(args).env("BCTL_HOME", &self.path).output().unwrap()
    }

    pub fn json(&self, args: &[&str]) -> Response {
        let mut args = args.to_vec();
        args.push("--json");
        let output = self.bctl(&args);
        json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {args:?}: {output:?}"))
    }

    pub fn up(&self, session: &str, profile: &str) -> Output {
        self.bctl(&["up", session, "--profile", profile, "--image", "ref-alpine-chromium"])
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
        let smolvm = self.path.join("runtime/smolvm/smolvm");
        let output = Command::new(smolvm).args(["machine", "ls", "--json"]).output().unwrap();
        let machines: Vec<Machine> = json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
        let prefix = format!("bctl-{}-", self.prefix());
        machines.into_iter().map(|m| m.name).filter(|n| n.starts_with(&prefix)).collect()
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
