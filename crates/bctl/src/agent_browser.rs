//! `bctl agent-browser-plugin`: a `browser.provider` plugin for agent-browser v0.42.0, speaking
//! `agent-browser.plugin.v1` (one JSON request on stdin, one JSON response on stdout). See docs/agent-browser.md.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::ExitCode;

use bctl_core::SessionName;
use miniserde::json::{self, Number, Object, Value};
use miniserde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::protocol::{ErrorBody, ErrorCode, Op, Request, Response, SessionView};
use crate::runtime::RuntimeKind;

const PROTOCOL: &str = "agent-browser.plugin.v1";
const CAPABILITY: &str = "browser.provider";
const OWNED_PREFIX: &str = "ab-";

/// Which bctl session a launch attaches to.
pub(crate) enum Lease {
    /// An existing session: launch mints a URL for it, close revokes that URL and leaves the session running.
    Borrowed(SessionName),
    /// A session the plugin starts for each agent-browser session and stops on close.
    Owned { image: Option<String>, allow_private: Vec<String> },
}

#[derive(Deserialize)]
struct Envelope {
    protocol: String,
    #[serde(rename = "type")]
    kind: String,
    capability: Option<String>,
    request: Option<Value>,
}

#[derive(Deserialize)]
struct LaunchRequest {
    session: Option<String>,
    #[serde(rename = "launchOptions")]
    launch_options: Option<Value>,
}

/// The opaque handle agent-browser stores and sends back with `browser.close`.
#[derive(Serialize, Deserialize)]
struct Cleanup {
    lease: String,
    session: String,
    url: String,
    generation: Option<u64>,
}

/// What the plugin records for a session it started, so only that generation is ever stopped.
#[derive(Serialize, Deserialize)]
struct Owner {
    generation: u64,
    url: String,
}

pub(crate) fn run(paths: &Paths, lease: &Lease) -> ExitCode {
    let mut input = String::new();
    let body = match std::io::stdin().read_to_string(&mut input) {
        Ok(_) => handle(paths, lease, &input),
        Err(e) => Err(format!("cannot read the request: {e}")),
    };
    let mut reply = Object::new();
    reply.insert("protocol".into(), string(PROTOCOL));
    match body {
        Ok(fields) => {
            reply.insert("success".into(), Value::Bool(true));
            reply.extend(fields);
        }
        Err(error) => {
            reply.insert("success".into(), Value::Bool(false));
            reply.insert("error".into(), string(&error));
        }
    }
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{}", json::to_string(&Value::Object(reply))).is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn handle(paths: &Paths, lease: &Lease, input: &str) -> Result<Object, String> {
    let envelope: Envelope = json::from_str(input).map_err(|_| "the request is not an agent-browser plugin request".to_string())?;
    if envelope.protocol != PROTOCOL {
        return Err(format!("unsupported protocol {:?}", envelope.protocol));
    }
    let request = envelope.request.unwrap_or(Value::Null);
    match envelope.kind.as_str() {
        "plugin.manifest" => Ok(manifest()),
        "browser.launch" | "browser.close" if envelope.capability.as_deref() != Some(CAPABILITY) => {
            Err(format!("{} needs the {CAPABILITY} capability", envelope.kind))
        }
        "browser.launch" => {
            let launch: LaunchRequest = json::from_str(&json::to_string(&request)).map_err(|_| "malformed browser.launch request".to_string())?;
            launch_browser(paths, lease, &launch)
        }
        "browser.close" => {
            let cleanup: Cleanup = json::from_str(&json::to_string(&request)).map_err(|_| "malformed cleanup handle".to_string())?;
            close_browser(paths, &cleanup).map(|()| Object::new())
        }
        other => Err(format!("unsupported request type {other:?}")),
    }
}

fn manifest() -> Object {
    let mut manifest = Object::new();
    manifest.insert("name".into(), string("bctl"));
    manifest.insert("capabilities".into(), Value::Array(vec![string(CAPABILITY)].into_iter().collect()));
    manifest.insert("description".into(), string("Helium in a bctl session VM, attached over a revocable CDP URL"));
    let mut fields = Object::new();
    fields.insert("manifest".into(), Value::Object(manifest));
    fields
}

/// Refuses launch options whose effect bctl cannot give, rather than ignoring them.
fn check_options(options: Option<&Value>, headless_runtime: bool) -> Result<(), String> {
    let Some(options) = options else { return Ok(()) };
    let Value::Object(options) = options else { return Err("launchOptions is not an object".into()) };
    for (key, value) in options {
        match (key.as_str(), value) {
            (_, Value::Null) => {}
            ("engine", Value::String(engine)) if engine == "chrome" => {}
            ("engine", Value::String(engine)) => return Err(format!("engine {engine:?} is unsupported: bctl sessions run Helium, a Chromium")),
            ("headed", Value::Bool(false)) => {}
            ("headed", Value::Bool(true)) if !headless_runtime => {}
            ("headed", Value::Bool(true)) => return Err("headed is unsupported: this runtime's sessions are headless".into()),
            (key, _) => return Err(format!("launch option {key} is unsupported by bctl")),
        }
    }
    Ok(())
}

fn launch_browser(paths: &Paths, lease: &Lease, launch: &LaunchRequest) -> Result<Object, String> {
    match lease {
        Lease::Borrowed(name) => {
            let view = session(paths, name)?.ok_or_else(|| format!("no bctl session named {name}"))?;
            check_options(launch.launch_options.as_ref(), view.display.as_deref() == Some("headless"))?;
            let url = mint(paths, name)?;
            Ok(browser(Cleanup { lease: "borrowed".into(), session: name.to_string(), url, generation: None }))
        }
        Lease::Owned { image, allow_private } => {
            let headless = RuntimeKind::for_host()? == RuntimeKind::Nanos;
            check_options(launch.launch_options.as_ref(), headless)?;
            let client = launch.session.as_deref().unwrap_or("default");
            let name = SessionName::new(&format!("{OWNED_PREFIX}{client}"))
                .map_err(|e| format!("agent-browser session {client:?} does not give a bctl session name: {e}"))?;
            if !reclaim(paths, &name)? && session(paths, &name)?.is_some() {
                return Err(format!("bctl session {name} exists and was not started by this plugin; pass --session {name} to borrow it"));
            }
            let up = Request {
                op: Op::Up.as_str().into(),
                session: Some(name.to_string()),
                image: image.clone(),
                allow_private: allow_private.clone(),
                ..Request::empty()
            };
            let started = call(paths, &up);
            let generation = match started.as_ref().map(|r| r.sessions.first().map(|s| s.generation)) {
                Ok(Some(generation)) => generation,
                Ok(None) => return Err(abandon(paths, &name, format!("bctl up {name} returned no session"))),
                Err(error) => return Err(abandon(paths, &name, error.clone())),
            };
            let url = mint(paths, &name).map_err(|e| abandon(paths, &name, e))?;
            write_owner(paths, &name, &Owner { generation, url: url.clone() }).map_err(|e| abandon(paths, &name, e))?;
            Ok(browser(Cleanup { lease: "owned".into(), session: name.to_string(), url, generation: Some(generation) }))
        }
    }
}

fn browser(cleanup: Cleanup) -> Object {
    let mut metadata = Object::new();
    metadata.insert("bctlSession".into(), string(&cleanup.session));
    metadata.insert("lease".into(), string(&cleanup.lease));
    if let Some(generation) = cleanup.generation {
        metadata.insert("generation".into(), Value::Number(Number::U64(generation)));
    }
    let mut browser = Object::new();
    browser.insert("cdpUrl".into(), string(&cleanup.url));
    browser.insert("directPage".into(), Value::Bool(false));
    browser.insert("metadata".into(), Value::Object(metadata));
    browser.insert("cleanup".into(), json::from_str(&json::to_string(&cleanup)).expect("a cleanup handle is JSON"));
    let mut fields = Object::new();
    fields.insert("browser".into(), Value::Object(browser));
    fields
}

fn close_browser(paths: &Paths, cleanup: &Cleanup) -> Result<(), String> {
    let name = SessionName::new(&cleanup.session).map_err(|e| e.to_string())?;
    match (cleanup.lease.as_str(), cleanup.generation) {
        ("borrowed", None) => revoke(paths, &name, &cleanup.url),
        ("owned", Some(generation)) => {
            let Some(owner) = read_owner(paths, &name)? else { return Ok(()) };
            if owner.generation != generation || owner.url != cleanup.url {
                return Err(format!("the cleanup handle does not match the session this plugin started as {name}"));
            }
            release(paths, &name, &owner)
        }
        _ => Err("malformed cleanup handle".into()),
    }
}

/// Stops a session a previous launch started and never closed, as when its agent-browser died, and keeps its
/// record so the next `up` gives it a new generation and so a new machine. Returns whether there was one.
fn reclaim(paths: &Paths, name: &SessionName) -> Result<bool, String> {
    let Some(owner) = read_owner(paths, name)? else { return Ok(false) };
    revoke(paths, name, &owner.url)?;
    if session(paths, name)?.is_some_and(|view| view.generation == owner.generation) {
        let down = Request { op: Op::Down.as_str().into(), session: Some(name.to_string()), ..Request::empty() };
        call(paths, &down)?;
    }
    forget_owner(paths, name)?;
    Ok(true)
}

/// Revokes the owned session's URL and removes it, unless it has since been started again outside the plugin.
fn release(paths: &Paths, name: &SessionName, owner: &Owner) -> Result<(), String> {
    revoke(paths, name, &owner.url)?;
    if session(paths, name)?.is_some_and(|view| view.generation == owner.generation) {
        remove_session(paths, name)?;
    }
    forget_owner(paths, name)
}

fn forget_owner(paths: &Paths, name: &SessionName) -> Result<(), String> {
    match fs::remove_file(owner_path(paths, name)) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(format!("cannot remove the owner record of {name}: {e}")),
        _ => Ok(()),
    }
}

/// Removes a session whose launch failed, and returns the launch's error with any removal failure added.
fn abandon(paths: &Paths, name: &SessionName, error: String) -> String {
    let _ = fs::remove_file(owner_path(paths, name));
    match remove_session(paths, name) {
        Ok(()) => error,
        Err(cleanup) => format!("{error}; then {cleanup}"),
    }
}

fn remove_session(paths: &Paths, name: &SessionName) -> Result<(), String> {
    for op in [Op::Down, Op::Rm] {
        match call(paths, &Request { op: op.as_str().into(), session: Some(name.to_string()), ..Request::empty() }) {
            Ok(_) => {}
            Err(e) if e.starts_with(ErrorCode::NoSuchSession.as_str()) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn session(paths: &Paths, name: &SessionName) -> Result<Option<SessionView>, String> {
    match call(paths, &Request { op: Op::Inspect.as_str().into(), session: Some(name.to_string()), ..Request::empty() }) {
        Ok(mut response) => Ok(response.sessions.pop()),
        Err(e) if e.starts_with(ErrorCode::NoSuchSession.as_str()) => Ok(None),
        Err(e) => Err(e),
    }
}

fn mint(paths: &Paths, name: &SessionName) -> Result<String, String> {
    let response = call(paths, &Request { op: Op::CdpUrl.as_str().into(), session: Some(name.to_string()), ..Request::empty() })?;
    response.url.ok_or_else(|| format!("bctl cdp-url {name} returned no URL"))
}

fn revoke(paths: &Paths, name: &SessionName, url: &str) -> Result<(), String> {
    let request = Request { op: Op::CdpRevoke.as_str().into(), session: Some(name.to_string()), url: Some(url.into()), ..Request::empty() };
    match call(paths, &request) {
        Err(e) if !e.starts_with(ErrorCode::NoSuchSession.as_str()) => Err(e),
        _ => Ok(()),
    }
}

/// One daemon request; a failure reads `CODE: message`.
fn call(paths: &Paths, request: &Request) -> Result<Response, String> {
    let failed = |e: &ErrorBody| format!("{}: {}", e.code, e.message);
    let response = crate::call(paths, request).map_err(|e| failed(&e))?;
    match &response.error {
        Some(error) => Err(failed(error)),
        None => Ok(response),
    }
}

fn owner_path(paths: &Paths, name: &SessionName) -> PathBuf {
    paths.home.join("agent-browser").join(format!("{name}.json"))
}

fn read_owner(paths: &Paths, name: &SessionName) -> Result<Option<Owner>, String> {
    let path = owner_path(paths, name);
    match fs::read_to_string(&path) {
        Ok(text) => json::from_str(&text).map(Some).map_err(|_| format!("{} is not an owner record", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn write_owner(paths: &Paths, name: &SessionName, owner: &Owner) -> Result<(), String> {
    let path = owner_path(paths, name);
    let dir = path.parent().expect("an owner record has a directory");
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    file.write_all(json::to_string(owner).as_bytes()).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn string(text: &str) -> Value {
    Value::String(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(text: &str) -> Value {
        json::from_str(text).unwrap()
    }

    #[test]
    fn the_defaults_agent_browser_sends_are_accepted_on_both_runtimes() {
        let defaults = options(r#"{"headed":false,"engine":"chrome","userAgent":null,"colorScheme":null}"#);
        assert_eq!(check_options(Some(&defaults), false), Ok(()));
        assert_eq!(check_options(Some(&defaults), true), Ok(()));
    }

    #[test]
    fn options_bctl_cannot_honour_are_refused() {
        let headed = options(r#"{"headed":true}"#);
        assert_eq!(check_options(Some(&headed), false), Ok(()));
        assert!(check_options(Some(&headed), true).unwrap_err().contains("headless"));
        assert!(check_options(Some(&options(r#"{"engine":"lightpanda"}"#)), false).unwrap_err().contains("lightpanda"));
        assert!(check_options(Some(&options(r#"{"userAgent":"x"}"#)), false).unwrap_err().contains("userAgent"));
        assert!(check_options(Some(&options(r#"{"colorScheme":"dark"}"#)), false).unwrap_err().contains("colorScheme"));
    }

    #[test]
    fn requests_outside_the_protocol_fail_without_reaching_the_daemon() {
        let paths = Paths { home: PathBuf::from("/nonexistent/bctl-plugin-test") };
        let lease = Lease::Owned { image: None, allow_private: Vec::new() };
        let ask = |text: &str| handle(&paths, &lease, text);
        assert!(ask(r#"{"protocol":"agent-browser.plugin.v2","type":"plugin.manifest"}"#).unwrap_err().contains("protocol"));
        assert!(ask(r#"{"protocol":"agent-browser.plugin.v1","type":"credential.resolve","capability":"credential.read"}"#).unwrap_err().contains("unsupported"));
        assert!(ask(r#"{"protocol":"agent-browser.plugin.v1","type":"browser.launch","capability":"launch.mutate"}"#).unwrap_err().contains(CAPABILITY));
        let manifest = ask(r#"{"protocol":"agent-browser.plugin.v1","type":"plugin.manifest","capability":"plugin.manifest","request":{}}"#).unwrap();
        assert!(json::to_string(&manifest).contains(r#""capabilities":["browser.provider"]"#), "{}", json::to_string(&manifest));
    }

    #[test]
    fn a_close_handle_must_name_its_lease_consistently() {
        let paths = Paths { home: PathBuf::from("/nonexistent/bctl-plugin-test") };
        let handle = |lease: &str, generation| Cleanup { lease: lease.into(), session: "ab-x".into(), url: "http://t.localhost:1".into(), generation };
        assert!(close_browser(&paths, &handle("owned", None)).unwrap_err().contains("malformed"));
        assert!(close_browser(&paths, &handle("borrowed", Some(1))).unwrap_err().contains("malformed"));
        assert_eq!(close_browser(&paths, &handle("owned", Some(1))), Ok(()), "no owner record: already closed");
    }
}
