use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use bctl_rfb::{Client, Rgb};
use miniserde::{Deserialize, json};

use crate::cdp::Endpoint;
use crate::browser::{HOST_FROM_GUEST, HOST_RANGE};
use crate::host::{self, Fallible, Runtime, Verdict};
use crate::net::{self, WebSocket};
use crate::report::Samples;
use crate::rfb::{Probe, Viewer};
use crate::session::{Home, session_name};
use crate::serve;
use crate::stats::Statistic;
use crate::{Context, Outcome};

const TODOS: usize = 5;
const TODO_APP: &str = "speedometer/resources/todomvc/architecture-examples/react/dist/index.html";
const NEW_TODO: &str = "What needs to be done?";
const STEPS: [&str; 5] = ["goto", "fill", "press", "check", "snapshot"];
const CALLS_PER_BLOCK: usize = 100;
const WARMUP_CALLS: usize = 10;
const TRIALS_PER_BLOCK: usize = 20;
const KEY_A: u32 = 0x61;
const MARKER: Rgb = Rgb(255, 0, 255);
const PAGE: &str = "<body style=\"margin:0;min-height:100vh;background:#00aa00\"><div style=\"position:fixed;left:0;top:0;width:24px;height:24px;background:#ff00ff\"></div><script>let on=false;window.flips=0;const flip=()=>{window.flips++;on=!on;document.body.style.background=on?'#0000aa':'#00aa00'};addEventListener('pointerdown',flip);addEventListener('keydown',flip)</script></body>";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Path {
    Direct,
    Gateway,
}

impl Path {
    fn name(self) -> &'static str {
        match self {
            Path::Direct => "direct",
            Path::Gateway => "gateway",
        }
    }
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e6
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

struct Series {
    path: Path,
    label: &'static str,
    samples: Samples,
}

fn find<'a>(series: &'a mut [Series], path: Path, label: &str) -> &'a mut Samples {
    &mut series.iter_mut().find(|s| s.path == path && s.label == label).expect("series exists").samples
}

fn direct_socket(home: &Home) -> Fallible<PathBuf> {
    let mut sockets = std::fs::read_dir(home.path.join("cdp")).map_err(|e| e.to_string())?.flatten().map(|d| d.path().join("s"));
    sockets.next().filter(|_| sockets.next().is_none()).ok_or_else(|| "expected exactly one exposed CDP socket".into())
}

fn cdp_block(endpoint: &Endpoint) -> Fallible<Vec<f64>> {
    let (_, mut page) = endpoint.attach_first_page()?;
    for _ in 0..WARMUP_CALLS {
        page.evaluate("1")?;
    }
    let mut calls = Vec::with_capacity(CALLS_PER_BLOCK);
    for _ in 0..CALLS_PER_BLOCK {
        let started = Instant::now();
        page.evaluate("1")?;
        calls.push(micros(started.elapsed()));
    }
    Ok(calls)
}

fn timed(steps: &mut [Vec<f64>; 5], step: &str, run: impl FnOnce() -> Fallible<String>) -> Fallible<String> {
    let started = Instant::now();
    let output = run()?;
    let at = STEPS.iter().position(|s| *s == step).expect("known step");
    steps[at].push(millis(started.elapsed()));
    Ok(output)
}

fn todo_workflow(home: &Home, session: &str, url: &str) -> Fallible<([Vec<f64>; 5], f64)> {
    let mut steps: [Vec<f64>; 5] = Default::default();
    let started = Instant::now();
    timed(&mut steps, "goto", || home.page(&["goto", session, url, "--wait", "load"]))?;
    for todo in 0..TODOS {
        let text = format!("todo {todo}");
        timed(&mut steps, "fill", || home.page(&["fill", session, "--placeholder", NEW_TODO, &text]))?;
        timed(&mut steps, "press", || home.page(&["press", session, "--placeholder", NEW_TODO, "Enter"]))?;
    }
    timed(&mut steps, "check", || home.page(&["check", session, "--css", ".todo-list li .toggle", "--nth", "1"]))?;
    let snapshot = timed(&mut steps, "snapshot", || home.page(&["snapshot", session]))?;
    let total = millis(started.elapsed());
    let state = home.page(&["eval", session, "[document.querySelectorAll('.todo-list li').length, document.querySelectorAll('.todo-list li.completed').length].join('/')"])?;
    if state.trim_matches('"') != format!("{TODOS}/1") {
        return Err(format!("the todo app holds {state} items/completed after the workflow, not {TODOS}/1"));
    }
    let items: Vec<&str> = snapshot.lines().filter(|l| l.starts_with("- checkbox [")).collect();
    if items.len() != TODOS || items.iter().filter(|l| l.ends_with(", checked]")).count() != 1 {
        return Err(format!("the snapshot does not show {TODOS} todo checkboxes with one checked: {snapshot}"));
    }
    Ok((steps, total))
}

#[derive(Deserialize)]
struct ControlState {
    authority: String,
    mine: bool,
}

struct Dashboard {
    port: u16,
    cookie: String,
}

fn sign_in(home: &Home, session: &str) -> Fallible<Dashboard> {
    let login = home.login_url()?;
    let rest = login.strip_prefix("http://127.0.0.1:").ok_or("login url is not on 127.0.0.1")?;
    let (port, path) = rest.split_once('/').ok_or("login url has no path")?;
    let port: u16 = port.parse().map_err(|_| "login port")?;
    let host = format!("127.0.0.1:{port}");
    let mut stream = net::tcp("127.0.0.1", port)?;
    let response = net::request(&mut stream, "GET", &host, &format!("/{path}"), &[], b"")?;
    let cookie = response.header("set-cookie").and_then(|c| c.split(';').next()).ok_or_else(|| format!("login answered {} without a cookie", response.status))?.to_string();
    let dashboard = Dashboard { port, cookie };
    take_control(&dashboard, session)?;
    Ok(dashboard)
}

fn take_control(dashboard: &Dashboard, session: &str) -> Fallible<()> {
    let host = format!("127.0.0.1:{}", dashboard.port);
    let origin = format!("http://{host}");
    let mut stream = net::tcp("127.0.0.1", dashboard.port)?;
    let control = net::request(&mut stream, "POST", &host, &format!("/api/sessions/{session}/control"), &[("Cookie", &dashboard.cookie), ("Origin", &origin)], b"")?;
    if control.status != 200 {
        return Err(format!("taking control answered {}: {}", control.status, control.text()));
    }
    let state: ControlState = json::from_str(&control.text()).map_err(|_| format!("control answered {:?}", control.text()))?;
    if state.authority != "human" || !state.mine {
        return Err(format!("taking control left authority {} (mine: {})", state.authority, state.mine));
    }
    Ok(())
}

fn connect_viewer(path: Path, vnc_port: u16, dashboard: &Dashboard, session: &str) -> Fallible<Viewer> {
    match path {
        Path::Direct => Viewer::handshake(net::tcp("127.0.0.1", vnc_port)?),
        Path::Gateway => {
            take_control(dashboard, session)?;
            let host = format!("127.0.0.1:{}", dashboard.port);
            let origin = format!("http://{host}");
            let socket = WebSocket::upgrade(net::tcp("127.0.0.1", dashboard.port)?, &host, &format!("/api/sessions/{session}/vnc"), &[("Cookie", &dashboard.cookie), ("Origin", &origin)])?;
            Viewer::handshake(Box::new(socket))
        }
    }
}

fn input_block(viewer: &mut Viewer, probe: &Probe, click: (u16, u16)) -> Fallible<(Vec<f64>, Vec<f64>, usize)> {
    let mut known = viewer.settle(probe)?;
    let (mut pointer, mut key, mut failures) = (Vec::new(), Vec::new(), 0);
    for trial in 0..TRIALS_PER_BLOCK * 2 {
        let is_pointer = trial % 2 == 0;
        let result = viewer.time_to_change(probe, known, Duration::from_secs(3), |v| if is_pointer { v.press_pointer(click.0, click.1) } else { v.tap_key(KEY_A) });
        match result {
            Ok((elapsed, pixel)) => {
                known = pixel;
                if is_pointer { pointer.push(millis(elapsed)) } else { key.push(millis(elapsed)) }
            }
            Err(_) => {
                failures += 1;
                known = viewer.settle(probe)?;
            }
        }
        if is_pointer {
            viewer.release_pointer(click.0, click.1)?;
        }
        thread::sleep(Duration::from_millis(400));
    }
    if pointer.is_empty() && key.is_empty() {
        return Err("no input trial changed the probe pixel".into());
    }
    Ok((pointer, key, failures))
}

pub fn run(ctx: &Context) -> Fallible<Outcome> {
    let runtime = Runtime::current()?;
    let image = runtime.image();
    let mut outcome = Outcome { images: vec![image], ..Outcome::default() };
    let home = Home::new("inter")?;
    home.start_daemon()?;
    let session = session_name("inter");
    let server = serve::start(host::root().join(crate::browser::SPEEDOMETER_DIR))?;
    home.up(&session, &session, image, &[HOST_RANGE.to_string()])?;
    let vnc_port = if runtime == Runtime::SmolVm { Some(home.session(&session)?.vnc_port.ok_or("session has no VNC port")?) } else { None };
    if runtime == Runtime::Nanos {
        outcome.notes.push("input to pixel needs a display; the nanos runtime is headless".into());
    }
    let gateway_url = home.cdp_url(&session)?;
    let host = gateway_url.strip_prefix("http://").ok_or("cdp url is not http")?.to_string();
    let port: u16 = host.rsplit(':').next().and_then(|p| p.parse().ok()).ok_or("cdp url has no port")?;
    let gateway = Endpoint::Tcp { port, host };
    let direct = Endpoint::Unix { path: direct_socket(&home)? };
    outcome.browser_versions.push(gateway.browser_version()?);

    let mut rtt: Vec<Series> = Vec::new();
    let mut latency: Vec<Series> = Vec::new();
    for path in [Path::Direct, Path::Gateway] {
        rtt.push(Series { path, label: "Runtime.evaluate", samples: Samples::new() });
        for label in ["pointer", "key"] {
            latency.push(Series { path, label, samples: Samples::new() });
        }
    }
    let paths = |round: usize| if round.is_multiple_of(2) { [Path::Direct, Path::Gateway] } else { [Path::Gateway, Path::Direct] };

    for round in 0..ctx.rounds {
        if ctx.gate.gave_up() {
            outcome.notes.push(format!("perf gate deferred: host busy; CDP blocks stopped before round {round} of {}", ctx.rounds));
            break;
        }
        for path in paths(round) {
            let endpoint = if path == Path::Direct { &direct } else { &gateway };
            match ctx.gate.run(|| cdp_block(endpoint)) {
                Ok(Some((calls, _, verdict))) => {
                    eprintln!("interaction round {round}: cdp {} {} calls ({verdict:?})", path.name(), calls.len());
                    let samples = find(&mut rtt, path, "Runtime.evaluate");
                    match verdict {
                        Verdict::Clean => calls.into_iter().for_each(|c| samples.push(c)),
                        other => samples.discard(None, format!("{other:?}: block of {} calls dropped", calls.len())),
                    }
                }
                Ok(None) => {
                    outcome.notes.push(format!("perf gate deferred: host busy; stopped during round {round}"));
                    break;
                }
                Err(e) => outcome.notes.push(format!("cdp block {} round {round} failed: {e}", path.name())),
            }
        }
    }

    let todo_url = format!("http://{HOST_FROM_GUEST}:{}/{TODO_APP}", server.port);
    let mut steps: [Samples; 5] = std::array::from_fn(|_| Samples::new());
    let mut workflow = Samples::new();
    home.open_tab(&session, "about:blank")?;
    for round in 0..ctx.rounds {
        if ctx.gate.gave_up() {
            outcome.notes.push(format!("perf gate deferred: host busy; automation workflows stopped before round {round} of {}", ctx.rounds));
            break;
        }
        match ctx.gate.run(|| todo_workflow(&home, &session, &todo_url)) {
            Ok(Some(((timings, total), _, verdict))) => {
                eprintln!("interaction round {round}: todo workflow {total:.0} ms ({verdict:?})");
                workflow.record(total, verdict);
                for (samples, values) in steps.iter_mut().zip(timings) {
                    values.into_iter().for_each(|v| samples.record(v, verdict));
                }
            }
            Ok(None) => {
                outcome.notes.push(format!("perf gate deferred: host busy; stopped during round {round}"));
                break;
            }
            Err(e) => outcome.notes.push(format!("todo workflow round {round} failed: {e}")),
        }
    }

    if runtime == Runtime::SmolVm {
        let vnc_port = vnc_port.ok_or("smolvm reports a VNC port")?;
        let (_, mut page) = gateway.attach_first_page()?;
        page.call("Page.bringToFront", "{}")?;
        page.navigate(&format!("data:text/html;charset=utf-8,{}", PAGE.replace('#', "%23")))?;
        drop(page);
        thread::sleep(Duration::from_secs(2));
        let origin = {
            let mut client = Client::connect(("127.0.0.1", vnc_port), Duration::from_secs(10)).map_err(|e| e.to_string())?;
            client.refresh().map_err(|e| e.to_string())?.find(MARKER).ok_or("the fixture marker is not on screen")?
        };
        let click = (origin.0 + 100, origin.1 + 100);
        let probe = Probe { x: origin.0 + 300, y: origin.1 + 300 };
        let dashboard = sign_in(&home, &session)?;
        let mut failures = 0;
        for round in 0..ctx.rounds {
            if ctx.gate.gave_up() {
                outcome.notes.push(format!("perf gate deferred: host busy; input blocks stopped before round {round} of {}", ctx.rounds));
                break;
            }
            for path in paths(round) {
                let mut viewer = connect_viewer(path, vnc_port, &dashboard, &session)?;
                let block = ctx.gate.run(|| input_block(&mut viewer, &probe, click));
                drop(viewer);
                thread::sleep(Duration::from_secs(1));
                match block {
                    Ok(Some(((pointer, key, failed), _, verdict))) => {
                        eprintln!("interaction round {round}: input {} {} pointer / {} key ({verdict:?})", path.name(), pointer.len(), key.len());
                        failures += failed;
                        for (label, values) in [("pointer", pointer), ("key", key)] {
                            let samples = find(&mut latency, path, label);
                            for value in values {
                                samples.record(value, verdict);
                            }
                        }
                    }
                    Ok(None) => {
                        outcome.notes.push(format!("perf gate deferred: host busy; stopped during round {round}"));
                        break;
                    }
                    Err(e) => outcome.notes.push(format!("input block {} round {round} failed: {e}", path.name())),
                }
            }
        }
        let (_, mut page) = direct.attach_first_page()?;
        let flips = page.evaluate("window.flips")?;
        let attempted = ctx.rounds * 2 * TRIALS_PER_BLOCK * 2;
        outcome.notes.push(format!("the page counted {flips} toggles for {attempted} attempted input trials; {failures} trials saw no pixel change within 3 s and are excluded"));
    }

    for series in &rtt {
        let name = format!("interaction.cdp_rtt.{}.{}", series.path.name(), series.label);
        let conditions = "one sequential call at a time on one WebSocket, 10 warm-up calls, blocks of 100 calls; gateway = capability URL on the daemon's loopback listener, direct = the exposed Unix socket, which bypasses the gateway; both include the guest relay and Chromium";
        outcome.measures.push(series.samples.finish(&format!("{name}.p50"), "us", Statistic::Median, conditions));
        outcome.measures.push(series.samples.finish(&format!("{name}.p95"), "us", Statistic::P95, conditions));
    }
    let automation = format!("bctl page commands from a host process on Speedometer 3.1's React TodoMVC, served from the host: goto with --wait load, then {TODOS} times fill and press Enter on the new-todo field, check the first item, snapshot; each command includes the CLI process, the daemon, the gateway's CDP session and the guest");
    for (step, samples) in STEPS.iter().zip(&steps) {
        outcome.measures.push(samples.finish(&format!("interaction.automation.{step}.p50"), "ms", Statistic::Median, &automation));
        outcome.measures.push(samples.finish(&format!("interaction.automation.{step}.p95"), "ms", Statistic::P95, &automation));
    }
    outcome.measures.push(workflow.finish("interaction.automation.todo_workflow", "ms", Statistic::Median, &automation));
    if runtime == Runtime::SmolVm {
        for series in &latency {
            let name = format!("interaction.input_to_pixel.{}.{}", series.path.name(), series.label);
            let conditions = format!("RFB pointer press or key tap written to the stream until an update repaints a probe pixel of a full-viewport color toggle; {image} 1280x800; direct = the VMM's VNC port, gateway = dashboard WebSocket with the input lease; 400 ms between trials");
            outcome.measures.push(series.samples.finish(&format!("{name}.p50"), "ms", Statistic::Median, &conditions));
            outcome.measures.push(series.samples.finish(&format!("{name}.p95"), "ms", Statistic::P95, &conditions));
        }
    }
    Ok(outcome)
}
