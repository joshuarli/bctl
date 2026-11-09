mod daemon;
mod paths;
mod protocol;

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lexopt::prelude::*;
use miniserde::json;

use paths::Paths;
use protocol::{ErrorBody, ErrorCode, Op, Request, Response, SessionView};

const USAGE: &str = "usage:
  bctl up NAME [--profile PROFILE] [--image IMAGE] [--json]   start or reuse a browser session
  bctl down NAME [--json]                                     stop it, keeping its profile
  bctl rm NAME [--json]                                       forget a stopped session
  bctl status [--json]                               list sessions and why they are not ready
  bctl inspect NAME [--json]                         one session in detail
  bctl profile list [--json]                         profiles and the session holding each
  bctl doctor [--json]                               check the runtime and images
  bctl daemon                                        run the daemon in the foreground";

enum Cli {
    Request { op: Op, request: Request, json: bool },
    Doctor { json: bool },
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
        "inspect" => (Op::Inspect, true),
        "profile" => match parser.next()? {
            Some(Value(v)) if v == "list" => (Op::Profiles, false),
            _ => return Err("usage: bctl profile list".into()),
        },
        "doctor" | "daemon" => (Op::Ping, false),
        other => return Err(format!("unknown command {other:?}").into()),
    };
    let mut request = Request { op: op.as_str().to_string(), ..Request::empty() };
    let mut json = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Long("json") if command != "daemon" => json = true,
            Long("profile") if op == Op::Up => request.profile = Some(parser.value()?.string()?),
            Long("image") if op == Op::Up => request.image = Some(parser.value()?.string()?),
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
        _ => Cli::Request { op, request, json },
    })
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
        Cli::Request { op, request, json } => match call(&paths, &request) {
            Ok(response) => render(op, &response, json),
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
        println!("{}", json::to_string(&Response { error: Some(error.clone()), ..Response::empty() }));
    } else {
        eprintln!("bctl: {} [{}]", error.message, error.code);
        if let Some(remedy) = &error.remedy {
            eprintln!("  {remedy}");
        }
    }
    ExitCode::FAILURE
}

fn render(op: Op, response: &Response, json: bool) -> ExitCode {
    if let Some(error) = &response.error {
        return report(error, json);
    }
    if json {
        println!("{}", json::to_string(response));
        return ExitCode::SUCCESS;
    }
    match op {
        Op::Status => {
            println!("{:<20} {:<16} {:<10} {:<12} {:<22} IMAGE", "NAME", "PROFILE", "STATE", "WAITING ON", "CDP");
            for s in &response.sessions {
                let waiting = s.stage.clone().unwrap_or_else(|| "-".into());
                let cdp = s.cdp_url.clone().unwrap_or_else(|| "-".into());
                println!("{:<20} {:<16} {:<10} {:<12} {:<22} {}", s.name, s.profile, s.state, waiting, cdp, s.image);
            }
        }
        Op::Profiles => {
            for p in &response.profiles {
                println!("{:<20} {}", p.name, p.holder.as_deref().unwrap_or("-"));
            }
        }
        Op::Up | Op::Down | Op::Inspect => response.sessions.iter().for_each(print_session),
        Op::Rm | Op::Ping => {}
    }
    ExitCode::SUCCESS
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
    field("generation", Some(s.generation.to_string()));
    field("machine", s.machine.clone());
    field("cdp", s.cdp_url.clone());
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
