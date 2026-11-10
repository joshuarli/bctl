use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use miniserde::{Deserialize, json};

use crate::host::{self, Fallible, Runtime, output_of};

#[derive(Deserialize, Clone)]
pub struct SessionView {
    pub name: String,
    pub state: String,
    pub stage: Option<String>,
    pub machine: Option<String>,
    pub vnc_port: Option<u16>,
    pub failure: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[derive(Deserialize)]
struct Reply {
    error: Option<ErrorBody>,
    sessions: Vec<SessionView>,
    url: Option<String>,
}

pub struct UpTiming {
    pub total: Duration,
    pub stages: Vec<(String, Duration)>,
}

pub struct Home {
    pub path: PathBuf,
    bctl: PathBuf,
    runtime: Runtime,
}

pub fn session_name(tag: &str) -> String {
    format!("h1-{tag}-{}", std::process::id())
}

impl Home {
    pub fn new(tag: &str) -> Fallible<Home> {
        let root = host::root();
        let runtime = Runtime::current()?;
        let path = root.join(format!(".work/tmp/h1-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(path.join("runtime")).and_then(|()| std::fs::create_dir_all(path.join("images"))).map_err(|e| e.to_string())?;
        let link = |from: PathBuf, to: PathBuf| std::os::unix::fs::symlink(from, to).map_err(|e| e.to_string());
        let image = runtime.image();
        let extension = match runtime {
            Runtime::SmolVm => {
                link(root.join(".work/runtime/smolvm"), path.join("runtime/smolvm"))?;
                "tar"
            }
            Runtime::Nanos => {
                std::fs::create_dir_all(path.join("runtime/nanos")).map_err(|e| e.to_string())?;
                let built = root.join(".work/runtime/nanos");
                link(built.join("hydrogen/nanos/output/platform/pc/bin/kernel.img"), path.join("runtime/nanos/kernel.img"))?;
                link(built.join("bin/vhost-device-vsock"), path.join("runtime/nanos/vhost-device-vsock"))?;
                "img"
            }
        };
        let source = root.join(format!(".work/images/{image}.{extension}"));
        if !source.exists() {
            return Err(match runtime {
                Runtime::SmolVm => format!("no {}; run BCTL_BROWSER_ARCHIVE=DIR images/build.sh {image}", source.display()),
                Runtime::Nanos => format!("no {}; build it with images/{image}/build.sh and copy .work/images/{image}.img there", source.display()),
            });
        }
        link(source, path.join(format!("images/{image}.{extension}")))?;
        let bctl = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("bctl");
        if !bctl.exists() {
            return Err(format!("no {}; run cargo build --release -p bctl -p bctl-bench", bctl.display()));
        }
        Ok(Home { path, bctl, runtime })
    }

    pub fn smolvm(&self) -> PathBuf {
        self.path.join("runtime/smolvm/smolvm")
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.bctl);
        command.args(args).env("BCTL_HOME", &self.path).env("BCTL_RUNTIME", self.runtime.name());
        command
    }

    fn reply(&self, args: &[&str]) -> Fallible<Reply> {
        let mut args = args.to_vec();
        args.push("--json");
        let output: Output = self.command(&args).output().map_err(|e| format!("cannot run bctl: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reply: Reply = json::from_str(&stdout).map_err(|_| format!("bctl {args:?} printed {stdout:?} / {}", String::from_utf8_lossy(&output.stderr)))?;
        match reply.error {
            Some(error) => Err(format!("bctl {args:?}: {} [{}]", error.message, error.code)),
            None => Ok(reply),
        }
    }

    pub fn status(&self) -> Fallible<Vec<SessionView>> {
        Ok(self.reply(&["status"])?.sessions)
    }

    pub fn session(&self, name: &str) -> Fallible<SessionView> {
        self.status()?.into_iter().find(|s| s.name == name).ok_or_else(|| format!("no session {name}"))
    }

    pub fn cdp_url(&self, name: &str) -> Fallible<String> {
        self.reply(&["cdp-url", name])?.url.ok_or_else(|| "cdp-url returned no url".into())
    }

    pub fn login_url(&self) -> Fallible<String> {
        self.reply(&["serve"])?.url.ok_or_else(|| "serve returned no url".into())
    }

    pub fn start_daemon(&self) -> Fallible<Duration> {
        let started = Instant::now();
        self.status()?;
        Ok(started.elapsed())
    }

    pub fn up(&self, name: &str, profile: &str, image: &str, allow_private: &[String]) -> Fallible<UpTiming> {
        let done = Arc::new(AtomicBool::new(false));
        let poller = {
            let done = Arc::clone(&done);
            let command_path = self.bctl.clone();
            let bctl_home = self.path.clone();
            let runtime = self.runtime.name();
            let name = name.to_string();
            thread::spawn(move || {
                let started = Instant::now();
                let mut seen: Vec<(String, Duration)> = Vec::new();
                while !done.load(Ordering::Relaxed) {
                    let output = Command::new(&command_path).args(["status", "--json"]).env("BCTL_HOME", &bctl_home).env("BCTL_RUNTIME", runtime).stderr(Stdio::null()).output();
                    let at = started.elapsed();
                    if let Ok(output) = output
                        && let Ok(reply) = json::from_str::<Reply>(&String::from_utf8_lossy(&output.stdout))
                        && let Some(stage) = reply.sessions.iter().find(|s| s.name == name).and_then(|s| s.stage.clone())
                        && seen.last().is_none_or(|(last, _)| *last != stage)
                    {
                        seen.push((stage, at));
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                seen
            })
        };
        let mut args = vec!["up", name, "--profile", profile, "--image", image];
        for range in allow_private {
            args.extend(["--allow-private", range]);
        }
        let started = Instant::now();
        let result = self.reply(&args);
        let total = started.elapsed();
        done.store(true, Ordering::Relaxed);
        let stages = poller.join().map_err(|_| "stage poller panicked")?;
        let view = result?.sessions.into_iter().find(|s| s.name == name).ok_or("up returned no session")?;
        if view.state != "ready" {
            return Err(format!("{name} is {} after up: {}", view.state, view.failure.unwrap_or_default()));
        }
        Ok(UpTiming { total, stages })
    }

    pub fn pause(&self, name: &str) -> Fallible<Duration> {
        let started = Instant::now();
        self.reply(&["pause", name])?;
        Ok(started.elapsed())
    }

    pub fn resume(&self, name: &str) -> Fallible<Duration> {
        let started = Instant::now();
        let view = self.reply(&["resume", name])?.sessions.into_iter().find(|s| s.name == name).ok_or("resume returned no session")?;
        if view.state != "ready" {
            return Err(format!("{name} is {} after resume: {}", view.state, view.failure.unwrap_or_default()));
        }
        Ok(started.elapsed())
    }

    pub fn open_tab(&self, name: &str, url: &str) -> Fallible<()> {
        let output = self.command(&["tab", "new", name, url]).output().map_err(|e| format!("cannot run bctl: {e}"))?;
        if !output.status.success() {
            return Err(format!("bctl tab new {name} exited {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim()));
        }
        Ok(())
    }

    pub fn page(&self, args: &[&str]) -> Fallible<String> {
        let output = self.command(&[&["page"], args].concat()).output().map_err(|e| format!("cannot run bctl: {e}"))?;
        if !output.status.success() {
            return Err(format!("bctl page {args:?} exited {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub fn down(&self, name: &str) -> Fallible<Duration> {
        let started = Instant::now();
        self.reply(&["down", name])?;
        Ok(started.elapsed())
    }

    pub fn remove(&self, name: &str) -> Fallible<()> {
        self.reply(&["rm", name]).map(drop)
    }

    pub fn daemon_pid(&self) -> Fallible<u32> {
        let lock = self.path.join("daemon.lock");
        output_of("lsof", &["-t", lock.to_str().expect("utf-8 path")])?.lines().next().and_then(|p| p.parse().ok()).ok_or_else(|| "no daemon holds the lock".into())
    }

    pub fn vmm_pids(&self) -> Fallible<Vec<u32>> {
        match self.runtime {
            Runtime::SmolVm => host::pids_matching(&format!("{}/runtime/smolvm/smolvm-bin", self.path.display())),
            Runtime::Nanos => host::pids_matching(&format!("{}/machines/", self.path.display())),
        }
    }

    pub fn machine_pid(&self, machine: &str) -> Fallible<u32> {
        match self.runtime {
            Runtime::SmolVm => {
                let status = output_of(self.smolvm().to_str().expect("utf-8 path"), &["machine", "status", "--name", machine, "--json"])?;
                #[derive(Deserialize)]
                struct Status {
                    pid: u32,
                }
                json::from_str::<Status>(&status).map(|s| s.pid).map_err(|_| format!("machine status of {machine} has no pid"))
            }
            Runtime::Nanos => {
                let mut pids = host::pids_matching(&format!("{}/machines/{machine}/qmp.sock", self.path.display()))?;
                match pids.len() {
                    1 => Ok(pids.remove(0)),
                    count => Err(format!("{count} processes hold {machine}'s qmp socket")),
                }
            }
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Ok(sessions) = self.status() {
            for session in sessions {
                let _ = self.down(&session.name);
            }
        }
        if let Ok(pid) = self.daemon_pid() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
