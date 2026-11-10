use std::thread;
use std::time::{Duration, Instant};

use miniserde::{Deserialize, json};

use crate::cdp::{Cdp, Endpoint};
use crate::host::{self, Fallible, Runtime};
use crate::report::{Samples, fact};
use crate::serve;
use crate::session::{Home, session_name};
use crate::stats::Statistic;
use crate::{Context, Outcome};

const STALL_TIMEOUT: Duration = Duration::from_secs(120);
const SPEEDOMETER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const LOADS_PER_BATCH: usize = 5;
pub const SPEEDOMETER_DIR: &str = ".work/dl/speedometer/src";
pub const HOST_RANGE: &str = "100.96.0.0/30";
pub const HOST_FROM_GUEST: &str = "100.96.0.1";
const NEWS_SITE: &str = "speedometer/resources/newssite/news-next/dist/index.html";

struct Target {
    base_url: String,
    version: String,
    page: Cdp,
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

fn guest_target(home: &Home, runtime: Runtime, server_port: u16, notes: &mut Vec<String>) -> Fallible<Target> {
    let image = runtime.image();
    let name = session_name(&format!("br-{image}"));
    home.up(&name, &name, image, &[HOST_RANGE.to_string()])?;
    let url = home.cdp_url(&name)?;
    let host = url.strip_prefix("http://").ok_or("cdp url is not http")?.to_string();
    let port: u16 = host.rsplit(':').next().and_then(|p| p.parse().ok()).ok_or("cdp url has no port")?;
    notes.push(format!("{image}: guest reaches the benchmark server at {HOST_FROM_GUEST}:{server_port}, the {} runtime's gateway to the host loopback, through --allow-private {HOST_RANGE}", runtime.name()));
    let endpoint = Endpoint::Tcp { port, host };
    let version = wait_for_cdp(&endpoint)?;
    let (_, mut page) = endpoint.attach_first_page()?;
    page.call("Page.enable", "{}")?;
    page.call("Page.bringToFront", "{}")?;
    page.call("Network.enable", "{}")?;
    page.call("Network.setCacheDisabled", r#"{"cacheDisabled":true}"#)?;
    Ok(Target { base_url: format!("http://{HOST_FROM_GUEST}:{server_port}"), version, page })
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
    let runtime = Runtime::current()?;
    let image = runtime.image();
    let mut outcome = Outcome { images: vec![image], ..Outcome::default() };
    let speedometer_dir = host::root().join(SPEEDOMETER_DIR);
    if !speedometer_dir.join("index.html").exists() {
        return Err(format!("no Speedometer checkout at {}; see docs/performance.md for the pinned release/3.1 commit", speedometer_dir.display()));
    }
    let server = serve::start(speedometer_dir)?;
    let home = Home::new("browser")?;
    home.start_daemon()?;
    let mut target = guest_target(&home, runtime, server.port, &mut outcome.notes)?;
    outcome.browser_versions.push(target.version.clone());

    let mut scores = Samples::new();
    let mut iteration_scores = Samples::new();
    let mut load = Samples::new();
    let mut dcl = Samples::new();
    let mut response = Samples::new();
    let mut lcp = Samples::new();
    let mut nonce = 0;

    for round in 0..ctx.rounds {
        if ctx.gate.gave_up() {
            outcome.notes.push(format!("perf gate deferred: host busy; stopped before round {round} of {}", ctx.rounds));
            break;
        }
        match ctx.gate.run(|| speedometer(&mut target, ctx.iterations)) {
            Ok((iterations, _, verdict)) => {
                let mean = iterations.iter().sum::<f64>() / iterations.len() as f64;
                eprintln!("browser round {round}: speedometer {mean:.2} ({verdict:?})");
                scores.record(mean, verdict);
                if verdict == crate::host::Verdict::Clean {
                    iterations.iter().for_each(|v| iteration_scores.push(*v));
                }
            }
            Err(e) => {
                outcome.notes.push(format!("speedometer round {round} failed: {e}"));
                scores.discard(None, format!("failed: {e}"));
            }
        }
        let batch = ctx.gate.run(|| {
            (0..LOADS_PER_BATCH)
                .map(|_| {
                    nonce += 1;
                    page_load(&mut target, nonce)
                })
                .collect::<Fallible<Vec<Timing>>>()
        });
        match batch {
            Ok((timings, _, verdict)) => {
                for timing in timings {
                    load.record(timing.load, verdict);
                    dcl.record(timing.dcl, verdict);
                    response.record(timing.response, verdict);
                    lcp.record(timing.lcp, verdict);
                }
            }
            Err(e) => {
                outcome.notes.push(format!("page load round {round} failed: {e}"));
                load.discard(None, format!("failed: {e}"));
            }
        }
        target.page.navigate("about:blank")?;
    }

    let mode = if runtime == Runtime::Nanos { "headless (--headless=new)" } else { "headed" };
    let conditions = format!("Speedometer 3.1 (WebKit/Speedometer release/3.1), iterationCount={}, {mode}, 1280x800 viewport, {}", ctx.iterations, target.version);
    outcome.measures.push(scores.finish("browser.speedometer.score", "score", Statistic::Median, &conditions));
    outcome.measures.push(iteration_scores.finish("browser.speedometer.iteration_score", "score", Statistic::Median, &format!("all iteration scores of every clean run, pooled; {conditions}")));
    let page = "Speedometer 3.1's news-next site (a Next.js static build, about 1 MiB of JS, CSS and images, rendered on the client) from the host over HTTP/1.1 keep-alive, cache disabled, from a fresh about:blank; a load counts once the articles are in the DOM";
    outcome.measures.push(lcp.finish("browser.page_load.largest_contentful_paint", "ms", Statistic::Median, page));
    outcome.measures.push(load.finish("browser.page_load.load_event_end", "ms", Statistic::Median, page));
    outcome.measures.push(dcl.finish("browser.page_load.dom_content_loaded_end", "ms", Statistic::Median, page));
    outcome.measures.push(response.finish("browser.page_load.response_end", "ms", Statistic::Median, page));
    outcome.measures.push(fact("browser.speedometer.iterations_per_run", "iterations", ctx.iterations as f64, "iterationCount passed to every run"));
    Ok(outcome)
}
