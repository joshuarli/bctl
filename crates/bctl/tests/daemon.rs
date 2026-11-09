use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use miniserde::{Deserialize, json};

#[derive(Deserialize)]
struct Error {
    code: String,
}

#[derive(Deserialize)]
struct Session {
    name: String,
    state: String,
    generation: u64,
    stage: Option<String>,
    input: Option<String>,
    machine: Option<String>,
}

#[derive(Deserialize)]
struct Response {
    error: Option<Error>,
    sessions: Vec<Session>,
}

#[derive(Deserialize)]
struct Machine {
    name: String,
}

struct Home {
    path: PathBuf,
}

impl Home {
    fn new() -> Home {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let path = workspace.join(format!(".work/tmp/bctl-c3-{}", std::process::id()));
        std::fs::create_dir_all(path.join("runtime")).unwrap();
        std::fs::create_dir_all(path.join("images")).unwrap();
        std::os::unix::fs::symlink(workspace.join(".work/runtime/smolvm"), path.join("runtime/smolvm")).unwrap();
        std::os::unix::fs::symlink(workspace.join(".work/images/ref-alpine-chromium.tar"), path.join("images/ref-alpine-chromium.tar")).unwrap();
        Home { path }
    }

    fn bctl(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_bctl")).args(args).env("BCTL_HOME", &self.path).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Response {
        let mut args = args.to_vec();
        args.push("--json");
        let output = self.bctl(&args);
        json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {args:?}: {output:?}"))
    }

    fn up(&self, session: &str, profile: &str) -> Output {
        self.bctl(&["up", session, "--profile", profile, "--image", "ref-alpine-chromium"])
    }

    fn daemon_pid(&self) -> String {
        let output = Command::new("lsof").arg("-t").arg(self.path.join("daemon.lock")).output().unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn bctl_machines(&self) -> Vec<String> {
        let smolvm = self.path.join("runtime/smolvm/smolvm");
        let output = Command::new(smolvm).args(["machine", "ls", "--json"]).output().unwrap();
        let machines: Vec<Machine> = json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
        let prefix = format!("bctl-c3{}-", std::process::id());
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

fn status_of<'a>(response: &'a Response, name: &str) -> &'a Session {
    response.sessions.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no session {name}"))
}

fn assert_ok(output: &Output) {
    assert!(output.status.success(), "{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn the_daemon_owns_sessions_across_restarts() {
    let home = Home::new();
    let pid = std::process::id();
    let (a, b, c) = (format!("c3{pid}-a"), format!("c3{pid}-b"), format!("c3{pid}-c"));

    let starting = {
        let (home_path, c) = (home.path.clone(), c.clone());
        thread::spawn(move || {
            Command::new(env!("CARGO_BIN_EXE_bctl"))
                .args(["up", &c, "--profile", "pc", "--image", "ref-alpine-chromium"])
                .env("BCTL_HOME", home_path)
                .output()
                .unwrap()
        })
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let seen_stage = loop {
        let status = home.json(&["status"]);
        if let Some(stage) = status.sessions.iter().find(|s| s.name == c).and_then(|s| s.stage.clone()) {
            break stage;
        }
        assert!(Instant::now() < deadline, "status never named a readiness stage for a starting session");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(["machine", "guest", "seat", "compositor", "browser", "sandbox", "cdp", "display"].contains(&seen_stage.as_str()));
    assert_ok(&starting.join().unwrap());

    let ups: Vec<_> = [(a.clone(), "pa"), (b.clone(), "pb")]
        .into_iter()
        .map(|(name, profile)| {
            let home_path = home.path.clone();
            thread::spawn(move || {
                Command::new(env!("CARGO_BIN_EXE_bctl"))
                    .args(["up", &name, "--profile", profile, "--image", "ref-alpine-chromium"])
                    .env("BCTL_HOME", home_path)
                    .output()
                    .unwrap()
            })
        })
        .collect();
    for up in ups {
        assert_ok(&up.join().unwrap());
    }

    let before = home.json(&["status"]);
    for name in [&a, &b, &c] {
        let s = status_of(&before, name);
        assert_eq!((s.state.as_str(), s.generation, s.input.as_deref()), ("ready", 1, Some("interactive")), "{name}");
    }

    assert_ok(&home.up(&a, "pa"));
    assert_eq!(status_of(&home.json(&["status"]), &a).generation, 1, "a repeated up must not restart the session");

    let busy = home.json(&["up", &format!("c3{pid}-d"), "--profile", "pa", "--image", "ref-alpine-chromium"]);
    assert_eq!(busy.error.map(|e| e.code).as_deref(), Some("PROFILE_BUSY"));
    let conflict = home.json(&["up", &a, "--profile", "pb", "--image", "ref-alpine-chromium"]);
    assert_eq!(conflict.error.map(|e| e.code).as_deref(), Some("CONFLICT"));

    let daemon = home.daemon_pid();
    assert!(!daemon.is_empty(), "no daemon holds the lock");
    assert!(Command::new("kill").args(["-9", &daemon]).status().unwrap().success());
    thread::sleep(Duration::from_millis(300));

    let after = home.json(&["status"]);
    assert_ne!(home.daemon_pid(), daemon, "status should have started a new daemon");
    for name in [&a, &b, &c] {
        let s = status_of(&after, name);
        assert_eq!(s.generation, 1, "{name} was restarted instead of adopted");
        assert!(matches!(s.state.as_str(), "ready" | "starting"), "{name} is {}", s.state);
        assert_eq!(s.machine.as_deref(), Some(format!("bctl-{name}-1").as_str()));
    }
    let mut machines = home.bctl_machines();
    machines.sort();
    let mut expected = vec![format!("bctl-{a}-1"), format!("bctl-{b}-1"), format!("bctl-{c}-1")];
    expected.sort();
    assert_eq!(machines, expected, "the restart duplicated or lost VMs");

    for name in [&a, &b, &c] {
        assert_ok(&home.bctl(&["down", name]));
    }
    assert!(home.bctl_machines().is_empty(), "down left VMs behind");
    assert_eq!(status_of(&home.json(&["status"]), &a).state, "stopped");
}
