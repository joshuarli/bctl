mod daemon;
mod gateway;
mod page;
mod paths;
mod protocol;
mod runs;
mod tabs;

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lexopt::prelude::*;
use miniserde::json;

use paths::Paths;
use protocol::{
    BrowserRequest, ErrorBody, ErrorCode, ExecRequest, GrabRequest, JobRequest, Op, Request, ResourcesView, Response, RunView, SessionView, SnapshotRequest,
    TabView,
};

const USAGE: &str = "usage:
  bctl up NAME [--profile PROFILE] [--image IMAGE] [--allow-private CIDR]... [--json]
                                                              start or reuse a browser session
  bctl down NAME [--json]                                     stop it, keeping its profile
  bctl rm NAME [--json]                                       forget a stopped session
  bctl status [--json]                               list sessions and why they are not ready
  bctl inspect NAME [--json]                         one session in detail
  bctl serve [--open] [--json]                        print (or open) a one-time dashboard login link
  bctl cdp-url NAME [--json]                         mint a revocable CDP control URL for a ready session
  bctl cdp-revoke NAME [--json]                      revoke every CDP control URL of a session
  bctl profile list [--json]                         profiles and the session holding each
  bctl profile login PROFILE [--image IMAGE]          start a session on PROFILE and open it to sign in by hand
  bctl grab URL... --session S [--json]              crawl URLs with Grab in session S
  bctl run JOB --session S [--config FILE] [--json]  run a bctl.toml job (default ./bctl.toml) in session S
  bctl runs [RUN] [--session S] [--json]             list runs, or one run with every resolved field
  bctl logs RUN                                      print a run's log
  bctl pause RUN | resume RUN | cancel RUN [--json]  stop a grab at a page boundary, continue, or end a run
  bctl tab list SESSION [--json]                     list page targets; * marks the one native commands act on
  bctl tab new SESSION [URL] [--json]                open a page target (default about:blank) and select it
  bctl tab use SESSION TARGET_ID [--json]            select a page target for native commands
  bctl tab close SESSION TARGET_ID [--json]          close a page target
  bctl page snapshot SESSION [--full] [--depth N] [--scope CSS] [--json]
                                                     the selected target's accessibility tree with element refs
  bctl page resolve SESSION REF [--json]             the live role and name of the node an element ref names
  bctl doctor [--json]                               check the runtime and images
  bctl daemon                                        run the daemon in the foreground";

enum Cli {
    Request { op: Op, request: Request, json: bool, open: bool },
    Doctor { json: bool },
    ProfileLogin { profile: String, image: Option<String> },
    Daemon,
}

fn parse() -> Result<Cli, lexopt::Error> {
    let mut parser = lexopt::Parser::from_env();
    let command = match parser.next()? {
        Some(Value(v)) => v.string()?,
        Some(arg) => return Err(arg.unexpected()),
        None => return Err("a command is required".into()),
    };
    let (op, needs_session) = match command.as_str() {
        "up" => (Op::Up, true),
        "down" => (Op::Down, true),
        "rm" => (Op::Rm, true),
        "status" => (Op::Status, false),
        "serve" => (Op::Serve, false),
        "inspect" => (Op::Inspect, true),
        "cdp-url" => (Op::CdpUrl, true),
        "cdp-revoke" => (Op::CdpRevoke, true),
        "profile" => match parser.next()? {
            Some(Value(v)) if v == "list" => (Op::Profiles, false),
            Some(Value(v)) if v == "login" => {
                let profile = match parser.next()? {
                    Some(Value(v)) => v.string()?,
                    _ => return Err("usage: bctl profile login PROFILE [--image IMAGE]".into()),
                };
                let mut image = None;
                while let Some(arg) = parser.next()? {
                    match arg {
                        Long("image") => image = Some(parser.value()?.string()?),
                        _ => return Err(arg.unexpected()),
                    }
                }
                return Ok(Cli::ProfileLogin { profile, image });
            }
            _ => return Err("usage: bctl profile list | bctl profile login PROFILE [--image IMAGE]".into()),
        },
        "grab" | "run" | "runs" | "logs" | "pause" | "resume" | "cancel" => return parse_run_command(&command, parser),
        "tab" => return parse_tab_command(parser),
        "page" => return parse_page_command(parser),
        "doctor" | "daemon" => (Op::Ping, false),
        other => return Err(format!("unknown command {other:?}").into()),
    };
    let mut request = Request { op: op.as_str().to_string(), ..Request::empty() };
    let mut json = false;
    let mut open = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Long("json") if command != "daemon" => json = true,
            Long("open") if op == Op::Serve => open = true,
            Long("profile") if op == Op::Up => request.profile = Some(parser.value()?.string()?),
            Long("image") if op == Op::Up => request.image = Some(parser.value()?.string()?),
            Long("allow-private") if op == Op::Up => request.allow_private.push(parser.value()?.string()?),
            Value(v) if needs_session && request.session.is_none() => request.session = Some(v.string()?),
            _ => return Err(arg.unexpected()),
        }
    }
    if needs_session && request.session.is_none() {
        return Err(format!("bctl {command} needs a session name").into());
    }
    Ok(match command.as_str() {
        "doctor" => Cli::Doctor { json },
        "daemon" => Cli::Daemon,
        _ => Cli::Request { op, request, json, open },
    })
}

fn parse_run_command(command: &str, mut parser: lexopt::Parser) -> Result<Cli, lexopt::Error> {
    let op = match command {
        "grab" | "run" => Op::Run,
        "runs" => Op::Runs,
        "logs" => Op::Logs,
        "pause" => Op::Pause,
        "resume" => Op::Resume,
        _ => Op::Cancel,
    };
    let mut request = Request { op: op.as_str().to_string(), ..Request::empty() };
    let mut json = false;
    let mut positional = Vec::new();
    let mut config = None;
    while let Some(arg) = parser.next()? {
        match arg {
            Long("json") if command != "logs" => json = true,
            Long("session") if matches!(op, Op::Run | Op::Runs) => request.session = Some(parser.value()?.string()?),
            Long("config") if command == "run" => config = Some(std::path::PathBuf::from(parser.value()?)),
            Value(v) => positional.push(v.string()?),
            _ => return Err(arg.unexpected()),
        }
    }
    match command {
        "grab" => {
            if positional.is_empty() {
                return Err("bctl grab needs at least one URL".into());
            }
            let grab = GrabRequest { urls: positional, max_pages: None, jobs: None, robots: true, max_runtime_secs: None };
            request.job = Some(Box::new(JobRequest { name: None, browser: None, exec: None, grab: Some(grab) }));
        }
        "run" => {
            let [job] = positional.as_slice() else { return Err("bctl run needs exactly one job name".into()) };
            request.job = Some(Box::new(resolve_job(config.as_deref(), job)?));
        }
        "runs" => match positional.as_slice() {
            [] => {}
            [run] => request.run = Some(run.parse().map_err(|_| format!("{run:?} is not a run id"))?),
            _ => return Err("bctl runs takes at most one run id".into()),
        },
        _ => {
            let [run] = positional.as_slice() else { return Err(format!("bctl {command} needs one run id").into()) };
            request.run = Some(run.parse().map_err(|_| format!("{run:?} is not a run id"))?);
        }
    }
    if op == Op::Run && request.session.is_none() {
        return Err(format!("bctl {command} needs --session SESSION").into());
    }
    Ok(Cli::Request { op, request, json, open: false })
}

const TAB_USAGE: &str = "usage: bctl tab list SESSION | new SESSION [URL] | use SESSION TARGET_ID | close SESSION TARGET_ID [--json]";

fn parse_tab_command(mut parser: lexopt::Parser) -> Result<Cli, lexopt::Error> {
    let op = match parser.next()? {
        Some(Value(v)) if v == "list" => Op::TabList,
        Some(Value(v)) if v == "new" => Op::TabNew,
        Some(Value(v)) if v == "use" => Op::TabUse,
        Some(Value(v)) if v == "close" => Op::TabClose,
        _ => return Err(TAB_USAGE.into()),
    };
    let mut json = false;
    let mut positional = Vec::new();
    while let Some(arg) = parser.next()? {
        match arg {
            Long("json") => json = true,
            Value(v) => positional.push(v.string()?),
            _ => return Err(arg.unexpected()),
        }
    }
    let mut request = Request { op: op.as_str().to_string(), ..Request::empty() };
    let mut positional = positional.into_iter();
    request.session = Some(positional.next().ok_or(TAB_USAGE)?);
    match op {
        Op::TabNew => request.url = positional.next(),
        Op::TabUse | Op::TabClose => request.target = Some(positional.next().ok_or(TAB_USAGE)?),
        _ => {}
    }
    if positional.next().is_some() {
        return Err(TAB_USAGE.into());
    }
    Ok(Cli::Request { op, request, json, open: false })
}

const PAGE_USAGE: &str = "usage: bctl page snapshot SESSION [--full] [--depth N] [--scope CSS] | resolve SESSION REF [--json]";

fn parse_page_command(mut parser: lexopt::Parser) -> Result<Cli, lexopt::Error> {
    let op = match parser.next()? {
        Some(Value(v)) if v == "snapshot" => Op::PageSnapshot,
        Some(Value(v)) if v == "resolve" => Op::PageResolve,
        _ => return Err(PAGE_USAGE.into()),
    };
    let mut json = false;
    let mut snapshot = SnapshotRequest::default();
    let mut positional = Vec::new();
    while let Some(arg) = parser.next()? {
        match arg {
            Long("json") => json = true,
            Long("full") if op == Op::PageSnapshot => snapshot.full = true,
            Long("depth") if op == Op::PageSnapshot => {
                let depth = parser.value()?.string()?;
                snapshot.depth = Some(depth.parse().map_err(|_| format!("--depth {depth:?} is not a level count"))?);
            }
            Long("scope") if op == Op::PageSnapshot => snapshot.scope = Some(parser.value()?.string()?),
            Value(v) => positional.push(v.string()?),
            _ => return Err(arg.unexpected()),
        }
    }
    snapshot.nodes = json;
    let mut request = Request { op: op.as_str().to_string(), ..Request::empty() };
    let mut positional = positional.into_iter();
    request.session = Some(positional.next().ok_or(PAGE_USAGE)?);
    match op {
        Op::PageSnapshot => request.snapshot = Some(Box::new(snapshot)),
        _ => request.element = Some(positional.next().ok_or(PAGE_USAGE)?),
    }
    if positional.next().is_some() {
        return Err(PAGE_USAGE.into());
    }
    Ok(Cli::Request { op, request, json, open: false })
}

/// Resolves `job` from `bctl.toml` here, where relative paths mean something, and sends the daemon only the result.
fn resolve_job(config: Option<&std::path::Path>, job: &str) -> Result<JobRequest, lexopt::Error> {
    let path = config.unwrap_or(std::path::Path::new("bctl.toml"));
    let path = path.canonicalize().map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let resolved = bctl_jobs::resolve(&bctl_jobs::load(&path).map_err(|e| e.to_string())?, job).map_err(|e| e.to_string())?;
    let base = path.parent().ok_or_else(|| format!("{} has no directory", path.display()))?;
    let resources = resolved.resources();
    let browser = BrowserRequest {
        profile: resolved.profile.to_string(),
        image: resolved.image.clone(),
        resources: ResourcesView { width: resources.viewport[0], height: resources.viewport[1], cpus: resources.cpus, memory_mib: resources.memory_mib },
    };
    let (exec, grab) = match resolved.kind {
        bctl_jobs::ResolvedKind::Exec(exec) => {
            let cwd = exec.cwd.map_or_else(|| base.to_path_buf(), |cwd| base.join(cwd));
            let exec = ExecRequest { argv: exec.argv, cwd: cwd.display().to_string(), timeout_secs: exec.timeout.map(|t| t.as_secs()) };
            (Some(exec), None)
        }
        bctl_jobs::ResolvedKind::Grab(grab) => {
            let grab = GrabRequest {
                urls: grab.urls,
                max_pages: grab.max_pages,
                jobs: grab.jobs,
                robots: grab.robots,
                max_runtime_secs: grab.max_runtime.map(|t| t.as_secs()),
            };
            (None, Some(grab))
        }
    };
    Ok(JobRequest { name: Some(resolved.name.to_string()), browser: Some(browser), exec, grab })
}

fn main() -> ExitCode {
    let cli = match parse() {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("bctl: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let paths = match Paths::from_env() {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("bctl: {e}");
            return ExitCode::FAILURE;
        }
    };
    match cli {
        Cli::Daemon => match daemon::run(paths) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("bctl daemon: {e}");
                ExitCode::FAILURE
            }
        },
        Cli::Doctor { json } => doctor(&paths, json),
        Cli::ProfileLogin { profile, image } => profile_login(&paths, profile, image),
        Cli::Request { op, request, json, open } => match call(&paths, &request) {
            Ok(response) => {
                let code = render(op, &response, json, request.run.is_some());
                if open && let Some(url) = &response.url && Command::new("open").arg(url).status().map_or(true, |s| !s.success()) {
                    eprintln!("bctl: could not open {url}");
                    return ExitCode::FAILURE;
                }
                code
            }
            Err(error) => report(&error, json),
        },
    }
}

fn send(paths: &Paths, request: &Request) -> std::io::Result<Response> {
    let mut stream = UnixStream::connect(paths.socket())?;
    writeln!(stream, "{}", json::to_string(request))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    json::from_str(&line).map_err(|_| std::io::Error::new(ErrorKind::InvalidData, format!("daemon sent {line:?}")))
}

/// Sends `request`, starting the daemon first if nothing is listening. Closing the CLI never stops the daemon.
fn call(paths: &Paths, request: &Request) -> Result<Response, ErrorBody> {
    let unreachable = |e: std::io::Error| {
        ErrorBody::new(ErrorCode::DaemonUnreachable, e.to_string(), Some(format!("see {}", paths.log().display())))
    };
    match send(paths, request) {
        Ok(response) => return Ok(response),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {}
        Err(e) => return Err(unreachable(e)),
    }
    let log = daemon::open_log(paths).map_err(unreachable)?;
    let exe = std::env::current_exe().map_err(unreachable)?;
    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(unreachable)?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .map_err(unreachable)?;
    let ping = Request { op: Op::Ping.as_str().to_string(), ..Request::empty() };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match send(paths, &ping) {
            Ok(_) => return send(paths, request).map_err(unreachable),
            Err(e) if Instant::now() > deadline => return Err(unreachable(e)),
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn report(error: &ErrorBody, json: bool) -> ExitCode {
    if json {
        println!("{}", json::to_string(&Response { error: Some(Box::new(error.clone())), ..Response::empty() }));
    } else {
        eprintln!("bctl: {} [{}]", error.message, error.code);
        if let Some(remedy) = &error.remedy {
            eprintln!("  {remedy}");
        }
        if let Some(targets) = &error.targets {
            targets.iter().for_each(|t| eprintln!("  {}", tab_line(t, None)));
        }
    }
    ExitCode::FAILURE
}

fn render(op: Op, response: &Response, json: bool, detail: bool) -> ExitCode {
    if let Some(error) = &response.error {
        return report(error, json);
    }
    if json {
        println!("{}", json::to_string(response));
        return ExitCode::SUCCESS;
    }
    match op {
        Op::Status => {
            println!("{:<20} {:<16} {:<10} {:<12} IMAGE", "NAME", "PROFILE", "STATE", "WAITING ON");
            for s in &response.sessions {
                let waiting = s.stage.clone().unwrap_or_else(|| "-".into());
                println!("{:<20} {:<16} {:<10} {:<12} {}", s.name, s.profile, s.state, waiting, s.image);
            }
        }
        Op::Profiles => {
            for p in &response.profiles {
                println!("{:<20} {}", p.name, p.holder.as_deref().unwrap_or("-"));
            }
        }
        Op::Up | Op::Down | Op::Inspect => response.sessions.iter().for_each(print_session),
        Op::Serve | Op::CdpUrl => {
            if let Some(url) = &response.url {
                println!("{url}");
            }
        }
        Op::Rm | Op::Ping | Op::CdpRevoke => {}
        Op::Run | Op::Pause | Op::Resume | Op::Cancel => response.runs.iter().for_each(print_run),
        Op::Runs if detail => response.runs.iter().for_each(print_run),
        Op::Runs => {
            println!("{:<6} {:<20} {:<20} {:<5} {:<18} {:<20} PROGRESS", "RUN", "SESSION", "JOB", "KIND", "STATE", "OUTCOME");
            for r in &response.runs {
                println!(
                    "{:<6} {:<20} {:<20} {:<5} {:<18} {:<20} {}",
                    r.id,
                    r.session,
                    r.job.as_deref().unwrap_or("-"),
                    r.kind,
                    r.state,
                    r.outcome.as_deref().unwrap_or("-"),
                    progress(r)
                );
            }
        }
        Op::Logs => print!("{}", response.log.as_ref().map_or("", |log| log.text.as_str())),
        Op::TabList | Op::TabNew | Op::TabUse | Op::TabClose => {
            if let Some(tabs) = &response.tabs {
                tabs.targets.iter().for_each(|t| println!("{}", tab_line(t, tabs.selected.as_deref())));
            }
        }
        Op::PageSnapshot => print!("{}", response.page.as_ref().and_then(|p| p.snapshot.as_ref()).and_then(|s| s.text.as_deref()).unwrap_or_default()),
        Op::PageResolve => {
            if let Some(e) = response.page.as_ref().and_then(|p| p.element.as_ref()) {
                println!("{} {} {}  frame {}", e.element, e.role, json::to_string(&e.name), e.frame);
            }
        }
    }
    let failed = matches!(op, Op::Run | Op::Resume) && response.runs.iter().any(|r| r.state == "failed");
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn tab_line(tab: &TabView, selected: Option<&str>) -> String {
    format!("{} {}  {}  {}", if selected == Some(tab.id.as_str()) { "*" } else { " " }, tab.id, tab.url, tab.title)
}

fn progress(run: &RunView) -> String {
    run.progress.map_or_else(|| "-".into(), |p| format!("{} pages, {} queued, {} assets", p.pages_done, p.pages_queued, p.assets_done))
}

fn print_run(r: &RunView) {
    let field = |name: &str, value: Option<String>| {
        if let Some(value) = value {
            println!("{name:<15}{value}");
        }
    };
    field("run", Some(r.id.to_string()));
    field("state", Some(r.state.clone()));
    field("outcome", r.outcome.clone());
    field("detail", r.detail.clone());
    field("waiting for", r.interaction.as_ref().map(|i| format!("{} at {} (`bctl resume {}` once it is done)", i.reason, i.url, r.id)));
    field("progress", r.progress.map(|_| progress(r)));
    field("session", Some(r.session.clone()));
    field("job", Some(r.job.clone().unwrap_or_else(|| "(one-off grab)".into())));
    field("kind", Some(r.kind.clone()));
    field("profile", Some(r.profile.clone()));
    field("image", Some(format!("{} ({})", r.image, r.image_digest)));
    field("browser", Some(r.browser_version.clone()));
    field("input digest", Some(r.input_digest.clone()));
    field("config", Some(json::to_string(&r.config)));
    field("output", Some(r.output_dir.clone()));
    field("log", Some(r.log.clone()));
    for a in &r.attempts {
        let outcome = a.outcome.as_deref().map(|o| format!(" {o}")).unwrap_or_default();
        let detail = a.detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default();
        field(&format!("attempt {}", a.number), Some(format!("{}{outcome}{detail}", a.state)));
    }
}

fn print_session(s: &SessionView) {
    let field = |name: &str, value: Option<String>| {
        if let Some(value) = value {
            println!("{name:<11}{value}");
        }
    };
    field("session", Some(s.name.clone()));
    field("state", Some(s.state.clone()));
    field("waiting on", s.stage.clone());
    field("failure", s.failure.clone());
    field("profile", Some(s.profile.clone()));
    field("image", Some(s.image.clone()));
    field("resources", Some(format!("{}x{}, {} CPUs, {} MiB", s.resources.width, s.resources.height, s.resources.cpus, s.resources.memory_mib)));
    field("private", (!s.allow_private.is_empty()).then(|| s.allow_private.join(", ")));
    field("generation", Some(s.generation.to_string()));
    field("machine", s.machine.clone());
    field("vnc", s.vnc_port.map(|p| format!("127.0.0.1:{p}")));
    field("input", s.input.clone());
}

fn doctor(paths: &Paths, json_output: bool) -> ExitCode {
    let runtime = paths.runtime();
    let version = Command::new(&runtime).arg("--version").output().ok().filter(|o| o.status.success());
    let images: Vec<String> = std::fs::read_dir(paths.images())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".tar").map(str::to_string))
        .collect();
    let daemon = send(paths, &Request { op: Op::Ping.as_str().to_string(), ..Request::empty() }).is_ok();
    let checks = [
        ("home", true, paths.home.display().to_string()),
        (
            "runtime",
            version.is_some(),
            match &version {
                Some(o) => format!("{} ({})", String::from_utf8_lossy(&o.stdout).trim(), runtime.display()),
                None => format!("missing at {}: build with runtime/build.sh and copy .work/runtime/smolvm there", runtime.display()),
            },
        ),
        (
            "images",
            !images.is_empty(),
            if images.is_empty() { format!("none in {}", paths.images().display()) } else { images.join(", ") },
        ),
        ("daemon", true, if daemon { "running".into() } else { "not running (starts on demand)".to_string() }),
    ];
    if json_output {
        let body: Vec<String> = checks
            .iter()
            .map(|(name, ok, detail)| format!("{{\"name\":{},\"ok\":{ok},\"detail\":{}}}", json::to_string(name), json::to_string(detail)))
            .collect();
        println!("{{\"checks\":[{}]}}", body.join(","));
    } else {
        for (name, ok, detail) in &checks {
            println!("{} {name:<8} {detail}", if *ok { "ok  " } else { "FAIL" });
        }
    }
    if checks.iter().all(|(_, ok, _)| *ok) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Brings up a session named after the profile and opens the dashboard on it, so the user can sign in to sites by hand.
fn profile_login(paths: &Paths, profile: String, image: Option<String>) -> ExitCode {
    let up = Request { op: Op::Up.as_str().to_string(), session: Some(profile.clone()), profile: Some(profile.clone()), image, ..Request::empty() };
    match call(paths, &up) {
        Ok(response) if response.error.is_none() => {}
        Ok(response) => return render(Op::Up, &response, false, false),
        Err(error) => return report(&error, false),
    }
    let serve = Request { op: Op::Serve.as_str().to_string(), ..Request::empty() };
    let url = match call(paths, &serve) {
        Ok(Response { url: Some(url), .. }) => format!("{url}&next=/sessions/{profile}"),
        Ok(response) => return render(Op::Serve, &response, false, false),
        Err(error) => return report(&error, false),
    };
    println!("{url}");
    match Command::new("open").arg(&url).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        _ => {
            eprintln!("bctl: could not open the dashboard; visit the link above");
            ExitCode::FAILURE
        }
    }
}
