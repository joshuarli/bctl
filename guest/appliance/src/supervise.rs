use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::accounts::{self, Account};
use crate::check::{cdp_answers, is_socket};
use crate::signals::{Event, Signals, send};
use crate::{devices, layout, maps, procfs, relay};

const TICK: Duration = Duration::from_millis(100);
const STAGE_TIMEOUT: Duration = Duration::from_secs(30);
const BROWSER_GRACE: Duration = Duration::from_secs(15);
const CHILD_GRACE: Duration = Duration::from_secs(5);

enum Stop {
    Requested(i32),
    Exited { stage: &'static str, how: String },
    Failed(String),
}

impl From<String> for Stop {
    fn from(message: String) -> Stop {
        Stop::Failed(message)
    }
}

struct Supervised {
    stage: &'static str,
    child: Child,
}

struct Supervisor {
    signals: Signals,
    seat: Option<Supervised>,
    compositor: Option<Supervised>,
    browser: Option<u32>,
}

pub fn run() -> Result<ExitCode, String> {
    let signals = Signals::block()?;
    let mut supervisor = Supervisor { signals, seat: None, compositor: None, browser: None };
    let stop = match supervisor.bring_up() {
        Ok(()) => supervisor.hold(),
        Err(stop) => stop,
    };
    if let Stop::Requested(signal) = stop {
        eprintln!("bctl-guest: signal {signal}: stopping the browser, the compositor, then the seat");
    }
    supervisor.shut_down();
    match stop {
        Stop::Requested(_) => Ok(ExitCode::SUCCESS),
        Stop::Exited { stage, how } => Err(format!("{stage} exited unexpectedly: {how}")),
        Stop::Failed(message) => Err(message),
    }
}

fn io_error(what: impl AsRef<Path>) -> impl FnOnce(io::Error) -> String {
    move |error| format!("{}: {error}", what.as_ref().display())
}

fn private_log(name: &str) -> Result<File, String> {
    let path = layout::log(name);
    File::create(&path).map_err(io_error(&path))
}

fn own(path: &Path, account: &Account, mode: Option<u32>) -> Result<(), String> {
    std::os::unix::fs::chown(path, Some(account.uid), Some(account.gid)).map_err(io_error(path))?;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_error(path))?;
    }
    Ok(())
}

impl Supervisor {
    fn bring_up(&mut self) -> Result<(), Stop> {
        let browser = accounts::user(layout::BROWSER_USER)?;
        let video = accounts::group(layout::DEVICE_GROUP)?;
        let runtime = layout::runtime_dir(browser.uid);
        let profile = layout::profile_dir();
        let cdp_port = layout::cdp_tcp_port()?;
        for directory in [Path::new(layout::STATE_DIR), &runtime, &profile, &browser.home] {
            fs::create_dir_all(directory).map_err(io_error(directory))?;
        }
        own(&runtime, &browser, Some(0o700))?;
        own(&profile, &browser, None)?;
        own(&browser.home, &browser, None)?;
        let browser_log = layout::log("browser.log");
        File::create(&browser_log).map_err(io_error(&browser_log))?;
        own(&browser_log, &browser, None)?;
        if maps::requested()? {
            maps::start();
        }
        devices::expose(video)?;

        let seatd_log = private_log("seatd.log")?;
        let seatd = Command::new(layout::SEATD)
            .args(["-g", layout::DEVICE_GROUP])
            .env("SEATD_VTBOUND", "0")
            .stdin(Stdio::null())
            .stdout(seatd_log.try_clone().map_err(io_error("seatd.log"))?)
            .stderr(seatd_log)
            .spawn()
            .map_err(io_error(layout::SEATD))?;
        self.seat = Some(Supervised { stage: "seat", child: seatd });
        self.await_stage("seat", || is_socket(Path::new(layout::SEAT_SOCKET)))?;

        let compositor_log = private_log("compositor.log")?;
        let tinyland = Command::new(layout::TINYLAND)
            .args(["--seat", "--socket", layout::WAYLAND_SOCKET, "--", layout::SELF, "browser"])
            .uid(browser.uid)
            .gid(browser.gid)
            .env("HOME", &browser.home)
            .env("USER", layout::BROWSER_USER)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("LIBSEAT_BACKEND", "seatd")
            .env("SEATD_SOCK", layout::SEAT_SOCKET)
            .env("TINYLAND_XKB_ROOT", layout::XKB_ROOT)
            .stdin(Stdio::null())
            .stdout(compositor_log.try_clone().map_err(io_error("compositor.log"))?)
            .stderr(compositor_log)
            .spawn()
            .map_err(io_error(layout::TINYLAND))?;
        let compositor_pid = tinyland.id();
        self.compositor = Some(Supervised { stage: "compositor", child: tinyland });
        let wayland = layout::wayland_socket(browser.uid);
        self.await_stage("compositor", || is_socket(&wayland))?;

        relay::start(&layout::cdp_socket(), cdp_port, &relay::Log::open(&layout::log("relay.log"))?)?;
        self.await_stage("browser", cdp_answers)?;
        let browser_pid = procfs::children(compositor_pid)
            .into_iter()
            .next()
            .ok_or_else(|| format!("the browser answers CDP but is not a child of the compositor ({compositor_pid})"))?;
        self.browser = Some(browser_pid);
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "bctl-guest: ready").and_then(|()| stdout.flush()).map_err(io_error("stdout"))?;
        Ok(())
    }

    fn hold(&mut self) -> Stop {
        loop {
            if let Err(stop) = self.watch() {
                return stop;
            }
        }
    }

    fn await_stage(&mut self, stage: &str, mut reached: impl FnMut() -> bool) -> Result<(), Stop> {
        let deadline = Instant::now() + STAGE_TIMEOUT;
        loop {
            if reached() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Stop::Failed(format!("timed out after {STAGE_TIMEOUT:?} waiting for the {stage} stage")));
            }
            self.watch()?;
        }
    }

    fn watch(&mut self) -> Result<(), Stop> {
        if let Some(Event::Stop(signal)) = self.signals.wait(TICK) {
            return Err(Stop::Requested(signal));
        }
        if let Some(pid) = self.browser
            && !procfs::alive(pid)
        {
            self.browser = None;
            return Err(Stop::Exited { stage: "browser", how: format!("pid {pid} is gone") });
        }
        for slot in [&mut self.seat, &mut self.compositor] {
            let Some(supervised) = slot else { continue };
            match supervised.child.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) => {
                    let stage = supervised.stage;
                    *slot = None;
                    return Err(Stop::Exited { stage, how: status.to_string() });
                }
                Err(error) => return Err(Stop::Failed(format!("wait for the {} stage: {error}", supervised.stage))),
            }
        }
        Ok(())
    }

    fn shut_down(&mut self) {
        if let Some(mut compositor) = self.compositor.take() {
            let mut browsers = procfs::children(compositor.child.id());
            if let Some(pid) = self.browser.take()
                && !browsers.contains(&pid)
            {
                browsers.push(pid);
            }
            for &pid in &browsers {
                terminate(pid, "browser");
            }
            let deadline = Instant::now() + BROWSER_GRACE;
            while browsers.iter().any(|&pid| procfs::alive(pid)) && Instant::now() < deadline {
                thread::sleep(TICK);
            }
            stop_child(&mut compositor);
        }
        if let Some(mut seat) = self.seat.take() {
            stop_child(&mut seat);
        }
        unsafe { libc::sync() };
    }
}

fn terminate(pid: u32, what: &str) {
    match send(pid, libc::SIGTERM) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {}
        Err(error) => eprintln!("bctl-guest: SIGTERM {what} ({pid}): {error}"),
    }
}

fn stop_child(supervised: &mut Supervised) {
    let child = &mut supervised.child;
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    terminate(child.id(), supervised.stage);
    let deadline = Instant::now() + CHILD_GRACE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(TICK),
            Err(error) => {
                eprintln!("bctl-guest: wait for the {} stage: {error}", supervised.stage);
                break;
            }
        }
    }
    eprintln!("bctl-guest: the {} stage ignored SIGTERM for {CHILD_GRACE:?}; killing it", supervised.stage);
    if let Err(error) = child.kill().and_then(|()| child.wait().map(drop)) {
        eprintln!("bctl-guest: kill the {} stage: {error}", supervised.stage);
    }
}
