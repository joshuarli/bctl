use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::accounts::{self, Account};
use crate::browser::{DISPLAY_ENV, Display};
use crate::check::{cdp_answers, headless_report};
use crate::relay::{self, Log};
use crate::signals::{Event, Signals, send};
use crate::vsock::{VsockListener, VsockStream};
use crate::layout;

const TICK: Duration = Duration::from_millis(100);
const BRING_UP_TICK: Duration = Duration::from_millis(2);
const BROWSER_TIMEOUT: Duration = Duration::from_secs(60);
const BROWSER_CLOSE_GRACE: Duration = Duration::from_secs(8);
const BROWSER_GRACE: Duration = Duration::from_secs(7);

fn io_error(what: impl AsRef<Path>) -> impl FnOnce(io::Error) -> String {
    move |error| format!("{}: {error}", what.as_ref().display())
}

fn own(path: &Path, account: &Account) -> Result<(), String> {
    std::os::unix::fs::chown(path, Some(account.uid), Some(account.gid)).map_err(io_error(path))
}

pub fn run(persistent: bool) -> Result<ExitCode, String> {
    crate::startup::mark("begin");
    let signals = Signals::block()?;
    if persistent {
        let mounts = fs::read_to_string("/proc/mounts").map_err(io_error("/proc/mounts"))?;
        if !mounts.lines().any(|line| line.split_whitespace().nth(1) == Some("/profile")) {
            return Err("the persistent profile volume is not mounted at /profile".into());
        }
    }
    let browser = accounts::user(layout::BROWSER_USER)?;
    crate::startup::mark("profile_checked");
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
    crate::startup::mark("directories_ready");

    let mut command = Command::new(layout::SELF);
    command.arg("browser")
        .uid(browser.uid)
        .gid(browser.gid)
        .env(DISPLAY_ENV, Display::Headless.as_str())
        .env("HOME", &browser.home)
        .env("USER", layout::BROWSER_USER)
        .stdin(Stdio::null());
    signals.unblock_in(&mut command);
    crate::startup::mark("browser_spawn_begin");
    let mut child = command.spawn()
        .map_err(io_error(layout::SELF))?;
    crate::startup::mark("browser_spawned");

    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let stopping = Arc::new(AtomicBool::new(false));
    let outcome = bring_up(&signals, &mut child, persistent.then_some((shutdown_tx, Arc::clone(&stopping))))
        .and_then(|()| hold(&signals, &mut child, &stopping));
    let stopped = stop_browser(&mut child);
    let outcome = outcome.and_then(|code| stopped.map(|()| code));
    if outcome.is_err() {
        echo_log_tail(&browser_log);
    }
    unsafe { libc::sync() };
    if let Ok(stream) = shutdown_rx.try_recv() {
        answer_stop(stream, outcome.is_ok());
    }
    outcome
}

trait HalfClose: Read + Write {
    fn shutdown_write(&self) -> io::Result<()>;
}

impl HalfClose for VsockStream {
    fn shutdown_write(&self) -> io::Result<()> {
        VsockStream::shutdown_write(self)
    }
}

fn answer_stop(mut stream: impl HalfClose, ok: bool) {
    let _ = stream.write_all(if ok { b"ok\n" } else { b"error\n" });
    let _ = stream.shutdown_write();
    // Powering off can discard vsock data the host has not consumed yet, so wait until the host closes after reading the reply.
    let _ = io::copy(&mut stream, &mut io::sink());
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

fn bring_up(signals: &Signals, child: &mut Child, shutdown: Option<(mpsc::Sender<VsockStream>, Arc<AtomicBool>)>) -> Result<(), String> {
    let deadline = Instant::now() + BROWSER_TIMEOUT;
    loop {
        crate::startup::mark("cdp_probe_begin");
        let ready = cdp_answers();
        crate::startup::mark(if ready { "cdp_probe_ready" } else { "cdp_probe_missing" });
        if ready { break; }
        if let Some(status) = child.try_wait().map_err(io_error("browser"))? {
            return Err(format!("the browser exited before CDP answered: {status}"));
        }
        if let Some(Event::Stop(signal)) = signals.wait(BRING_UP_TICK) {
            return Err(format!("signal {signal} before the browser was ready"));
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out after {BROWSER_TIMEOUT:?} waiting for CDP"));
        }
    }
    crate::startup::mark("cdp_answered");
    let relay_log = Log::open(&layout::log("relay.log"))?;
    relay::start_vsock(layout::VSOCK_CDP_PORT, &relay_log)?;
    relay::start_egress(&relay_log)?;
    start_check_server(shutdown.is_some())?;
    if let Some((shutdown, stopping)) = shutdown {
        let listener = VsockListener::bind(layout::VSOCK_STOP_PORT).map_err(|e| format!("listen for shutdown: {e}"))?;
        thread::spawn(move || {
            if let Ok(stream) = listener.accept() && shutdown.send(stream).is_ok() {
                stopping.store(true, Ordering::Release);
            }
        });
    }
    crate::startup::mark("relays_ready");
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "bctl-guest: ready").and_then(|()| stdout.flush()).map_err(io_error("stdout"))
}

fn start_check_server(persistent: bool) -> Result<(), String> {
    let port = layout::VSOCK_CHECK_PORT;
    let listener = VsockListener::bind(port).map_err(|e| format!("listen on vsock port {port}: {e}"))?;
    thread::spawn(move || {
        for client in listener.incoming() {
            match client {
                Ok(mut client) => {
                    let mut report = headless_report();
                    if persistent { report.push_str("profile ok\n"); }
                    if let Err(error) = client.write_all(report.as_bytes()).and_then(|()| client.shutdown_write()) {
                        eprintln!("bctl-guest: check reply: {error}");
                    }
                }
                Err(error) => eprintln!("bctl-guest: check accept: {error}"),
            }
        }
    });
    Ok(())
}

fn hold(signals: &Signals, child: &mut Child, stopping: &AtomicBool) -> Result<ExitCode, String> {
    loop {
        if stopping.load(Ordering::Acquire) {
            return Ok(ExitCode::SUCCESS);
        }
        if let Some(Event::Stop(signal)) = signals.wait(TICK) {
            eprintln!("bctl-guest: signal {signal}: stopping the browser");
            return Ok(ExitCode::SUCCESS);
        }
        if let Some(status) = child.try_wait().map_err(io_error("browser"))? {
            return Err(format!("browser exited unexpectedly: {status}"));
        }
    }
}

fn stop_browser(child: &mut Child) -> Result<(), String> {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return Ok(());
    }
    match crate::cdp::close_browser() {
        Ok(()) => {
            let deadline = Instant::now() + BROWSER_CLOSE_GRACE;
            while Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return Ok(());
                }
                thread::sleep(TICK);
            }
            eprintln!("bctl-guest: the browser ignored Browser.close for {BROWSER_CLOSE_GRACE:?}; sending SIGTERM");
        }
        Err(error) => eprintln!("bctl-guest: Browser.close: {error}; sending SIGTERM"),
    }
    if let Err(error) = send(child.id(), libc::SIGTERM)
        && error.raw_os_error() != Some(libc::ESRCH)
    {
        eprintln!("bctl-guest: SIGTERM browser: {error}");
    }
    let deadline = Instant::now() + BROWSER_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return Ok(());
        }
        thread::sleep(TICK);
    }
    eprintln!("bctl-guest: the browser ignored SIGTERM for {BROWSER_GRACE:?}; killing it");
    if let Err(error) = child.kill().and_then(|()| child.wait().map(drop)) {
        eprintln!("bctl-guest: kill the browser: {error}");
    }
    Err(format!("the browser did not stop within {BROWSER_GRACE:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;

    impl HalfClose for UnixStream {
        fn shutdown_write(&self) -> io::Result<()> {
            self.shutdown(Shutdown::Write)
        }
    }

    #[test]
    fn the_stop_answer_holds_the_connection_until_the_host_has_read_it() {
        let (host, guest) = UnixStream::pair().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let answering = thread::spawn(move || {
            answer_stop(guest, true);
            done_tx.send(()).unwrap();
        });
        let mut reply = String::new();
        io::BufReader::new(&host).read_line(&mut reply).unwrap();
        assert_eq!(reply, "ok\n");
        assert_eq!(done_rx.recv_timeout(Duration::from_millis(200)), Err(mpsc::RecvTimeoutError::Timeout));
        drop(host);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        answering.join().unwrap();
    }
}
