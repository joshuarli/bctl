use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use bctl_rfb::{Buttons, Client, KEY_DOWN, KEY_RETURN, Rgb};
use miniserde::json;
use shadowdriver::crawler::{ChromiumSessionOptions, CrawlerSession, CrawlerTab, TabOptions};

const DISPLAY: (u16, u16) = (1280, 800);
const MARKER: Rgb = Rgb(255, 0, 255);
const FIXTURE: &str = include_str!("../../../tests/fixtures/input.html");
const READY_TIMEOUT: Duration = Duration::from_secs(180);

type Proof<T> = Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runs = match args.as_slice() {
        [cmd] if cmd == "census" => return census_main(),
        [cmd] if cmd == "m1" => 1,
        [cmd, flag, n] if cmd == "m1" && flag == "--runs" => match n.parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => return usage(),
        },
        _ => return usage(),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let smolvm = root.join(".work/runtime/smolvm/smolvm");
    let image = std::env::var_os("BCTL_PROOF_IMAGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".work/images/ref-alpine-chromium.tar"));
    for (what, path, how) in [("runtime", &smolvm, "runtime/build.sh"), ("image", &image, "images/build.sh ref-alpine-chromium")] {
        if !path.exists() {
            eprintln!("bctl-proof: no {what} at {}; run {how}", path.display());
            return ExitCode::FAILURE;
        }
    }
    for run in 1..=runs {
        let started = Instant::now();
        match m1(&smolvm, &image, run) {
            Ok(()) => println!("m1 run {run}/{runs}: ok in {:.0?}", started.elapsed()),
            Err(error) => {
                eprintln!("m1 run {run}/{runs}: FAILED: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!("usage: bctl-proof m1 [--runs N] | bctl-proof census");
    ExitCode::from(2)
}

struct Machine {
    smolvm: PathBuf,
    name: String,
    pid: Option<u32>,
}

impl Machine {
    fn start(smolvm: &Path, image: &Path, name: &str, vnc_port: u16, forwards: &[(u16, u16)]) -> Proof<Machine> {
        let mut machine = Machine { smolvm: smolvm.to_path_buf(), name: name.to_string(), pid: None };
        let image = image.to_string_lossy();
        let ports: Vec<String> = forwards.iter().map(|(host, guest)| format!("{host}:{guest}")).collect();
        let mut create = vec!["machine", "create", "--name", name, "--gpu", "--mem", "4096", "--image", &image, "-e", "BCTL_CDP_PORT=9222"];
        for port in &ports {
            create.extend(["-p", port]);
        }
        machine.smolvm(&create)?;
        let started = Command::new(&machine.smolvm)
            .args(["machine", "start", "--name", name])
            .env("SMOLVM_DISPLAY", format!("{}x{}", DISPLAY.0, DISPLAY.1))
            .env("SMOLVM_VNC", format!("127.0.0.1:{vnc_port}"))
            .output()
            .map_err(|e| format!("cannot run smolvm: {e}"))?;
        let stdout = String::from_utf8_lossy(&started.stdout);
        if !started.status.success() {
            return Err(format!("machine start failed: {}", String::from_utf8_lossy(&started.stderr).trim()));
        }
        machine.pid = stdout
            .split("(PID: ")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .and_then(|pid| pid.parse().ok());
        if machine.pid.is_none() {
            return Err(format!("machine start did not report a PID: {}", stdout.trim()));
        }
        Ok(machine)
    }

    fn smolvm(&self, args: &[&str]) -> Proof<String> {
        let output = Command::new(&self.smolvm).args(args).output().map_err(|e| format!("cannot run smolvm: {e}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(format!("smolvm {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim()))
        }
    }

    fn guest_check(&self) -> Proof<String> {
        self.smolvm(&["machine", "exec", "--name", &self.name, "--", "bctl-guest", "check"])
    }

    fn stop(mut self) -> Proof<Option<u32>> {
        self.smolvm(&["machine", "stop", "--name", &self.name])?;
        self.smolvm(&["machine", "delete", "--name", &self.name, "-f"])?;
        let pid = self.pid.take();
        self.name.clear();
        Ok(pid)
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        if !self.name.is_empty() {
            let _ = self.smolvm(&["machine", "stop", "--name", &self.name]);
            let _ = self.smolvm(&["machine", "delete", "--name", &self.name, "-f"]);
        }
    }
}

fn free_port() -> Proof<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot reserve a port: {e}"))?;
    listener.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

fn port_open(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

fn cdp_version(port: u16) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").ok()?;
    let mut response = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        response.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&response);
        let (head, body) = text.split_once("\r\n\r\n")?;
        let length: usize = head
            .lines()
            .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))?
            .parse()
            .ok()?;
        if body.len() >= length {
            return body.contains("webSocketDebuggerUrl").then(|| body.to_string());
        }
    }
}

fn wait_until<T>(what: &str, timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Proof<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Ok(value);
        }
        if Instant::now() > deadline {
            return Err(format!("timed out after {timeout:?} waiting for {what}"));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn m1(smolvm: &Path, image: &Path, run: usize) -> Proof<()> {
    let vnc_port = free_port()?;
    let cdp_port = free_port()?;
    let name = format!("bctl-proof-{}-{run}", std::process::id());
    let machine = Machine::start(smolvm, image, &name, vnc_port, &[(cdp_port, 9222)])?;
    wait_until("CDP through the host forward", READY_TIMEOUT, || cdp_version(cdp_port))?;
    let check = machine.guest_check()?;
    for stage in ["seat", "compositor", "browser", "sandbox"] {
        if !check.lines().any(|line| line == format!("{stage} ok")) {
            return Err(format!("guest check did not report {stage} ok:\n{check}"));
        }
    }

    async_io::block_on(drive(cdp_port, vnc_port))?;

    let pid = machine.stop()?;
    wait_until("VM process exit", Duration::from_secs(30), || {
        let alive = pid.is_some_and(|pid| Command::new("kill").args(["-0", &pid.to_string()]).output().is_ok_and(|o| o.status.success()));
        (!alive).then_some(())
    })?;
    for port in [vnc_port, cdp_port] {
        if port_open(port) {
            return Err(format!("port {port} still accepts connections after stop"));
        }
    }
    Ok(())
}

async fn drive(cdp_port: u16, vnc_port: u16) -> Proof<()> {
    let mut options = ChromiumSessionOptions::new(PathBuf::new(), PathBuf::new());
    options.control_url = Some(format!("http://127.0.0.1:{cdp_port}"));
    options.headless = false;
    options.stealth = false;
    options.identity_mode = shadowdriver::BrowserIdentityMode::BrowserNative;
    let session = CrawlerSession::ensure_chromium(options).await.map_err(|e| format!("attach failed: {e}"))?;
    let tab = session
        .new_tab(TabOptions { preload_script: None, apply_stealth: false, viewport_width: 1200, viewport_height: 600 })
        .await
        .map_err(|e| format!("new tab failed: {e}"))?;
    tab.activate().await.map_err(|e| format!("bring to front failed: {e}"))?;
    let fixture = format!("data:text/html;base64,{}", base64::engine::general_purpose::STANDARD.encode(FIXTURE));
    tab.navigate(&fixture, "load", Duration::from_secs(30)).await.map_err(|e| format!("fixture navigation failed: {e}"))?;

    let mut vnc = Client::connect(("127.0.0.1", vnc_port), Duration::from_secs(10)).map_err(|e| format!("VNC connect failed: {e}"))?;
    let size = (vnc.frame().width(), vnc.frame().height());
    if size != DISPLAY {
        return Err(format!("VNC framebuffer is {size:?}, expected {DISPLAY:?}"));
    }
    let origin = wait_until("the fixture marker on screen", Duration::from_secs(20), || {
        vnc.refresh().ok().and_then(|frame| frame.find(MARKER))
    })?;
    let targets = targets(&tab, origin).await?;
    let at = |id: &str| targets.get(id).copied().ok_or_else(|| format!("fixture has no {id}"));

    let mut before = vnc.refresh().map_err(io)?.clone();
    let mut step = |vnc: &mut Client, what: &str| -> Proof<()> {
        thread::sleep(Duration::from_millis(400));
        let after = vnc.refresh().map_err(io)?.clone();
        if after.changed_pixels(&before) == 0 {
            let dump = dump_frame(&after, what);
            return Err(format!("{what}: no pixels changed; frame at {dump}"));
        }
        before = after;
        Ok(())
    };

    let (x, y) = at("text")?;
    vnc.click(x, y).map_err(io)?;
    wait_for_js(&tab, "document.activeElement.id === 'text'").await?;
    vnc.type_text("Hello bctl!").map_err(io)?;
    wait_for_js(&tab, "document.getElementById('text').value === 'Hello bctl!'").await?;
    step(&mut vnc, "typing")?;

    let (x, y) = at("choice")?;
    vnc.click(x, y).map_err(io)?;
    step(&mut vnc, "opening the select popup")?;
    vnc.tap(KEY_DOWN).map_err(io)?;
    vnc.tap(KEY_RETURN).map_err(io)?;
    step(&mut vnc, "choosing an option")?;

    let (x, y) = at("clicker")?;
    vnc.click(x, y).map_err(io)?;
    vnc.scroll(x, y, Buttons::WHEEL_DOWN, 5).map_err(io)?;
    step(&mut vnc, "scrolling")?;

    let (x, y) = at("opener")?;
    vnc.click(x, y).map_err(io)?;
    step(&mut vnc, "opening a popup window")?;

    let log = evaluate_string(&tab, "JSON.stringify(window.__bctl)").await?;
    for expected in [r#"["text","Hello bctl!"]"#, r#"["choice","second"]"#, r#"["click",true]"#, r#"["wheel","down"]"#, r#"["popup",true]"#] {
        if !log.contains(expected) {
            return Err(format!("fixture log lacks {expected}: {log}"));
        }
    }
    if !log.contains(r#"["scroll","#) {
        return Err(format!("fixture never scrolled: {log}"));
    }

    tab.navigate("data:text/html,<title>bctl second page</title>", "load", Duration::from_secs(30))
        .await
        .map_err(|e| format!("second navigation failed: {e}"))?;
    wait_for_js(&tab, "document.title === 'bctl second page'").await?;
    session.close(false).await;
    Ok(())
}

fn io(error: std::io::Error) -> String {
    format!("VNC: {error}")
}

async fn evaluate_string(tab: &CrawlerTab, js: &str) -> Proof<String> {
    match tab.evaluate(js).await.map_err(|e| format!("evaluate {js:?} failed: {e}"))? {
        json::Value::String(s) => Ok(s),
        other => Err(format!("evaluate {js:?} returned {}", json::to_string(&other))),
    }
}

/// Screen coordinates of each fixture control: its viewport position plus where the marker at viewport (0, 0) landed.
async fn targets(tab: &CrawlerTab, origin: (u16, u16)) -> Proof<BTreeMap<String, (u16, u16)>> {
    let rects = evaluate_string(
        tab,
        r#"Object.entries(window.__bctlRects()).map(([k, [x, y]]) => `${k}:${x},${y}`).join("|")"#,
    )
    .await?;
    rects
        .split('|')
        .map(|entry| {
            let (id, xy) = entry.split_once(':').ok_or_else(|| format!("bad rect {entry}"))?;
            let (x, y) = xy.split_once(',').ok_or_else(|| format!("bad rect {entry}"))?;
            let x: u16 = x.parse().map_err(|_| format!("bad rect {entry}"))?;
            let y: u16 = y.parse().map_err(|_| format!("bad rect {entry}"))?;
            Ok((id.to_string(), (origin.0 + x, origin.1 + y)))
        })
        .collect()
}

/// Writes `frame` as a binary PPM under `.work/tmp` and returns its path, or why it could not.
fn dump_frame(frame: &bctl_rfb::Frame, what: &str) -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp");
    let path = dir.join(format!("proof-{}.ppm", what.replace(' ', "-")));
    let mut ppm = format!("P6\n{} {}\n255\n", frame.width(), frame.height()).into_bytes();
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            let Rgb(r, g, b) = frame.pixel(x, y);
            ppm.extend_from_slice(&[r, g, b]);
        }
    }
    match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, ppm)) {
        Ok(()) => path.display().to_string(),
        Err(error) => format!("(not written: {error})"),
    }
}

/// Polls a boolean page expression until it holds, so input is never sent before the page can receive it.
async fn wait_for_js(tab: &CrawlerTab, condition: &str) -> Proof<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let js = format!("String(Boolean({condition}))");
    loop {
        if evaluate_string(tab, &js).await? == "true" {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("page never satisfied {condition}"));
        }
        async_io::Timer::after(Duration::from_millis(50)).await;
    }
}

const CENSUS_GUEST_PORT: u16 = 9301;

fn census_main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let smolvm = root.join(".work/runtime/smolvm/smolvm");
    let image = root.join(".work/images/ref-alpine-chromium.tar");
    match census(&smolvm, &image, &root.join(".work/tmp/census.log")) {
        Ok(summary) => {
            print!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("census: FAILED: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Records every Wayland global Chromium binds and every request it sends while driving the B1 interactions
/// plus a dialog and a context menu, using a second, WAYLAND_DEBUG-logging browser in the same guest.
fn census(smolvm: &Path, image: &Path, log_path: &Path) -> Proof<String> {
    let vnc_port = free_port()?;
    let cdp_port = free_port()?;
    let census_port = free_port()?;
    let name = format!("bctl-census-{}", std::process::id());
    let machine = Machine::start(smolvm, image, &name, vnc_port, &[(cdp_port, 9222), (census_port, CENSUS_GUEST_PORT)])?;
    wait_until("the appliance browser", READY_TIMEOUT, || cdp_version(cdp_port))?;
    machine.smolvm(&[
        "machine", "exec", "--name", &name, "--", "sh", "-c",
        "nohup su-exec browser env XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 WAYLAND_DEBUG=1 \
         chromium --ozone-platform=wayland --remote-debugging-port=9300 --user-data-dir=/tmp/census \
         --no-first-run --no-default-browser-check about:blank > /run/bctl/census.log 2>&1 & \
         nohup socat TCP-LISTEN:9301,fork,reuseaddr TCP:127.0.0.1:9300 > /dev/null 2>&1 &",
    ])?;
    wait_until("the census browser", READY_TIMEOUT, || cdp_version(census_port))?;
    async_io::block_on(drive(census_port, vnc_port))?;
    async_io::block_on(dialog_and_menu(census_port, vnc_port))?;
    let log = machine.smolvm(&["machine", "exec", "--name", &name, "--", "cat", "/run/bctl/census.log"])?;
    machine.stop()?;
    std::fs::write(log_path, &log).map_err(|e| format!("cannot write {}: {e}", log_path.display()))?;
    Ok(summarize(&log, log_path))
}

async fn dialog_and_menu(cdp_port: u16, vnc_port: u16) -> Proof<()> {
    let mut options = ChromiumSessionOptions::new(PathBuf::new(), PathBuf::new());
    options.control_url = Some(format!("http://127.0.0.1:{cdp_port}"));
    options.headless = false;
    options.stealth = false;
    options.identity_mode = shadowdriver::BrowserIdentityMode::BrowserNative;
    let session = CrawlerSession::ensure_chromium(options).await.map_err(|e| format!("attach failed: {e}"))?;
    let tab = session
        .new_tab(TabOptions { preload_script: None, apply_stealth: false, viewport_width: 1200, viewport_height: 600 })
        .await
        .map_err(|e| format!("new tab failed: {e}"))?;
    tab.activate().await.map_err(|e| format!("bring to front failed: {e}"))?;
    tab.navigate("data:text/html,<body style='background:%23ff00ff'><p title='a tooltip'>menu</p></body>", "load", Duration::from_secs(30))
        .await
        .map_err(|e| format!("navigation failed: {e}"))?;
    let mut vnc = Client::connect(("127.0.0.1", vnc_port), Duration::from_secs(10)).map_err(io)?;
    let (x, y) = wait_until("the magenta page", Duration::from_secs(20), || vnc.refresh().ok().and_then(|f| f.find(MARKER)))?;
    let (x, y) = (x + 40, y + 40);
    vnc.pointer(x, y, Buttons::NONE).map_err(io)?;
    vnc.pointer(x, y, Buttons::RIGHT).map_err(io)?;
    vnc.pointer(x, y, Buttons::NONE).map_err(io)?;
    thread::sleep(Duration::from_millis(800));
    vnc.tap(bctl_rfb::KEY_ESCAPE).map_err(io)?;
    tab.evaluate("setTimeout(() => alert('bctl'), 0); 'scheduled'").await.map_err(|e| format!("alert failed: {e}"))?;
    thread::sleep(Duration::from_millis(800));
    vnc.tap(KEY_RETURN).map_err(io)?;
    thread::sleep(Duration::from_millis(400));
    session.close(false).await;
    Ok(())
}

fn summarize(log: &str, log_path: &Path) -> String {
    let mut globals: BTreeMap<String, String> = BTreeMap::new();
    let mut requests: BTreeMap<String, usize> = BTreeMap::new();
    for line in log.lines().filter(|l| l.contains(" -> ")) {
        let Some(call) = line.split(" -> ").nth(1) else { continue };
        let Some((target, rest)) = call.split_once('(') else { continue };
        let Some((object, request)) = target.split_once(['#', '@']) else { continue };
        let request = request.split_once('.').map(|(_, r)| r).unwrap_or(request);
        if object == "wl_registry" && request == "bind" {
            let mut args = rest.split(", ");
            if let (Some(_), Some(name), Some(version)) = (args.next(), args.next(), args.next()) {
                globals.insert(name.trim_matches('"').to_string(), version.to_string());
            }
        }
        *requests.entry(format!("{object}.{request}")).or_default() += 1;
    }
    let mut out = format!("census log: {}\n\nglobals bound (interface version):\n", log_path.display());
    for (name, version) in &globals {
        out.push_str(&format!("  {name} {version}\n"));
    }
    out.push_str("\nrequests sent (count):\n");
    for (request, count) in &requests {
        out.push_str(&format!("  {request} {count}\n"));
    }
    out
}
