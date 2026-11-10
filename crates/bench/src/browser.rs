use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use miniserde::{Deserialize, json};

use crate::cdp::{Cdp, Endpoint};
use crate::host::{self, Fallible, output_of};
use crate::report::{Samples, fact};
use crate::serve;
use crate::session::{Home, session_name};
use crate::stats::Statistic;
use crate::{Context, Outcome};

const STALL_TIMEOUT: Duration = Duration::from_secs(120);
const SPEEDOMETER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const LOADS_PER_BATCH: usize = 5;
pub const SPEEDOMETER_DIR: &str = ".work/dl/speedometer/src";
const CONTAINER_IMAGE: &str = "bctl-bench/chromium";
pub const HOST_RANGE: &str = "100.96.0.0/30";
pub const HOST_FROM_GUEST: &str = "100.96.0.1";
const GUEST_IMAGE: &str = "helium";
const NEWS_SITE: &str = "speedometer/resources/newssite/news-next/dist/index.html";
const THROTTLE_FLAGS: [&str; 3] = ["--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding", "--disable-background-timer-throttling"];

struct Target {
    label: String,
    base_url: String,
    version: String,
    page: Cdp,
    _guard: Guard,
}

enum Guard {
    None,
    Container(String),
}

impl Drop for Guard {
    fn drop(&mut self) {
        match self {
            Guard::None => {}
            Guard::Container(name) => {
                let _ = Command::new("docker").args(["rm", "-f", name]).stdout(Stdio::null()).stderr(Stdio::null()).status();
            }
        }
    }
}

fn wait_for_cdp(endpoint: &Endpoint) -> Fallible<String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match endpoint.browser_version() {
            Ok(version) => return Ok(version),
            Err(e) if Instant::now() > deadline => return Err(format!("browser never answered CDP: {e}")),
            Err(_) => thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn attach(label: &str, endpoint: Endpoint, base_url: String, guard: Guard) -> Fallible<Target> {
    let version = wait_for_cdp(&endpoint)?;
    let (_, mut page) = endpoint.attach_first_page()?;
    page.call("Page.enable", "{}")?;
    page.call("Page.bringToFront", "{}")?;
    page.call("Network.enable", "{}")?;
    page.call("Network.setCacheDisabled", r#"{"cacheDisabled":true}"#)?;
    Ok(Target { label: label.into(), base_url, version, page, _guard: guard })
}

fn guest_target(home: &Home, image: &str, server_port: u16, notes: &mut Vec<String>) -> Fallible<Target> {
    let name = session_name(&format!("br-{image}"));
    home.up(&name, &name, image, &[HOST_RANGE.to_string()])?;
    let url = home.cdp_url(&name)?;
    let host = url.strip_prefix("http://").ok_or("cdp url is not http")?.to_string();
    let port: u16 = host.rsplit(':').next().and_then(|p| p.parse().ok()).ok_or("cdp url has no port")?;
    notes.push(format!("{image}: guest reaches the benchmark server at {HOST_FROM_GUEST}:{server_port}, SmolVM's gateway to the host loopback, through --allow-private {HOST_RANGE}"));
    attach(&format!("guest-{image}"), Endpoint::Tcp { port, host }, format!("http://{HOST_FROM_GUEST}:{server_port}"), Guard::None)
}

fn container_target(server_port: u16, notes: &mut Vec<String>) -> Fallible<Target> {
    let name = format!("h1-chromium-{}", std::process::id());
    output_of("docker", &["build", "-q", "-t", CONTAINER_IMAGE, host::root().join("tests/bench/chromium-container").to_str().expect("utf-8 path")])?;
    let flags = THROTTLE_FLAGS.join(" ");
    output_of(
        "docker",
        &["run", "-d", "--rm", "--platform", "linux/arm64", "--name", &name, "-p", "127.0.0.1::9223", "--shm-size=1g", "--cpus=2", "--memory=4g", "--security-opt", "seccomp=unconfined", "-e", &format!("BENCH_CHROMIUM_FLAGS={flags}"), CONTAINER_IMAGE],
    )?;
    let guard = Guard::Container(name.clone());
    let mapping = output_of("docker", &["port", &name, "9223/tcp"])?;
    let port: u16 = mapping.lines().next().and_then(|l| l.rsplit(':').next()).and_then(|p| p.parse().ok()).ok_or_else(|| format!("docker port printed {mapping:?}"))?;
    let target = attach("container-linux-arm64", Endpoint::Tcp { port, host: format!("127.0.0.1:{port}") }, format!("http://host.docker.internal:{server_port}"), guard)?;
    let sandbox = output_of(
        "docker",
        &["exec", &name, "sh", "-c", "pid=$(pgrep -f -- --type=renderer | head -n 1); grep -E '^(NoNewPrivs|Seccomp):' /proc/$pid/status | tr '\\n' ' '"],
    )
    .unwrap_or_else(|e| format!("not readable: {e}"));
    notes.push(format!("container-linux-arm64 renderer sandbox state: {sandbox}"));
    Ok(target)
}

#[derive(Deserialize)]
struct SpeedometerState {
    done: Option<bool>,
    finished: Option<u32>,
    scores: Option<Vec<f64>>,
}

fn speedometer(target: &mut Target, iterations: usize) -> Fallible<Vec<f64>> {
    let url = format!("{}/speedometer/index.html?startAutomatically&iterationCount={iterations}", target.base_url);
    target.page.navigate(&url)?;
    let started = Instant::now();
    let mut progress = (0u32, Instant::now());
    let probe = "typeof benchmarkClient === 'undefined' ? '{}' : JSON.stringify({done: benchmarkClient._hasResults, finished: benchmarkClient._finishedTestCount, scores: benchmarkClient._metrics.Score ? benchmarkClient._metrics.Score.values : null})";
    loop {
        thread::sleep(Duration::from_secs(2));
        let state: SpeedometerState = json::from_str(&target.page.evaluate(probe)?).map_err(|_| "speedometer state is not JSON")?;
        if state.done == Some(true) {
            return match state.scores {
                Some(scores) if scores.len() == iterations => Ok(scores),
                other => Err(format!("speedometer finished without {iterations} scores: {other:?}")),
            };
        }
        let finished = state.finished.unwrap_or(0);
        if finished != progress.0 || started.elapsed() < Duration::from_secs(30) {
            progress = (finished, Instant::now());
        } else if progress.1.elapsed() > STALL_TIMEOUT {
            let frame = target.page.evaluate("(document.querySelector('iframe') || {}).src || ''").unwrap_or_default();
            return Err(format!("speedometer stalled for {} s at {finished} tests in {frame}", STALL_TIMEOUT.as_secs()));
        }
        if started.elapsed() > SPEEDOMETER_TIMEOUT {
            return Err("speedometer did not finish in time".into());
        }
    }
}

#[derive(Deserialize)]
struct Timing {
    dcl: f64,
    load: f64,
    response: f64,
    lcp: f64,
}

fn page_load(target: &mut Target, nonce: usize) -> Fallible<Timing> {
    let marker = format!("n={nonce}");
    target.page.navigate("about:blank")?;
    target.page.navigate(&format!("{}/{NEWS_SITE}?{marker}", target.base_url))?;
    let probe = format!(
        "(() => {{ const n = performance.getEntriesByType('navigation')[0]; if (location.search !== '?{marker}' || !n || !(n.loadEventEnd > 0)) return ''; \
         if (!window.__lcp) {{ window.__lcp = {{}}; new PerformanceObserver(l => {{ const e = l.getEntries(); window.__lcp.t = e[e.length - 1].startTime; }}).observe({{type: 'largest-contentful-paint', buffered: true}}); return ''; }} \
         if (!(window.__lcp.t > 0) || !document.querySelector('#__next article')) return ''; \
         return JSON.stringify({{dcl: n.domContentLoadedEventEnd, load: n.loadEventEnd, response: n.responseEnd, lcp: window.__lcp.t}}); }})()"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = target.page.evaluate(&probe)?;
        if !state.is_empty() {
            return json::from_str(&state).map_err(|_| "navigation timing is not JSON".into());
        }
        if Instant::now() > deadline {
            return Err("the news site never rendered its articles".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn run(ctx: &Context) -> Fallible<Outcome> {
    let mut outcome = Outcome { images: vec![GUEST_IMAGE], ..Outcome::default() };
    let speedometer_dir = host::root().join(SPEEDOMETER_DIR);
    if !speedometer_dir.join("index.html").exists() {
        return Err(format!("no Speedometer checkout at {}; see docs/performance.md for the pinned release/3.1 commit", speedometer_dir.display()));
    }
    let server = serve::start(speedometer_dir)?;
    let home = Home::new("browser")?;
    home.start_daemon()?;

    let mut targets: Vec<Target> = Vec::new();
    let mut open = |result: Fallible<Target>, outcome: &mut Outcome| match result {
        Ok(target) => {
            outcome.browser_versions.push(format!("{}: {}", target.label, target.version));
            targets.push(target);
        }
        Err(e) => outcome.notes.push(format!("variant unavailable: {e}")),
    };
    open(guest_target(&home, GUEST_IMAGE, server.port, &mut outcome.notes), &mut outcome);
    open(container_target(server.port, &mut outcome.notes), &mut outcome);

    let mut scores: Vec<Samples> = targets.iter().map(|_| Samples::new()).collect();
    let mut iteration_scores: Vec<Samples> = targets.iter().map(|_| Samples::new()).collect();
    let mut load: Vec<Samples> = targets.iter().map(|_| Samples::new()).collect();
    let mut dcl: Vec<Samples> = targets.iter().map(|_| Samples::new()).collect();
    let mut response: Vec<Samples> = targets.iter().map(|_| Samples::new()).collect();
    let mut lcp: Vec<Samples> = targets.iter().map(|_| Samples::new()).collect();
    let mut nonce = 0;

    'rounds: for round in 0..ctx.rounds {
        for step in 0..targets.len() {
            if ctx.gate.gave_up() {
                outcome.notes.push(format!("perf gate deferred: host busy; stopped before round {round} of {}", ctx.rounds));
                break 'rounds;
            }
            let at = (round + step) % targets.len();
            let target = &mut targets[at];
            let result = ctx.gate.run(|| speedometer(target, ctx.iterations));
            match result {
                Ok((iterations, _, verdict)) => {
                    let mean = iterations.iter().sum::<f64>() / iterations.len() as f64;
                    eprintln!("browser round {round}: {} speedometer {mean:.2} ({verdict:?})", target.label);
                    scores[at].record(mean, verdict);
                    if verdict == crate::host::Verdict::Clean {
                        iterations.iter().for_each(|v| iteration_scores[at].push(*v));
                    }
                }
                Err(e) => {
                    outcome.notes.push(format!("{} speedometer round {round} failed: {e}", target.label));
                    scores[at].discard(None, format!("failed: {e}"));
                }
            }
            let batch = ctx.gate.run(|| {
                (0..LOADS_PER_BATCH)
                    .map(|_| {
                        nonce += 1;
                        page_load(target, nonce)
                    })
                    .collect::<Fallible<Vec<Timing>>>()
            });
            match batch {
                Ok((timings, _, verdict)) => {
                    for timing in timings {
                        load[at].record(timing.load, verdict);
                        dcl[at].record(timing.dcl, verdict);
                        response[at].record(timing.response, verdict);
                        lcp[at].record(timing.lcp, verdict);
                    }
                }
                Err(e) => {
                    outcome.notes.push(format!("{} page load round {round} failed: {e}", target.label));
                    load[at].discard(None, format!("failed: {e}"));
                }
            }
            target.page.navigate("about:blank")?;
        }
    }

    for (at, target) in targets.iter().enumerate() {
        let label = &target.label;
        let conditions = format!("Speedometer 3.1 (WebKit/Speedometer release/3.1), iterationCount={}, headed, 1280x800 viewport, {} ; run order rotated each round", ctx.iterations, target.version);
        outcome.measures.push(scores[at].finish(&format!("browser.speedometer.{label}.score"), "score", Statistic::Median, &conditions));
        outcome.measures.push(iteration_scores[at].finish(&format!("browser.speedometer.{label}.iteration_score"), "score", Statistic::Median, &format!("all iteration scores of every clean run, pooled; {conditions}")));
        let page = "Speedometer 3.1's news-next site (a Next.js static build, about 1 MiB of JS, CSS and images, rendered on the client) from the host over HTTP/1.1 keep-alive, cache disabled, from a fresh about:blank; a load counts once the articles are in the DOM";
        outcome.measures.push(lcp[at].finish(&format!("browser.page_load.{label}.largest_contentful_paint"), "ms", Statistic::Median, page));
        outcome.measures.push(load[at].finish(&format!("browser.page_load.{label}.load_event_end"), "ms", Statistic::Median, page));
        outcome.measures.push(dcl[at].finish(&format!("browser.page_load.{label}.dom_content_loaded_end"), "ms", Statistic::Median, page));
        outcome.measures.push(response[at].finish(&format!("browser.page_load.{label}.response_end"), "ms", Statistic::Median, page));
    }
    outcome.measures.push(fact("browser.speedometer.iterations_per_run", "iterations", ctx.iterations as f64, "iterationCount passed to every variant"));
    Ok(outcome)
}
