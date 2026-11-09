#![allow(dead_code)]
use std::path::PathBuf;
use std::process::{Command, Output};

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
    pub cdp_url: Option<String>,
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
