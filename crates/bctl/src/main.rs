mod agent_browser;
mod authority;
mod daemon;
mod gateway;
mod image;
mod page;
mod paths;
mod protocol;
mod runs;
mod runtime;
mod tabs;
mod webmcp;

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lexopt::prelude::*;
use miniserde::json;

use paths::Paths;
use runtime::{Runtime, RuntimeKind};
use protocol::{
    ActionRequest, ActionView, BrowserRequest, ErrorBody, ErrorCode, ExecRequest, GrabRequest, JobRequest, Op, Request, ResourcesView, Response, RunView,
    SessionView, SnapshotRequest, TabView,
};

const USAGE: &str = "usage:
  bctl up NAME [--profile PROFILE] [--image IMAGE] [--allow-private CIDR]... [--json]
                                                              start or reuse a browser session (image default: helium)
  bctl down NAME [--json]                                     stop it, keeping its profile
  bctl rm NAME [--json]                                       forget a stopped session
  bctl status [--json]                               list sessions and why they are not ready
  bctl inspect NAME [--json]                         one session in detail
  bctl serve [--open] [--json]                        print (or open) a one-time dashboard login link
  bctl cdp-url NAME [--json]                         mint a revocable CDP control URL for a ready session
  bctl cdp-revoke NAME [--url URL] [--json]          revoke one CDP control URL of a session, or all of them
  bctl profile list [--json]                         profiles and the session holding each
  bctl profile login PROFILE [--image IMAGE]          start a session on PROFILE and open it to sign in by hand
  bctl grab URL... --session S [--json]              crawl URLs with Grab in session S
  bctl run JOB --session S [--config FILE] [--json]  run a bctl.toml job (default ./bctl.toml) in session S
  bctl runs [RUN] [--session S] [--json]             list runs, or one run with every resolved field
  bctl logs RUN                                      print a run's log
  bctl pause (SESSION | RUN) [--json]                checkpoint a ready session's VM, or pause a grab at a page boundary
  bctl resume (SESSION | RUN) [--json]               restore a paused session, or continue a grab
  bctl cancel RUN [--json]                           end a run
  bctl tab list SESSION [--json]                     list page targets; * marks the one native commands act on
  bctl tab new SESSION [URL] [--json]                open a page target (default about:blank) and select it
  bctl tab use SESSION TARGET_ID [--json]            select a page target for native commands
  bctl tab close SESSION TARGET_ID [--json]          close a page target
  bctl page snapshot SESSION [--full] [--depth N] [--scope CSS] [--json]
                                                     the selected target's accessibility tree with element refs
  bctl page resolve SESSION REF [--json]             the live role and name of the node an element ref names
  bctl page goto SESSION URL [--wait commit|domcontentloaded|load|networkidle] [--timeout MS]
  bctl page click|dblclick|hover|check|uncheck SESSION ELEMENT
  bctl page fill SESSION ELEMENT TEXT                replace a field's content
  bctl page type SESSION [ELEMENT] TEXT              type key by key at the end, or into what has focus
  bctl page press SESSION [ELEMENT] KEY              press Enter, Tab, Control+a, ...
  bctl page scroll SESSION (ELEMENT | --dx N --dy N) scroll an element into view, or turn the wheel
  bctl page select SESSION ELEMENT VALUE...          choose <select> options by value or label
  bctl page wait SESSION (QUERY [--gone] | --text T | --url PATTERN | --load domcontentloaded|load) [--timeout MS]
  bctl page get SESSION text [ELEMENT] | title | url | attribute NAME ELEMENT | value ELEMENT
  bctl page screenshot SESSION PATH                  write the viewport as a PNG
  bctl page eval SESSION EXPRESSION                  run JavaScript in the page and print its JSON value
  bctl webmcp list SESSION                           list same-origin tools from the selected page (JSON)
  bctl webmcp call SESSION TOOL [JSON_OBJECT] [--allow-write] [--allow-consequential]
                                                      execute a registered tool with explicit opt-ins
  bctl page dialog SESSION accept [--text T] | dismiss
                                                     answer the dialog the page shows
  bctl page batch SESSION FILE|-                     run a JSON list of page actions in order, stopping at the first failure
      ELEMENT is a ref (@e12) or a QUERY: --css SELECTOR | --role ROLE [--name NAME] | --label TEXT |
      --placeholder TEXT | --testid ID, plus --nth N to pick a match; a query matching several fails AMBIGUOUS.
      An action waits up to --timeout MS (default 5000) for its element to be there, visible, enabled, still,
      and not covered, then sends its input once. Page commands act on the target `bctl tab use` selected;
      every one takes --json.
  bctl agent-browser-plugin [--session NAME | --image IMAGE [--allow-private CIDR]...]
                                                     agent-browser's browser.provider plugin (docs/agent-browser.md)
  bctl image pull [IMAGE]                            install a release image (default: nanos-helium) and verify it
  bctl doctor [--json]                               check the runtime and images
  bctl daemon                                        run the daemon in the foreground";

enum Cli {
    Request { op: Op, request: Box<Request>, json: bool, open: bool },
    Doctor { json: bool },
    ImagePull { image: Option<String> },
    ProfileLogin { profile: String, image: Option<String> },
    AgentBrowserPlugin(agent_browser::Lease),
    WebMcp(webmcp::Command),
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
        "webmcp" => return Ok(Cli::WebMcp(webmcp::parse(parser)?)),
        "agent-browser-plugin" => return parse_agent_browser_plugin(parser),
        "image" => return parse_image_command(parser),
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
            Long("url") if op == Op::CdpRevoke => request.url = Some(parser.value()?.string()?),
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
        _ => Cli::Request { op, request: Box::new(request), json, open },
    })
}

const IMAGE_USAGE: &str = "usage: bctl image pull [IMAGE]";

fn parse_image_command(mut parser: lexopt::Parser) -> Result<Cli, lexopt::Error> {
    match parser.next()? {
        Some(Value(v)) if v == "pull" => {}
        _ => return Err(IMAGE_USAGE.into()),
    }
    let mut image = None;
    while let Some(arg) = parser.next()? {
        match arg {
            Value(v) if image.is_none() => image = Some(v.string()?),
            _ => return Err(arg.unexpected()),
        }
    }
    Ok(Cli::ImagePull { image })
}

const PLUGIN_USAGE: &str = "usage: bctl agent-browser-plugin [--session NAME | --image IMAGE [--allow-private CIDR]...]";

fn parse_agent_browser_plugin(mut parser: lexopt::Parser) -> Result<Cli, lexopt::Error> {
    let (mut borrow, mut image, mut allow_private) = (None, None, Vec::new());
    while let Some(arg) = parser.next()? {
        match arg {
            Long("session") => borrow = Some(parser.value()?.string()?),
            Long("image") => image = Some(parser.value()?.string()?),
            Long("allow-private") => allow_private.push(parser.value()?.string()?),
            _ => return Err(arg.unexpected()),
        }
    }
    let lease = match borrow {
        Some(_) if image.is_some() || !allow_private.is_empty() => return Err(PLUGIN_USAGE.into()),
        Some(name) => agent_browser::Lease::Borrowed(bctl_core::SessionName::new(&name).map_err(|e| e.to_string())?),
        None => agent_browser::Lease::Owned { image, allow_private },
    };
    Ok(Cli::AgentBrowserPlugin(lease))
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
        "pause" | "resume" => {
            let [target] = positional.as_slice() else { return Err(format!("bctl {command} needs one run id or session name").into()) };
            match target.parse::<i64>() {
                Ok(run) => request.run = Some(run),
                Err(_) => request.session = Some(target.clone()),
            }
        }
        _ => {
            let [run] = positional.as_slice() else { return Err(format!("bctl {command} needs one run id").into()) };
            request.run = Some(run.parse().map_err(|_| format!("{run:?} is not a run id"))?);
        }
    }
    if op == Op::Run && request.session.is_none() {
        return Err(format!("bctl {command} needs --session SESSION").into());
    }
    Ok(Cli::Request { op, request: Box::new(request), json, open: false })
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
    Ok(Cli::Request { op, request: Box::new(request), json, open: false })
}

const PAGE_USAGE: &str = "usage: bctl page snapshot SESSION [--full] [--depth N] [--scope CSS] | resolve SESSION REF [--json]";

fn parse_page_command(mut parser: lexopt::Parser) -> Result<Cli, lexopt::Error> {
    let op = match parser.next()? {
        Some(Value(v)) => match v.string()?.as_str() {
            "snapshot" => Op::PageSnapshot,
            "resolve" => Op::PageResolve,
            "batch" => return parse_page_batch(parser),
            verb => match Op::parse(&format!("page-{verb}")) {
                Some(op) => return parse_page_action(op, parser),
                None => return Err(format!("unknown page command {verb:?}; see `bctl --help`").into()),
            },
        },
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
    Ok(Cli::Request { op, request: Box::new(request), json, open: false })
}

const BATCH_USAGE: &str = "usage: bctl page batch SESSION FILE|- [--json]
  FILE holds a JSON list of steps such as [{\"action\":\"goto\",\"url\":\"https://example.com/\"},{\"action\":\"click\",\"role\":\"link\",\"name\":\"More\"}]";

fn parse_page_batch(mut parser: lexopt::Parser) -> Result<Cli, lexopt::Error> {
    let mut json = false;
    let mut positional = Vec::new();
    while let Some(arg) = parser.next()? {
        match arg {
            Long("json") => json = true,
            Value(v) => positional.push(v.string()?),
            _ => return Err(arg.unexpected()),
        }
    }
    let [session, file] = <[String; 2]>::try_from(positional).map_err(|_| BATCH_USAGE)?;
    let text = if file == "-" {
        std::io::read_to_string(std::io::stdin()).map_err(|e| format!("cannot read the batch from stdin: {e}"))?
    } else {
        std::fs::read_to_string(&file).map_err(|e| format!("cannot read {file}: {e}"))?
    };
    let steps = match json::from_str::<json::Value>(&text) {
        Ok(json::Value::Array(steps)) => steps,
        _ => return Err(format!("{file} is not a JSON list of steps\n{BATCH_USAGE}").into()),
    };
    let request = Request { op: Op::PageBatch.as_str().to_string(), session: Some(session), batch: Some(steps.into_iter().collect()), ..Request::empty() };
    Ok(Cli::Request { op: Op::PageBatch, request: Box::new(request), json, open: false })
}

fn parse_page_action(op: Op, mut parser: lexopt::Parser) -> Result<Cli, lexopt::Error> {
    let usage = || -> lexopt::Error { format!("bctl {} arguments are missing or extra; see `bctl --help`", op.as_str().replacen('-', " ", 1)).into() };
    let number = |flag: &str, text: String| -> Result<u64, lexopt::Error> { text.parse().map_err(|_| format!("--{flag} {text:?} is not a whole number").into()) };
    let mut json = false;
    let mut action = ActionRequest::default();
    let mut positional = Vec::new();
    while let Some(arg) = parser.next()? {
        match arg {
            Long("json") => json = true,
            Long("css") => action.css = Some(parser.value()?.string()?),
            Long("role") => action.role = Some(parser.value()?.string()?),
            Long("name") => action.name = Some(parser.value()?.string()?),
            Long("label") => action.label = Some(parser.value()?.string()?),
            Long("placeholder") => action.placeholder = Some(parser.value()?.string()?),
            Long("testid") => action.testid = Some(parser.value()?.string()?),
            Long("nth") => {
                let nth = number("nth", parser.value()?.string()?)?;
                action.nth = Some(u32::try_from(nth).map_err(|_| "--nth is too large")?);
            }
            Long("wait") if op == Op::PageGoto => action.load = Some(parser.value()?.string()?),
            Long("load") if op == Op::PageWait => action.load = Some(parser.value()?.string()?),
            Long("timeout") if !matches!(op, Op::PageScreenshot | Op::PageEval | Op::PageDialog) => action.timeout_ms = Some(number("timeout", parser.value()?.string()?)?),
            Long("text") if matches!(op, Op::PageWait | Op::PageDialog) => action.text = Some(parser.value()?.string()?),
            Long("url") if op == Op::PageWait => action.url = Some(parser.value()?.string()?),
            Long("gone") if op == Op::PageWait => action.gone = true,
            Long(flag @ ("dx" | "dy")) if op == Op::PageScroll => {
                let flag = flag.to_string();
                let text = parser.value()?.string()?;
                let distance: f64 = text.parse().map_err(|_| format!("--{flag} {text:?} is not a number"))?;
                if flag == "dx" { action.dx = Some(distance) } else { action.dy = Some(distance) }
            }
            Value(v) => positional.push(v.string()?),
            _ => return Err(arg.unexpected()),
        }
    }
    let named_by_flag = [&action.css, &action.role, &action.label, &action.placeholder, &action.testid].iter().any(|f| f.is_some());
    let mut positional = positional.into_iter().peekable();
    let session = positional.next().ok_or_else(usage)?;
    let mut element = |positional: &mut std::iter::Peekable<std::vec::IntoIter<String>>| {
        if !named_by_flag && positional.peek().is_some_and(|p| p.starts_with('@')) {
            action.element = positional.next();
        }
    };
    match op {
        Op::PageGoto => action.url = Some(positional.next().ok_or_else(usage)?),
        Op::PageClick | Op::PageDblclick | Op::PageHover | Op::PageCheck | Op::PageUncheck | Op::PageScroll | Op::PageWait => element(&mut positional),
        Op::PageFill | Op::PageType => {
            element(&mut positional);
            action.text = Some(positional.next().ok_or_else(usage)?);
        }
        Op::PagePress => {
            element(&mut positional);
            action.key = Some(positional.next().ok_or_else(usage)?);
        }
        Op::PageSelect => {
            element(&mut positional);
            action.values = positional.by_ref().collect();
        }
        Op::PageGet => {
            let read = positional.next().ok_or_else(usage)?;
            if read == "attribute" {
                action.attribute = Some(positional.next().ok_or_else(usage)?);
            }
            action.read = Some(read);
            element(&mut positional);
        }
        Op::PageScreenshot => {
            let path = std::path::PathBuf::from(positional.next().ok_or_else(usage)?);
            let path = if path.is_absolute() { path } else { std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?.join(path) };
            action.path = Some(path.to_str().ok_or("the screenshot path is not UTF-8")?.to_string());
        }
        Op::PageEval => action.expression = Some(positional.next().ok_or_else(usage)?),
        Op::PageDialog => action.answer = Some(positional.next().ok_or_else(usage)?),
        _ => unreachable!("{} is not a page action", op.as_str()),
    }
    if positional.next().is_some() {
        return Err(usage());
    }
    let request = Request { op: op.as_str().to_string(), session: Some(session), action: Some(Box::new(action)), ..Request::empty() };
    Ok(Cli::Request { op, request: Box::new(request), json, open: false })
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
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some(runtime::NANOS_SUPERVISOR_COMMAND) {
        return bctl_runtime_nanos::supervisor::main(&[runtime::NANOS_SUPERVISOR_COMMAND], &raw[1..]);
    }
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
        Cli::ImagePull { image } => image_pull(&paths, image),
        Cli::ProfileLogin { profile, image } => profile_login(&paths, profile, image),
        Cli::AgentBrowserPlugin(lease) => agent_browser::run(&paths, &lease),
        Cli::WebMcp(command) => webmcp::run(&paths, &command),
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

fn image_pull(paths: &Paths, image: Option<String>) -> ExitCode {
    let result = RuntimeKind::for_host().and_then(|kind| {
        if kind != RuntimeKind::Nanos {
            return Err(format!("bctl image pull installs Nanos images; BCTL_RUNTIME selects {}", kind.as_str()));
        }
        let manifest = image::Manifest::embedded()?;
        image::pull(&paths.images(), &manifest, image.as_deref().unwrap_or(kind.default_image()))
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bctl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn send(paths: &Paths, request: &Request) -> std::io::Result<Response> {
    let mut stream = UnixStream::connect(paths.socket())?;
    let mut line = json::to_string(request);
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line)? == 0 {
        return Err(std::io::Error::new(ErrorKind::UnexpectedEof, "daemon closed without answering"));
    }
    json::from_str(&line).map_err(|_| std::io::Error::new(ErrorKind::InvalidData, format!("daemon sent {line:?}")))
}

/// How long a command waits for a daemon to start and answer.
const DAEMON_START_DEADLINE: Duration = Duration::from_secs(15);

fn never_delivered(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe)
}

fn daemon_owns_lock(paths: &Paths) -> bool {
    std::fs::File::open(paths.lock()).is_ok_and(|lock| lock.try_lock().is_err())
}

fn spawn_daemon(paths: &Paths) -> std::io::Result<()> {
    let log = daemon::open_log(paths)?;
    Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .map(drop)
}

fn retryable_direct_op(op: &str) -> bool {
    matches!(
        Op::parse(op),
        Some(Op::Ping | Op::Status | Op::Inspect | Op::TabList | Op::PageSnapshot | Op::PageResolve | Op::PageGet)
    )
}

// macOS reports EOF when a Unix peer closes with unread bytes, so only a side-effect-free ping can safely retry EOF.
fn call(paths: &Paths, request: &Request) -> Result<Response, Box<ErrorBody>> {
    let unreachable = |e: std::io::Error| {
        Box::new(ErrorBody::new(ErrorCode::DaemonUnreachable, e.to_string(), Some(format!("see {}", paths.log().display()))))
    };
    let mut deadline = None;
    let mut spawned = false;
    let mut pause = Duration::from_micros(200);
    let direct = retryable_direct_op(&request.op);
    let ping = Request { op: Op::Ping.as_str().into(), ..Request::empty() };
    loop {
        let error = if direct {
            match send(paths, request) {
                Ok(response) => return Ok(response),
                Err(e) if never_delivered(&e) || e.kind() == ErrorKind::UnexpectedEof => e,
                Err(e) => return Err(unreachable(e)),
            }
        } else {
            match send(paths, &ping) {
                Ok(_) => match send(paths, request) {
                    Ok(response) => return Ok(response),
                    Err(e) if never_delivered(&e) => e,
                    Err(e) => return Err(unreachable(e)),
                },
                Err(e) if never_delivered(&e) || e.kind() == ErrorKind::UnexpectedEof => e,
                Err(e) => return Err(unreachable(e)),
            }
        };
        let deadline = *deadline.get_or_insert_with(|| Instant::now() + DAEMON_START_DEADLINE);
        if Instant::now() > deadline {
            return Err(unreachable(error));
        }
        if !spawned && !daemon_owns_lock(paths) {
            spawn_daemon(paths).map_err(unreachable)?;
            spawned = true;
        }
        thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(5));
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
        if let Some(candidates) = &error.candidates {
            candidates.iter().for_each(|c| eprintln!("  @{}  {}  frame {}", c.element, c.summary, c.frame));
        }
        if let Some(dialog) = &error.dialog {
            eprintln!("  {} {}", dialog.kind, json::to_string(&dialog.message));
        }
    }
    ExitCode::FAILURE
}

fn render(op: Op, response: &Response, json: bool, detail: bool) -> ExitCode {
    if json {
        println!("{}", json::to_string(response));
        return if response.error.is_some() { ExitCode::FAILURE } else { ExitCode::SUCCESS };
    }
    if let Some(batch) = response.page.as_ref().and_then(|p| p.batch.as_ref()) {
        for (number, step) in batch.ran.iter().enumerate() {
            if let Some(result) = &step.result {
                print!("{} ", number + 1);
                print_action(Op::parse(&format!("page-{}", step.action)).unwrap_or(op), result);
            }
        }
    }
    if let Some(error) = &response.error {
        return report(error, false);
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
        Op::Run | Op::Cancel => response.runs.iter().for_each(print_run),
        Op::Pause | Op::Resume => {
            if response.sessions.is_empty() {
                response.runs.iter().for_each(print_run);
            } else {
                response.sessions.iter().for_each(print_session);
            }
        }
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
        Op::PageGoto
        | Op::PageClick
        | Op::PageDblclick
        | Op::PageFill
        | Op::PageType
        | Op::PagePress
        | Op::PageHover
        | Op::PageScroll
        | Op::PageSelect
        | Op::PageCheck
        | Op::PageUncheck
        | Op::PageWait
        | Op::PageGet
        | Op::PageScreenshot
        | Op::PageEval
        | Op::PageDialog => {
            if let Some(action) = response.page.as_ref().and_then(|p| p.action.as_ref()) {
                print_action(op, action);
            }
        }
        Op::PageBatch => {}
    }
    let failed = matches!(op, Op::Run | Op::Resume) && response.runs.iter().any(|r| r.state == "failed");
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn print_action(op: Op, action: &ActionView) {
    match op {
        Op::PageGet => match (&action.text, &action.value, action.redacted) {
            (Some(text), _, _) => println!("{text}"),
            (None, Some(json::Value::String(value)), _) => println!("{value}"),
            (None, _, Some(true)) => println!("<redacted>"),
            _ => {}
        },
        Op::PageEval => println!("{}", json::to_string(action.value.as_ref().unwrap_or(&json::Value::Null))),
        _ => {
            let mut detail = Vec::new();
            if let Some(element) = &action.element {
                detail.push(format!("@{element}"));
            }
            if let Some(changed) = action.changed {
                detail.push(if changed { "changed".into() } else { "already so".into() });
            }
            if let Some(selected) = &action.selected {
                detail.push(format!("selected {}", json::to_string(selected)));
            }
            if let Some(matched) = action.matched {
                detail.push(format!("{matched} matching"));
            }
            if let (Some(path), Some(bytes)) = (&action.path, action.bytes) {
                detail.push(format!("{path} ({bytes} bytes)"));
            }
            if let Some(dialog) = &action.dialog {
                detail.push(format!("{} {}", dialog.kind, json::to_string(&dialog.message)));
            }
            println!("{} {} {}  {} ms, {} CDP calls", action.action, action.target, detail.join("  "), action.elapsed_ms, action.cdp_calls);
        }
    }
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
    let runtime = RuntimeKind::for_host().and_then(|kind| Runtime::new(kind, paths));
    let (runtime_check, images_check) = match &runtime {
        Ok(runtime) => {
            let receipt = runtime.receipt(paths);
            let (complete, incomplete) = runtime.images(paths);
            let images = if complete.is_empty() && incomplete.is_empty() {
                (false, format!("none in {}", paths.images().display()))
            } else if incomplete.is_empty() {
                (true, complete.join(", "))
            } else {
                (false, complete.into_iter().chain(incomplete).collect::<Vec<_>>().join(", "))
            };
            ((receipt.is_ok(), receipt.unwrap_or_else(|e| e)), images)
        }
        Err(error) => ((false, error.clone()), (false, error.clone())),
    };
    let daemon = send(paths, &Request { op: Op::Ping.as_str().to_string(), ..Request::empty() }).is_ok();
    let checks = [
        ("home", true, paths.home.display().to_string()),
        ("runtime", runtime_check.0, runtime_check.1),
        ("images", images_check.0, images_check.1),
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
