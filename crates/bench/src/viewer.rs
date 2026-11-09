use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use miniserde::{Deserialize, json};

use crate::cdp::{Cdp, Endpoint};
use crate::host::{self, Fallible};
use crate::net::{self, WebSocket};
use crate::report::{self, Samples};
use crate::rfb::Viewer;
use crate::session::{Home, session_name};
use crate::stats::Statistic;
use crate::{Context, Outcome};

const IMAGE: &str = "helium";
const WINDOW_SECONDS: usize = 10;
const WINDOW: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(200);
const PROBES: usize = 20;
const ANCHOR: [u8; 3] = [255, 0, 255];
const SCENARIOS: [&str; 3] = ["marker", "fullscreen", "scroll"];

const PAGE: &str = r#"<body style='margin:0;height:100000px;background:rgb(0,68,0)'><script>(()=>{const t0=performance.timeOrigin;window.frameTimes=[];window.mode='marker';let counter=0,scroll=0,lastMode='';const content=document.createElement('div');content.style.cssText='position:absolute;left:0;top:0;width:100%;height:100000px;background:repeating-linear-gradient(0deg,rgb(30,30,90) 0px,rgb(30,30,90) 20px,rgb(90,30,30) 20px,rgb(90,30,30) 40px)';content.style.display='none';document.body.appendChild(content);const strip=document.createElement('div');strip.style.cssText='position:fixed;left:0;top:0;display:flex;z-index:9';const cells=[];for(let i=0;i<17;i++){const d=document.createElement('div');d.style.cssText='width:16px;height:16px;background:rgb(0,0,0);flex:0 0 auto';strip.appendChild(d);cells.push(d)}cells[0].style.background='rgb(255,0,255)';document.body.appendChild(strip);window.reset=()=>{counter=0;window.frameTimes=[];scroll=0};const frame=()=>{counter++;window.frameTimes[counter]=t0+performance.now();const v=counter&65535;for(let b=0;b<16;b++){cells[b+1].style.background=((v>>b)&1)?'rgb(255,255,255)':'rgb(0,0,0)'}if(window.mode!==lastMode){lastMode=window.mode;content.style.display=(window.mode==='scroll')?'block':'none';if(window.mode!=='fullscreen'){document.body.style.background='rgb(0,68,0)'}}if(window.mode==='fullscreen'){const c=counter&255;document.body.style.background='rgb('+c+','+((counter*3)&255)+','+((counter*7)&255)+')'}if(window.mode==='scroll'){scroll+=4;if(scroll>90000){scroll=0}window.scrollTo(0,scroll)}requestAnimationFrame(frame)};requestAnimationFrame(frame)})()</script></body>"#;

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

    fn conditions(self) -> &'static str {
        match self {
            Path::Direct => "libkrun's host VNC TCP port, 127.0.0.1 (direct)",
            Path::Gateway => "dashboard WebSocket /api/sessions/{s}/vnc with the input lease",
        }
    }
}

struct Cell {
    path: Path,
    scenario: &'static str,
    fps: Samples,
    worst: Samples,
    latency: Samples,
    bandwidth: Samples,
    missed: Samples,
}

struct Window {
    per_second: Vec<f64>,
    latency: Vec<f64>,
    bandwidth: f64,
    missed: f64,
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1e3).unwrap_or(0.0)
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

fn clock_offset(page: &mut Cdp) -> Fallible<(f64, f64)> {
    let mut best: Option<(f64, f64)> = None;
    for _ in 0..PROBES {
        let before = now_ms();
        let guest: f64 = page.evaluate("String(performance.timeOrigin+performance.now())")?.parse().map_err(|_| "guest clock is not a number")?;
        let after = now_ms();
        let round_trip = after - before;
        let offset = guest - (before + after) / 2.0;
        if best.is_none_or(|(shortest, _)| round_trip < shortest) {
            best = Some((round_trip, offset));
        }
    }
    let (round_trip, offset) = best.ok_or("no clock probe ran")?;
    Ok((offset, round_trip / 2.0))
}

fn initial_sync(viewer: &mut Viewer, scale: f64) -> Fallible<(u16, u16)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() > deadline {
            return Err("the marker anchor did not appear within 10 s".into());
        }
        viewer.request(false)?;
        while !viewer.read_update_message()? {}
        if let Some(origin) = viewer.find(ANCHOR) {
            viewer.decode_strip(origin, scale).ok_or("the marker strip is not on screen")?;
            return Ok(origin);
        }
    }
}

fn measure_window(viewer: &mut Viewer, origin: (u16, u16), scale: f64, page: &mut Cdp, offset: f64) -> Fallible<Window> {
    let mut last = viewer.decode_strip(origin, scale).ok_or("the marker strip is not on screen")?;
    let start = Instant::now();
    let bytes_start = viewer.bytes_received();
    let mut seen: BTreeMap<u16, f64> = BTreeMap::new();
    let mut per_second = vec![0usize; WINDOW_SECONDS];
    let mut first: Option<u16> = None;
    let mut last_seen = last;
    while start.elapsed() < WINDOW {
        viewer.request(true)?;
        if viewer.read_update_message()? {
            if let Some(value) = viewer.decode_strip(origin, scale) {
                if value != last {
                    last = value;
                    let at = start.elapsed().as_secs_f64();
                    if at <= WINDOW_SECONDS as f64 && seen.insert(value, now_ms()).is_none() {
                        let bucket = (at.floor() as usize).min(WINDOW_SECONDS - 1);
                        per_second[bucket] += 1;
                        first.get_or_insert(value);
                        last_seen = value;
                    }
                }
            }
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let bytes = viewer.bytes_received().saturating_sub(bytes_start);
    let first = first.ok_or("the viewer decoded no marker while measuring")?;
    let drawn = i32::from(last_seen) - i32::from(first) + 1;
    let missed = if drawn > 0 { 1.0 - seen.range(first..=last_seen).count() as f64 / f64::from(drawn) } else { 0.0 };
    let times_json = page.evaluate(&format!("JSON.stringify(window.frameTimes.slice({first},{last_seen}+1))"))?;
    let times: Vec<f64> = json::from_str(&times_json).map_err(|_| "the page returned no frame times".to_string())?;
    let mut latency = Vec::with_capacity(seen.len());
    for (&counter, &host_seen) in &seen {
        let guest = times.get(usize::from(counter - first)).copied().ok_or_else(|| format!("the page has no time for marker {counter}"))?;
        latency.push(host_seen - (guest - offset));
    }
    Ok(Window { per_second: per_second.iter().map(|&n| n as f64).collect(), latency, bandwidth: bytes as f64 / elapsed, missed })
}

fn median(values: &[f64]) -> f64 {
    let sorted = crate::stats::sorted(values);
    Statistic::Median.of(&sorted)
}

fn cell<'a>(cells: &'a mut [Cell], path: Path, scenario: &str) -> &'a mut Cell {
    cells.iter_mut().find(|c| c.path == path && c.scenario == scenario).expect("cell exists")
}

pub fn run(ctx: &Context) -> Fallible<Outcome> {
    let mut outcome = Outcome { images: vec![IMAGE], ..Outcome::default() };
    let home = Home::new("viewer")?;
    let tar = host::root().join(".work/images/helium.tar");
    let link = home.path.join("images/helium.tar");
    if let Err(e) = std::os::unix::fs::symlink(&tar, &link)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(format!("{}: {e}", link.display()));
    }
    home.start_daemon()?;
    let session = session_name("viewer");
    home.up(&session, &session, IMAGE, &[])?;
    let view = home.session(&session)?;
    let vnc_port = view.vnc_port.ok_or("session has no VNC port")?;
    let gateway_url = home.cdp_url(&session)?;
    let host = gateway_url.strip_prefix("http://").ok_or("cdp url is not http")?.to_string();
    let port: u16 = host.rsplit(':').next().and_then(|p| p.parse().ok()).ok_or("cdp url has no port")?;
    let endpoint = Endpoint::Tcp { port, host };
    outcome.browser_versions.push(endpoint.browser_version()?);

    let (_, mut page) = endpoint.attach_first_page()?;
    page.call("Page.bringToFront", "{}")?;
    page.navigate(&format!("data:text/html;charset=utf-8,{PAGE}"))?;
    thread::sleep(Duration::from_secs(2));
    let scale: f64 = page.evaluate("String(window.devicePixelRatio)")?.parse().map_err(|_| "devicePixelRatio is not a number")?;
    let (offset, uncertainty) = clock_offset(&mut page)?;
    outcome.measures.push(report::fact("viewer.clock_offset", "ms", offset, "guest clock minus host clock, from the minimum-RTT probe of 20 bracketed Runtime.evaluate calls over CDP"));
    outcome.measures.push(report::fact("viewer.clock_uncertainty", "ms", uncertainty, "half the minimum round trip of the 20 clock probes"));
    outcome.notes.push(format!("clock offset {offset:.3} ms, uncertainty {uncertainty:.3} ms from {PROBES} probes"));
    let dashboard = sign_in(&home, &session)?;

    let mut cells: Vec<Cell> = Vec::new();
    for path in [Path::Direct, Path::Gateway] {
        for scenario in SCENARIOS {
            cells.push(Cell { path, scenario, fps: Samples::new(), worst: Samples::new(), latency: Samples::new(), bandwidth: Samples::new(), missed: Samples::new() });
        }
    }
    let paths = |round: usize| if round.is_multiple_of(2) { [Path::Direct, Path::Gateway] } else { [Path::Gateway, Path::Direct] };
    let mut completed = 0;
    let mut attempted = 0;

    'rounds: for round in 0..ctx.rounds {
        if ctx.gate.gave_up() {
            outcome.notes.push(format!("perf gate deferred: host busy; viewer rounds stopped before round {round} of {}", ctx.rounds));
            break;
        }
        for path in paths(round) {
            for scenario in SCENARIOS {
                if ctx.gate.gave_up() {
                    outcome.notes.push(format!("perf gate deferred: host busy; viewer windows stopped before {scenario} round {round} of {}", ctx.rounds));
                    break 'rounds;
                }
                attempted += 1;
                let result = ctx.gate.run(|| {
                    page.evaluate(&format!("window.mode='{scenario}'"))?;
                    thread::sleep(SETTLE);
                    page.evaluate("window.reset()")?;
                    let mut viewer = connect_viewer(path, vnc_port, &dashboard, &session)?;
                    let origin = initial_sync(&mut viewer, scale)?;
                    let window = measure_window(&mut viewer, origin, scale, &mut page, offset)?;
                    drop(viewer);
                    page.evaluate("window.mode='marker'")?;
                    Ok(window)
                });
                if path == Path::Gateway {
                    thread::sleep(Duration::from_secs(1));
                }
                match result {
                    Ok((window, _, verdict)) => {
                        completed += 1;
                        let fps = median(&window.per_second);
                        let worst = window.per_second.iter().copied().fold(f64::INFINITY, f64::min);
                        eprintln!(
                            "viewer round {round}: {scenario} {} {fps:.1} fps (worst {worst:.1}), {} latency samples, {:.0} bytes/s, {:.1}% missed ({verdict:?})",
                            path.name(),
                            window.latency.len(),
                            window.bandwidth,
                            window.missed * 100.0
                        );
                        let cell = cell(&mut cells, path, scenario);
                        cell.fps.record(fps, verdict);
                        cell.worst.record(worst, verdict);
                        for value in window.latency {
                            cell.latency.record(value, verdict);
                        }
                        cell.bandwidth.record(window.bandwidth, verdict);
                        cell.missed.record(window.missed, verdict);
                    }
                    Err(e) => outcome.notes.push(format!("viewer {scenario} {} round {round} failed: {e}", path.name())),
                }
            }
        }
    }

    for cell in &cells {
        let name = format!("viewer.{}.{}", cell.scenario, cell.path.name());
        let conditions = format!(
            "{}; {scenario}; raw encoding only; one fixed {WINDOW_SECONDS} s window per round; incremental updates requested back to back; latency subtracts the clock offset and its uncertainty is reported separately",
            cell.path.conditions(),
            scenario = cell.scenario
        );
        outcome.measures.push(cell.fps.finish(&format!("{name}.fps"), "fps", Statistic::Median, &conditions));
        outcome.measures.push(cell.worst.finish(&format!("{name}.fps.worst_second"), "fps", Statistic::Min, &conditions));
        outcome.measures.push(cell.latency.finish(&format!("{name}.latency.p50"), "ms", Statistic::Median, &conditions));
        outcome.measures.push(cell.latency.finish(&format!("{name}.latency.p95"), "ms", Statistic::P95, &conditions));
        outcome.measures.push(cell.bandwidth.finish(&format!("{name}.bandwidth"), "bytes/s", Statistic::Median, &conditions));
        outcome.measures.push(cell.missed.finish(&format!("{name}.missed"), "fraction", Statistic::Median, &conditions));
    }
    outcome.notes.push(format!("{completed} of {attempted} viewer windows retained clean samples"));
    outcome.notes.push("the VNC server offers Raw as its only pixel encoding, plus the DesktopSize, Cursor, and ContinuousUpdates pseudo-encodings; this suite asks for Raw and DesktopSize".into());
    Ok(outcome)
}
