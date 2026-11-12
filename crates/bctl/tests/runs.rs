mod common;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::{Home, assert_ok, image, status_of};
use miniserde::{Deserialize, json};
use shadowdriver::crawler::{ChromiumSessionOptions, CrawlerSession, TabOptions};

/// SmolVM's default guest link: the gateway, 100.96.0.1, is the host's loopback.
const HOST_RANGE: &str = "100.96.0.0/30";
const HOST_FROM_GUEST: &str = "100.96.0.1";

#[derive(Deserialize, Debug)]
struct Attempt {
    number: u32,
    state: String,
    outcome: Option<String>,
    pid: Option<u32>,
}

#[derive(Deserialize, Debug)]
struct Progress {
    pages_done: u32,
}

#[derive(Deserialize, Debug)]
struct Interaction {
    reason: String,
    url: String,
}

#[derive(Deserialize, Debug)]
struct Run {
    id: i64,
    job: Option<String>,
    kind: String,
    state: String,
    outcome: Option<String>,
    detail: Option<String>,
    profile: String,
    image: String,
    image_digest: String,
    browser_version: String,
    config: json::Value,
    input_digest: String,
    output_dir: String,
    progress: Option<Progress>,
    interaction: Option<Interaction>,
    attempts: Vec<Attempt>,
}

#[derive(Deserialize)]
struct Runs {
    error: Option<common::Error>,
    runs: Vec<Run>,
}

#[derive(Deserialize)]
struct Resources {
    width: u32,
    height: u32,
    cpus: u16,
    memory_mib: u32,
}

#[derive(Deserialize)]
struct SessionResources {
    vnc_port: Option<u16>,
    resources: Resources,
}

#[derive(Deserialize)]
struct Inspected {
    sessions: Vec<SessionResources>,
}

fn runs(home: &Home, args: &[&str]) -> Runs {
    let mut args = args.to_vec();
    args.push("--json");
    let output = home.bctl(&args);
    json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {args:?}: {output:?}"))
}

fn started(home: &Home, args: &[&str]) -> Run {
    let mut response = runs(home, args);
    assert!(response.error.is_none(), "bctl {args:?} failed: {:?}", response.error.map(|e| e.code));
    response.runs.pop().unwrap_or_else(|| panic!("bctl {args:?} returned no run"))
}

fn run(home: &Home, id: i64) -> Run {
    started(home, &["runs", &id.to_string()])
}

fn error_code(home: &Home, args: &[&str]) -> Option<String> {
    runs(home, args).error.map(|e| e.code)
}

fn logs(home: &Home, id: i64) -> String {
    let output = home.bctl(&["logs", &id.to_string()]);
    assert_ok(&output);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn wait_for(home: &Home, id: i64, what: &str, timeout: Duration, done: impl Fn(&Run) -> bool) -> Run {
    let deadline = Instant::now() + timeout;
    loop {
        let current = run(home, id);
        if done(&current) {
            return current;
        }
        assert!(Instant::now() < deadline, "run {id} never became {what}; last {current:?}\nlog:\n{}", logs(home, id));
        thread::sleep(Duration::from_millis(500));
    }
}

fn wait_state(home: &Home, id: i64, state: &str, timeout: Duration) -> Run {
    wait_for(home, id, state, timeout, |r| r.state == state)
}

fn up_with_host_access(home: &Home, session: &str) -> Output {
    home.bctl(&["up", session, "--profile", session, "--image", &image(), "--allow-private", HOST_RANGE])
}

fn process_alive(pid: &str) -> bool {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        && stat.rsplit_once(") ").is_some_and(|(_, rest)| rest.starts_with('Z'))
    {
        return false;
    }
    Command::new("kill").args(["-0", pid]).stderr(std::process::Stdio::null()).status().unwrap().success()
}

fn kill_daemon(home: &Home) {
    let daemon = home.daemon_pid();
    assert!(!daemon.is_empty(), "no daemon holds the lock");
    assert!(Command::new("kill").args(["-9", &daemon]).status().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_alive(&daemon) {
        assert!(Instant::now() < deadline, "daemon {daemon} survived kill -9");
        thread::sleep(Duration::from_millis(50));
    }
}

fn config_field<'a>(config: &'a json::Value, path: &[&str]) -> Option<&'a json::Value> {
    path.iter().try_fold(config, |value, key| match value {
        json::Value::Object(map) => map.get(*key),
        _ => None,
    })
}

fn write_config(home: &Home, body: &str) -> PathBuf {
    let path = home.path.join("bctl.toml");
    std::fs::write(&path, body).unwrap();
    path
}

fn read_text(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path.as_ref()).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.as_ref().display()))
}

struct Fixture {
    port: u16,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

struct Reply {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    body: String,
    delay: Duration,
}

fn html(body: String) -> Reply {
    Reply { status: "200 OK", headers: vec![("Content-Type", "text/html; charset=utf-8".into())], body, delay: Duration::ZERO }
}

fn padding() -> String {
    format!("<p>{}</p>", "Fixture text so the page clears the minimum-content check. ".repeat(30))
}

impl Fixture {
    fn start(site: fn(&str, bool) -> Reply) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let counted = Arc::clone(&hits);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let counted = Arc::clone(&counted);
                thread::spawn(move || serve(stream, site, &counted));
            }
        });
        Fixture { port, hits }
    }

    fn guest_url(&self, path: &str) -> String {
        format!("http://{HOST_FROM_GUEST}:{}{path}", self.port)
    }

    fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

fn serve(mut stream: TcpStream, site: fn(&str, bool) -> Reply, hits: &Mutex<HashMap<String, usize>>) {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&chunk[..n]),
        }
        if !request[0].is_ascii_uppercase() {
            return;
        }
    }
    let text = String::from_utf8_lossy(&request);
    let Some(target) = text.lines().next().and_then(|line| line.strip_prefix("GET ")).and_then(|rest| rest.split(' ').next()) else { return };
    let authed = text.lines().any(|line| line.to_ascii_lowercase().starts_with("cookie:") && line.contains("auth=ok"));
    let path = target.split('?').next().unwrap_or(target).to_string();
    *hits.lock().unwrap().entry(path).or_default() += 1;
    let reply = site(target, authed);
    thread::sleep(reply.delay);
    let mut head = format!("HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n", reply.status, reply.body.len());
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let _ = stream.write_all(format!("{head}\r\n{}", reply.body).as_bytes());
}

fn login_site(target: &str, authed: bool) -> Reply {
    let path = target.split('?').next().unwrap_or(target);
    match path {
        "/style.css" => Reply { status: "200 OK", headers: vec![("Content-Type", "text/css".into())], body: "body { color: #123; }".into(), delay: Duration::ZERO },
        "/login" if target.contains("user=operator") => Reply {
            status: "302 Found",
            headers: vec![("Set-Cookie", "auth=ok; Path=/".into()), ("Location", "/".into())],
            body: String::new(),
            delay: Duration::ZERO,
        },
        "/" | "/a.html" | "/b.html" if authed => html(format!(
            "<!doctype html><html><head><title>Members {path}</title><link rel=stylesheet href=/style.css></head>\
             <body><h1>members {path}</h1><a href=/a.html>a</a> <a href=/b.html>b</a>{}</body></html>",
            padding()
        )),
        "/" | "/a.html" | "/b.html" | "/login" => html(format!(
            "<!doctype html><html><head><title>Sign in</title></head><body>\
             <form action=/login><input id=user name=user><input id=pass name=pass type=password>\
             <div class=\"g-recaptcha\" data-sitekey=fixture></div><button>Sign in</button></form>{}</body></html>",
            padding()
        )),
        _ => Reply { status: "404 Not Found", headers: Vec::new(), body: "not found".into(), delay: Duration::ZERO },
    }
}

const CHAIN_PAGES: u32 = 12;

fn chain_site(target: &str, _authed: bool) -> Reply {
    let page = target.strip_prefix("/chain/").and_then(|rest| rest.strip_suffix(".html")).and_then(|n| n.parse::<u32>().ok());
    match page {
        Some(n) if n < CHAIN_PAGES => {
            let next = if n + 1 < CHAIN_PAGES { format!("<a href=/chain/{}.html>next</a>", n + 1) } else { String::new() };
            Reply { delay: Duration::from_millis(1500), ..html(format!("<!doctype html><html><head><title>Chain {n}</title></head><body><h1>chain {n}</h1>{next}{}</body></html>", padding())) }
        }
        _ => Reply { status: "404 Not Found", headers: Vec::new(), body: "not found".into(), delay: Duration::ZERO },
    }
}

fn require_grab() {
    let grab = std::env::var("BCTL_GRAB").unwrap_or_else(|_| panic!("set BCTL_GRAB to Shadowdriver's grab executable (grab/.venv/bin/grab)"));
    assert!(Path::new(&grab).is_file(), "BCTL_GRAB={grab} is not a file");
}

async fn sign_in(control_url: &str, login_url: &str) -> Result<(), String> {
    let mut options = ChromiumSessionOptions::new(PathBuf::new(), PathBuf::new());
    options.control_url = Some(control_url.to_string());
    options.headless = false;
    options.stealth = false;
    options.identity_mode = shadowdriver::BrowserIdentityMode::BrowserNative;
    let session = CrawlerSession::ensure_chromium(options).await.map_err(|e| e.to_string())?;
    let tab = session
        .new_tab(TabOptions { preload_script: None, apply_stealth: false, viewport_width: 1024, viewport_height: 700 })
        .await
        .map_err(|e| e.to_string())?;
    tab.navigate(login_url, "complete", Duration::from_secs(30)).await.map_err(|e| e.to_string())?;
    tab.evaluate("document.getElementById('user').value = 'operator'; document.getElementById('pass').value = 'secret'; document.querySelector('form').submit(); true")
        .await
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let signed_in = loop {
        let text = tab.evaluate("document.body ? document.body.innerText : ''").await.map_err(|e| e.to_string())?;
        if json::to_string(&text).contains("members /") {
            break Ok(());
        }
        if Instant::now() > deadline {
            break Err(format!("the members page never appeared; the page shows {}", json::to_string(&text)));
        }
        async_io::Timer::after(Duration::from_millis(200)).await;
    };
    drop(tab);
    drop(session);
    signed_in
}

fn browser_answers(control_url: &str) -> bool {
    let host = control_url.trim_start_matches("http://");
    let port: u16 = host.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap();
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else { return false };
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response.starts_with("HTTP/1.1 200") && response.contains("\"Browser\"")
}

fn gateway_get(control_url: &str, path: &str) -> (u16, String) {
    let host = control_url.trim_start_matches("http://");
    let port: u16 = host.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap();
    common::http(port, host, &format!("GET {path}"), "")
}

fn hold_browser_socket(control_url: &str) -> TcpStream {
    let (status, version) = gateway_get(control_url, "/json/version");
    assert_eq!(status, 200, "the operator's URL is admitted while the run waits: {version}");
    let path = version.split("ws://").nth(1).and_then(|rest| rest.find('/').map(|i| rest[i..].split('"').next().unwrap().to_string())).unwrap();
    let host = control_url.trim_start_matches("http://");
    let port: u16 = host.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
        head.push(byte[0]);
    }
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 101"), "{}", String::from_utf8_lossy(&head));
    stream
}

fn closed_within(stream: &mut TcpStream, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 4096];
    while Instant::now() < deadline {
        stream.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        match stream.read(&mut buf) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return true,
        }
    }
    false
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh, the BCTL_TEST_IMAGE image (default helium) in .work/images, and BCTL_GRAB"]
fn a_grab_behind_a_login_waits_for_the_operator_then_finishes_a_verified_mirror_in_the_same_browser() {
    require_grab();
    let home = Home::new("j3-l");
    let session = format!("{}-login", home.prefix());
    let fixture = Fixture::start(login_site);
    assert_ok(&up_with_host_access(&home, &session));
    let generation = status_of(&home.json(&["status"]), &session).generation;

    let seed = fixture.guest_url("/");
    let grab = started(&home, &["grab", &seed, "--session", &session]);
    assert_eq!((grab.kind.as_str(), grab.job.as_deref()), ("grab", None));
    let waiting = wait_state(&home, grab.id, "needs-interaction", Duration::from_secs(240));
    let interaction = waiting.interaction.expect("a run waiting for an operator says what for");
    assert_eq!((interaction.reason.as_str(), interaction.url.as_str()), ("challenge", seed.as_str()));
    assert_eq!(fixture.hits("/a.html"), 0, "the crawl went past the login wall without a login");

    let operator_url = home.cdp_url(&session);
    let mut operator_socket = hold_browser_socket(&operator_url);
    async_io::block_on(sign_in(&operator_url, &seed)).unwrap();
    assert!(!closed_within(&mut operator_socket, Duration::from_secs(2)), "the operator's socket stayed open while the run waited");
    let resumed = home.bctl(&["resume", &grab.id.to_string()]);
    assert_ok(&resumed);
    assert!(closed_within(&mut operator_socket, Duration::from_secs(5)), "resuming the run did not close the operator's socket");
    if run(&home, grab.id).state == "running" {
        let (status, refusal) = gateway_get(&operator_url, "/json/version");
        if run(&home, grab.id).state == "running" {
            assert_eq!(status, 409, "the operator's URL was admitted again while the run runs: {refusal}");
        }
    }

    let done = wait_for(&home, grab.id, "ended", Duration::from_secs(300), |r| !matches!(r.state.as_str(), "starting" | "running" | "needs-interaction"));
    assert_eq!((done.state.as_str(), done.outcome.as_deref()), ("succeeded", Some("succeeded")), "{done:?}\n{}", logs(&home, grab.id));
    assert_eq!(done.detail.as_deref(), Some("verified 1 mirror(s)"));
    assert_eq!(done.attempts.len(), 1, "resuming an interaction continues the same attempt");

    let mirror = Path::new(&done.output_dir).join(HOST_FROM_GUEST);
    assert!(read_text(mirror.join("index.html")).contains("members /"), "the seed was captured before the login");
    assert!(read_text(mirror.join("a.html/index.html")).contains("members /a.html"));
    assert!(read_text(mirror.join("b.html/index.html")).contains("members /b.html"));

    let status = home.json(&["status"]);
    let after = status_of(&status, &session);
    assert_eq!((after.state.as_str(), after.generation), ("ready", generation), "the crawl did not finish in the browser the operator signed in to");
    assert!(browser_answers(&operator_url), "the session's browser did not outlive the run");
    assert!(done.browser_version.starts_with("Helium/"), "{}", done.browser_version);
    assert!(done.image_digest.starts_with("sha256:") && done.input_digest.starts_with("sha256:"));
    assert_eq!((done.image.as_str(), done.profile.as_str()), (image().as_str(), session.as_str()));
    assert!(config_field(&done.config, &["grab", "urls"]).is_some_and(|urls| json::to_string(urls).contains(&seed)));
    let log = logs(&home, grab.id);
    assert!(log.contains("Grab needs challenge at") && log.contains("verifying"), "{log}");
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh, the BCTL_TEST_IMAGE image (default helium) in .work/images, and BCTL_GRAB"]
fn a_crawl_interrupted_by_a_daemon_kill_or_paused_continues_from_grab_state_on_resume() {
    require_grab();
    let home = Home::new("j3-c");
    let session = format!("{}-crawl", home.prefix());
    let fixture = Fixture::start(chain_site);
    assert_ok(&up_with_host_access(&home, &session));
    let seed = fixture.guest_url("/chain/0.html");
    let image = image();
    let config = write_config(
        &home,
        &format!(
            "version = 1\n[browsers.default]\nimage = \"{image}\"\n\
             [jobs.chain]\nkind = \"grab\"\nbrowser = \"default\"\nprofile = \"{session}\"\nurls = [\"{seed}\"]\njobs = 1\nrobots = false\n"
        ),
    );
    let crawl = started(&home, &["run", "chain", "--session", &session, "--config", config.to_str().unwrap()]);
    assert_eq!(crawl.job.as_deref(), Some("chain"));
    let midway = wait_for(&home, crawl.id, "two pages in", Duration::from_secs(240), |r| r.progress.as_ref().is_some_and(|p| p.pages_done >= 2));
    let grab_pid = midway.attempts[0].pid.expect("a running attempt records its process").to_string();

    kill_daemon(&home);
    assert!(process_alive(&grab_pid), "grab died with the daemon; this test needs it to outlive it");
    let interrupted = run(&home, crawl.id);
    assert_eq!((interrupted.state.as_str(), interrupted.outcome.as_deref()), ("interrupted", Some("interrupted")));
    assert!(!process_alive(&grab_pid), "the restarted daemon left grab {grab_pid} running");
    assert!(logs(&home, crawl.id).contains("outlived the daemon and was stopped"));
    thread::sleep(Duration::from_secs(3));
    assert_eq!(run(&home, crawl.id).attempts.len(), 1, "an interrupted attempt was retried without being asked");
    let first_page_hits = fixture.hits("/chain/0.html");
    let second_page_hits = fixture.hits("/chain/1.html");

    assert_ok(&home.bctl(&["resume", &crawl.id.to_string()]));
    wait_for(&home, crawl.id, "crawling again", Duration::from_secs(300), |r| {
        r.attempts.len() == 2 && r.state == "running" && r.progress.as_ref().is_some_and(|p| p.pages_done >= 4)
    });
    assert_ok(&home.bctl(&["pause", &crawl.id.to_string()]));
    let paused = wait_state(&home, crawl.id, "paused", Duration::from_secs(120));
    assert_eq!((paused.attempts[1].state.as_str(), paused.outcome.as_deref()), ("paused", None));
    let status = home.json(&["status"]);
    let held = status_of(&status, &session);
    assert_eq!((held.state.as_str(), held.generation), ("ready", 1), "pause touched the VM");
    let chain_hits = || (0..CHAIN_PAGES).map(|n| fixture.hits(&format!("/chain/{n}.html"))).sum::<usize>();
    let while_paused = chain_hits();
    thread::sleep(Duration::from_secs(5));
    assert_eq!(chain_hits(), while_paused, "a paused crawl kept navigating");

    assert_ok(&home.bctl(&["resume", &crawl.id.to_string()]));
    let done = wait_for(&home, crawl.id, "ended", Duration::from_secs(300), |r| r.attempts.len() == 3 && !matches!(r.state.as_str(), "starting" | "running"));
    assert_eq!((done.state.as_str(), done.outcome.as_deref()), ("succeeded", Some("succeeded")), "{done:?}\n{}", logs(&home, crawl.id));
    assert_eq!(
        done.attempts.iter().map(|a| (a.number, a.state.clone(), a.outcome.clone())).collect::<Vec<_>>(),
        [(1, "interrupted".into(), Some("interrupted".into())), (2, "paused".into(), None), (3, "succeeded".into(), Some("succeeded".into()))]
    );
    let attempts_log = logs(&home, crawl.id);
    let second = attempts_log.split("[bctl] attempt 2:").nth(1).expect("the log records the second attempt");
    let visited = second.split("resume: loaded state").nth(1).and_then(|rest| rest.split('(').nth(1)).and_then(|rest| rest.split(' ').next()).and_then(|n| n.parse::<u32>().ok());
    assert!(visited.is_some_and(|n| n >= 2), "attempt 2 did not continue from state.db:\n{second}");
    assert_eq!((fixture.hits("/chain/0.html"), fixture.hits("/chain/1.html")), (first_page_hits, second_page_hits), "pages finished before the kill were crawled again");
    let mirror = Path::new(&done.output_dir).join(HOST_FROM_GUEST);
    for n in 0..CHAIN_PAGES {
        assert!(read_text(mirror.join(format!("chain/{n}.html/index.html"))).contains(&format!("chain {n}")), "page {n} missing from the mirror");
    }
    assert!(browser_answers(&home.cdp_url(&session)), "the session's browser did not outlive the run");
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn exec_runs_end_with_categorized_outcomes_and_never_outlive_cancel_timeout_or_a_daemon_kill() {
    let home = Home::new("j3-e");
    let session = format!("{}-exec", home.prefix());
    let image = image();
    let config = write_config(
        &home,
        &format!(
            r#"version = 1
[browsers.small]
image = "{image}"
viewport = [1024, 768]
cpus = 1
memory_mib = 2048

[jobs.version]
kind = "exec"
browser = "small"
profile = "{session}"
argv = ["sh", "-c", "curl -fsS \"$BCTL_CDP_URL/json/version\" > \"$BCTL_OUTPUT_DIR/version.json\""]

[jobs.fails]
kind = "exec"
browser = "small"
profile = "{session}"
argv = ["sh", "-c", "echo 'about to fail' >&2; exit 3"]

[jobs.lingers]
kind = "exec"
browser = "small"
profile = "{session}"
argv = ["sh", "-c", "echo $$ > \"$BCTL_OUTPUT_DIR/leader\"; sleep 600 & echo $! > \"$BCTL_OUTPUT_DIR/child\"; wait"]

[jobs.slow]
kind = "exec"
browser = "small"
profile = "{session}"
argv = ["sleep", "60"]
timeout = "2s"
"#
        ),
    );
    let config = config.to_str().unwrap();
    let job = |name: &str| started(&home, &["run", name, "--session", &session, "--config", config]);
    let ended = |id: i64| wait_for(&home, id, "ended", Duration::from_secs(60), |r| !matches!(r.state.as_str(), "starting" | "running"));

    let version = ended(job("version").id);
    assert_eq!(version.outcome.as_deref(), Some("succeeded"), "{version:?}\n{}", logs(&home, version.id));
    assert!(read_text(Path::new(&version.output_dir).join("version.json")).contains(&version.browser_version), "the job reached a different browser");
    let inspected: Inspected = json::from_str(&String::from_utf8_lossy(&home.bctl(&["inspect", &session, "--json"]).stdout)).unwrap();
    let resources = &inspected.sessions[0].resources;
    assert_eq!((resources.width, resources.height, resources.cpus, resources.memory_mib), (1024, 768, 1, 2048));
    let mut viewer = bctl_rfb::Client::connect(("127.0.0.1", inspected.sessions[0].vnc_port.unwrap()), Duration::from_secs(5)).unwrap();
    let frame = viewer.refresh().unwrap();
    assert_eq!((frame.width(), frame.height()), (1024, 768), "the session did not launch with the job's viewport");
    assert_eq!(config_field(&version.config, &["exec", "argv"]).map(json::to_string).as_deref(), Some(r#"["sh","-c","curl -fsS \"$BCTL_CDP_URL/json/version\" > \"$BCTL_OUTPUT_DIR/version.json\""]"#));
    assert!(version.image_digest.starts_with("sha256:") && version.input_digest.starts_with("sha256:"));

    let fails = ended(job("fails").id);
    assert_eq!((fails.state.as_str(), fails.outcome.as_deref()), ("failed", Some("job-error")));
    assert!(fails.detail.as_deref().is_some_and(|d| d.contains("status 3")), "{:?}", fails.detail);
    assert!(logs(&home, fails.id).contains("about to fail"));

    let slow = ended(job("slow").id);
    assert_eq!(slow.outcome.as_deref(), Some("job-error"));
    assert!(slow.detail.as_deref().is_some_and(|d| d.contains("timed out after 2s")), "{:?}", slow.detail);

    let lingers = job("lingers");
    let pid_file = |name: &str| Path::new(&lingers.output_dir).join(name);
    wait_for(&home, lingers.id, "writing its pids", Duration::from_secs(30), |_| pid_file("child").is_file() && !read_text(pid_file("child")).trim().is_empty());
    assert_eq!(error_code(&home, &["run", "version", "--session", &session, "--config", config]).as_deref(), Some("RUN_ACTIVE"));
    assert_eq!(error_code(&home, &["pause", &lingers.id.to_string()]).as_deref(), Some("NOT_PAUSABLE"));
    assert_ok(&home.bctl(&["cancel", &lingers.id.to_string()]));
    let cancelled = ended(lingers.id);
    assert_eq!((cancelled.state.as_str(), cancelled.outcome.as_deref()), ("cancelled", Some("cancelled")));
    for name in ["leader", "child"] {
        let pid = read_text(pid_file(name)).trim().to_string();
        assert!(!process_alive(&pid), "cancel left the {name} process {pid} running");
    }

    let orphaned = job("lingers");
    let orphan_pid = |name: &str| read_text(Path::new(&orphaned.output_dir).join(name)).trim().to_string();
    wait_for(&home, orphaned.id, "writing its pids", Duration::from_secs(30), |r| {
        let child = Path::new(&r.output_dir).join("child");
        child.is_file() && !read_text(child).trim().is_empty()
    });
    kill_daemon(&home);
    assert!(process_alive(&orphan_pid("child")), "the job died with the daemon; this test needs it to outlive it");
    let interrupted = run(&home, orphaned.id);
    assert_eq!((interrupted.state.as_str(), interrupted.outcome.as_deref()), ("interrupted", Some("interrupted")));
    for name in ["leader", "child"] {
        assert!(!process_alive(&orphan_pid(name)), "the restarted daemon left the {name} process running");
    }
    assert_eq!(error_code(&home, &["resume", &orphaned.id.to_string()]).as_deref(), Some("NOT_RESUMABLE"));
    assert_eq!(run(&home, orphaned.id).attempts.len(), 1);
    assert_eq!(interrupted.attempts[0].state, "interrupted");
}
