use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::accounts::{self, Account};
use crate::browser::{DISPLAY_ENV, Display};
use crate::check::{cdp_answers, headless_report};
use crate::relay::{self, Log};
use crate::signals::{Event, Signals, send};
use crate::vsock::VsockListener;
use crate::layout;

const TICK: Duration = Duration::from_millis(100);
const BROWSER_TIMEOUT: Duration = Duration::from_secs(60);
const BROWSER_GRACE: Duration = Duration::from_secs(15);

fn io_error(what: impl AsRef<Path>) -> impl FnOnce(io::Error) -> String {
    move |error| format!("{}: {error}", what.as_ref().display())
}

fn own(path: &Path, account: &Account) -> Result<(), String> {
    std::os::unix::fs::chown(path, Some(account.uid), Some(account.gid)).map_err(io_error(path))
}

pub fn run() -> Result<ExitCode, String> {
    let signals = Signals::block()?;
    let browser = accounts::user(layout::BROWSER_USER)?;
    let profile = layout::profile_dir();
    for directory in [Path::new(layout::STATE_DIR), &profile, &browser.home] {
        fs::create_dir_all(directory).map_err(io_error(directory))?;
    }
    fs::set_permissions(layout::STATE_DIR, fs::Permissions::from_mode(0o755)).map_err(io_error(layout::STATE_DIR))?;
    own(&profile, &browser)?;
    own(&browser.home, &browser)?;
    let browser_log = layout::log("browser.log");
    File::create(&browser_log).map_err(io_error(&browser_log))?;
    own(&browser_log, &browser)?;

    let mut child = Command::new(layout::SELF)
        .arg("browser")
        .uid(browser.uid)
        .gid(browser.gid)
        .env(DISPLAY_ENV, Display::Headless.as_str())
        .env("HOME", &browser.home)
        .env("USER", layout::BROWSER_USER)
        .stdin(Stdio::null())
        .spawn()
        .map_err(io_error(layout::SELF))?;

    let outcome = bring_up(&signals, &mut child).and_then(|()| hold(&signals, &mut child));
    stop_browser(&mut child);
    if outcome.is_err() {
        echo_log_tail(&browser_log);
    }
    unsafe { libc::sync() };
    outcome
}

const LOG_TAIL_BYTES: usize = 16 * 1024;

fn echo_log_tail(path: &Path) {
    match fs::read(path) {
        Ok(bytes) => {
            let tail = &bytes[bytes.len().saturating_sub(LOG_TAIL_BYTES)..];
            eprintln!("bctl-guest: last {} bytes of {}:\n{}", tail.len(), path.display(), String::from_utf8_lossy(tail));
        }
        Err(error) => eprintln!("bctl-guest: {}: {error}", path.display()),
    }
}

fn bring_up(signals: &Signals, child: &mut Child) -> Result<(), String> {
    let deadline = Instant::now() + BROWSER_TIMEOUT;
    while !cdp_answers() {
        if let Some(status) = child.try_wait().map_err(io_error("browser"))? {
            return Err(format!("the browser exited before CDP answered: {status}"));
        }
        if let Some(Event::Stop(signal)) = signals.wait(TICK) {
            return Err(format!("signal {signal} before the browser was ready"));
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out after {BROWSER_TIMEOUT:?} waiting for CDP"));
        }
    }
    relay::start_vsock(layout::VSOCK_CDP_PORT, &Log::open(&layout::log("relay.log"))?)?;
    start_check_server()?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "bctl-guest: ready").and_then(|()| stdout.flush()).map_err(io_error("stdout"))
}

fn start_check_server() -> Result<(), String> {
    let port = layout::VSOCK_CHECK_PORT;
    let listener = VsockListener::bind(port).map_err(|e| format!("listen on vsock port {port}: {e}"))?;
    thread::spawn(move || {
        for client in listener.incoming() {
            match client {
                Ok(mut client) => {
                    if let Err(error) = client.write_all(headless_report().as_bytes()).and_then(|()| client.shutdown_write()) {
                        eprintln!("bctl-guest: check reply: {error}");
                    }
                }
                Err(error) => eprintln!("bctl-guest: check accept: {error}"),
            }
        }
    });
    Ok(())
}

fn hold(signals: &Signals, child: &mut Child) -> Result<ExitCode, String> {
    loop {
        if let Some(Event::Stop(signal)) = signals.wait(TICK) {
            eprintln!("bctl-guest: signal {signal}: stopping the browser");
            return Ok(ExitCode::SUCCESS);
        }
        if let Some(status) = child.try_wait().map_err(io_error("browser"))? {
            return Err(format!("browser exited unexpectedly: {status}"));
        }
    }
}

fn stop_browser(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    if let Err(error) = send(child.id(), libc::SIGTERM)
        && error.raw_os_error() != Some(libc::ESRCH)
    {
        eprintln!("bctl-guest: SIGTERM browser: {error}");
    }
    let deadline = Instant::now() + BROWSER_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(TICK);
    }
    eprintln!("bctl-guest: the browser ignored SIGTERM for {BROWSER_GRACE:?}; killing it");
    if let Err(error) = child.kill().and_then(|()| child.wait().map(drop)) {
        eprintln!("bctl-guest: kill the browser: {error}");
    }
}
