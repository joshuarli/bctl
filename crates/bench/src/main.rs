mod browser;
mod cdp;
mod gates;
mod host;
mod interaction;
mod lifecycle;
mod net;
mod report;
mod resources;
mod rfb;
mod serve;
mod session;
mod stats;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use host::{Fallible, QuietGate};
use report::{ImageIdentity, Measure, Report};
use session::Home;

const USAGE: &str = "usage:
  bctl-bench lifecycle|browser|interaction|resources [--rounds N] [--iterations N] [--quiet-wait SECS] [--out DIR]
  bctl-bench gate [RESULT.json...]
  bctl-bench serve                      serve the pinned Speedometer and the page-load fixture on a free port, for manual runs

Suites write DIR/<UTC time>-<suite>.json (default tests/bench/results) and print its path.
--iterations is the Speedometer iterationCount of the browser suite.
Time-based samples run only while perf-gate-ready exits 0; --quiet-wait bounds the total time a suite waits for a quiet host.";

pub struct Context {
    pub gate: QuietGate,
    pub rounds: usize,
    pub iterations: usize,
}

#[derive(Default)]
pub struct Outcome {
    pub measures: Vec<Measure>,
    pub notes: Vec<String>,
    pub browser_versions: Vec<String>,
    pub images: Vec<&'static str>,
}

pub fn cdp_version_of(home: &Home, session: &str) -> Fallible<String> {
    let url = home.cdp_url(session)?;
    let host = url.strip_prefix("http://").ok_or("cdp url is not http")?;
    let port: u16 = host.rsplit(':').next().and_then(|p| p.parse().ok()).ok_or("cdp url has no port")?;
    cdp::Endpoint::Tcp { port, host: host.to_string() }.browser_version()
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("bctl-bench: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Fallible<()> {
    let mut args = std::env::args().skip(1);
    let suite = args.next().ok_or(USAGE)?;
    let rest: Vec<String> = args.collect();
    if suite == "serve" {
        let server = serve::start(Some(host::root().join(browser::SPEEDOMETER_DIR)))?;
        println!("http://127.0.0.1:{}/speedometer/index.html", server.port);
        loop {
            std::thread::park();
        }
    }
    if suite == "gate" {
        return gates::check(&rest);
    }
    let mut rounds = 7;
    let mut iterations = 5;
    let mut quiet_wait = 900;
    let mut out = host::root().join("tests/bench/results");
    let mut options = rest.iter();
    while let Some(flag) = options.next() {
        let mut value = || options.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--rounds" => rounds = value()?.parse().map_err(|_| "--rounds needs a number")?,
            "--iterations" => iterations = value()?.parse().map_err(|_| "--iterations needs a number")?,
            "--quiet-wait" => quiet_wait = value()?.parse().map_err(|_| "--quiet-wait needs seconds")?,
            "--out" => out = PathBuf::from(value()?),
            _ => return Err(format!("unknown option {flag}\n{USAGE}")),
        }
    }
    let ctx = Context { gate: QuietGate::new(Duration::from_secs(quiet_wait)), rounds, iterations };
    let mut outcome = match suite.as_str() {
        "lifecycle" => lifecycle::run(&ctx)?,
        "browser" => browser::run(&ctx)?,
        "interaction" => interaction::run(&ctx)?,
        "resources" => resources::run(&ctx)?,
        _ => return Err(format!("unknown suite {suite}\n{USAGE}")),
    };
    outcome.notes.push(format!("waited {:.0} s in total for a quiet host", ctx.gate.waited().as_secs_f64()));
    let started = host::utc_now()?;
    let (commit, dirty) = host::commit()?;
    let images: Fallible<Vec<ImageIdentity>> = outcome.images.iter().map(|name| host::image_identity(name)).collect();
    let report = Report {
        suite: suite.clone(),
        started: started.clone(),
        host: host::host()?,
        commit,
        dirty,
        smolvm: host::smolvm_version(&host::root().join(".work/runtime/smolvm/smolvm"))?,
        images: images?,
        browser_versions: outcome.browser_versions,
        measures: outcome.measures,
        notes: outcome.notes,
    };
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let path = out.join(format!("{}-{suite}.json", started.replace([':', '-'], "")));
    std::fs::write(&path, report.to_json()).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("{}", path.display());
    Ok(())
}
