use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::thread;

use async_net::unix::UnixStream;
use bctl_automation::{Binding, BindingStore, BindingStoreError, Listing, PageTarget, SafeUrl, Selection, TabError, Tabs};
use bctl_cdp::protocol::TargetId;
use bctl_cdp::{CallError, Connection, Endpoint};
use bctl_core::{Generation, SelectedTarget, SessionName, SessionState, StoreError};
use futures_lite::future::block_on;
use miniserde::{Deserialize, json};

use crate::daemon::Daemon;
use crate::gateway;
use crate::protocol::{ErrorBody, ErrorCode, Request, Response, TabView, TabsView};

/// The daemon's CDP connection to each ready session's browser, opened on first use and tagged with the
/// generation it belongs to.
pub(crate) type Browsers = Mutex<HashMap<SessionName, Connection>>;

const DEFAULT_URL: &str = "about:blank";

fn lock(browsers: &Browsers) -> std::sync::MutexGuard<'_, HashMap<SessionName, Connection>> {
    browsers.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn failure(code: ErrorCode, message: impl Into<String>, remedy: Option<String>, target: Option<&TargetId>, targets: Option<Vec<TabView>>) -> Response {
    let mut body = ErrorBody::new(code, message, remedy);
    body.target = target.map(|t| t.to_string());
    body.targets = targets;
    Response { error: Some(Box::new(body)), ..Response::empty() }
}

fn views(targets: &[PageTarget]) -> Vec<TabView> {
    targets.iter().map(|t| TabView { id: t.id.to_string(), title: t.title.clone(), url: t.url.to_string() }).collect()
}

fn listed(targets: &[PageTarget], selected: Option<TargetId>) -> Response {
    let tabs = TabsView { selected: selected.map(|t| t.to_string()), targets: views(targets) };
    Response { tabs: Some(Box::new(tabs)), ..Response::empty() }
}

fn target_id(request: &Request) -> Result<TargetId, Response> {
    let id = request.target.as_deref().ok_or_else(|| failure(ErrorCode::BadRequest, "a target id is required", None, None, None))?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(failure(ErrorCode::BadRequest, format!("{id:?} is not a CDP target id"), Some("see `bctl tab list SESSION`".into()), None, None));
    }
    Ok(TargetId::new(id))
}

#[derive(Clone, Copy)]
pub(crate) enum TabCommand {
    List,
    New,
    Use,
    Close,
}

pub(crate) fn handle(daemon: &Daemon, session: &SessionName, command: TabCommand, request: &Request) -> Result<Response, Response> {
    let mutates = matches!(command, TabCommand::New | TabCommand::Close);
    if mutates && gateway::person_in_control(daemon, session) {
        return Err(failure(
            ErrorCode::HumanInControl,
            format!("a person has control of session {session}; native commands do not change its pages until they release it"),
            Some("release control in the dashboard, then retry".into()),
            None,
            None,
        ));
    }
    let connection = connection(daemon, session)?;
    let store = SessionBindings { daemon, session };
    let tabs = Tabs::new(&connection, &store);
    let listing = match command {
        TabCommand::List => block_on(tabs.list()),
        TabCommand::New => block_on(tabs.open(request.url.as_deref().unwrap_or(DEFAULT_URL))),
        TabCommand::Use => block_on(tabs.select(&target_id(request)?)),
        TabCommand::Close => block_on(tabs.close(&target_id(request)?)),
    }
    .map_err(|error| tab_failure(session, error))?;
    let Listing { targets, selection } = listing;
    match selection {
        Selection::Gone(binding) => Err(tab_failure(session, TabError::TargetGone { binding, remaining: targets })),
        Selection::Selected(selected) => Ok(listed(&targets, Some(selected))),
        Selection::Unselected => Ok(listed(&targets, None)),
    }
}

fn tab_failure(session: &SessionName, error: TabError) -> Response {
    let message = error.to_string();
    match error {
        TabError::TargetGone { binding, remaining } => failure(
            ErrorCode::TargetGone,
            message,
            Some(format!("choose one with `bctl tab use {session} TARGET_ID` or open one with `bctl tab new {session}`")),
            Some(&binding.target),
            Some(views(&remaining)),
        ),
        TabError::NotSelected { targets } => failure(
            ErrorCode::NoTargetSelected,
            message,
            Some(format!("choose one with `bctl tab use {session} TARGET_ID`")),
            None,
            Some(views(&targets)),
        ),
        TabError::NoSuchTarget { target, targets } => {
            failure(ErrorCode::NoSuchTarget, message, Some(format!("see `bctl tab list {session}`")), Some(&target), Some(views(&targets)))
        }
        TabError::LastTarget { target } => {
            failure(ErrorCode::LastTarget, message, Some(format!("open another first with `bctl tab new {session}`")), Some(&target), None)
        }
        TabError::Cdp(CallError::UnknownOutcome { .. }) => failure(
            ErrorCode::UnknownOutcome,
            message,
            Some(format!("bctl does not retry it; see what happened with `bctl tab list {session}`")),
            None,
            None,
        ),
        TabError::Cdp(_) => failure(ErrorCode::CdpFailed, message, Some(format!("see `bctl inspect {session}`")), None, None),
        TabError::Store(_) => failure(ErrorCode::Internal, message, None, None, None),
    }
}

/// The session's connection for its current generation, opening one if there is none or the one held belongs
/// to an earlier generation or has ended.
fn connection(daemon: &Daemon, session: &SessionName) -> Result<Connection, Response> {
    let record = daemon.store().session(session).map_err(|error| match error {
        StoreError::NoSuchSession(_) => failure(ErrorCode::NoSuchSession, format!("no session named {session}"), Some("see `bctl status`".into()), None, None),
        other => failure(ErrorCode::Internal, other.to_string(), None, None, None),
    })?;
    let not_ready = |detail: String| {
        failure(ErrorCode::SessionNotReady, format!("session {session} {detail}"), Some(format!("see `bctl inspect {session}`")), None, None)
    };
    if record.state != SessionState::Ready {
        return Err(not_ready(format!("is {}", record.state.as_str())));
    }
    let generation = bctl_cdp::Generation::new(record.generation.0);
    let mut browsers = lock(&daemon.browsers);
    if let Some(held) = browsers.get(session)
        && held.generation() == generation
        && held.termination().is_none()
    {
        return Ok(held.clone());
    }
    if let Some(stale) = browsers.remove(session) {
        thread::spawn(move || block_on(stale.shutdown()));
    }
    let socket = daemon.cdp_socket(session).ok_or_else(|| not_ready("is no longer ready".into()))?;
    let opened = open(&socket, generation).map_err(|e| not_ready(format!("does not answer CDP: {e}")))?;
    browsers.insert(session.clone(), opened.clone());
    Ok(opened)
}

fn open(socket: &Path, generation: bctl_cdp::Generation) -> Result<Connection, String> {
    #[derive(Deserialize)]
    struct Version {
        #[serde(rename = "webSocketDebuggerUrl")]
        url: String,
    }
    let version: Version = json::from_str(&gateway::cdp_get(socket, "/json/version")?).map_err(|_| "unexpected /json/version".to_string())?;
    let endpoint = Endpoint::parse(&version.url).map_err(|e| e.to_string())?;
    let stream = block_on(UnixStream::connect(socket)).map_err(|e| e.to_string())?;
    let (connection, driver) = block_on(bctl_cdp::open(stream, &endpoint, generation)).map_err(|e| e.to_string())?;
    thread::spawn(move || {
        let termination = block_on(driver);
        eprintln!("bctl daemon: CDP connection for generation {} ended: {termination}", generation.get());
    });
    Ok(connection)
}

/// Closes the session's connection, if it has one, and waits for it to finish.
pub(crate) fn close(daemon: &Daemon, session: &SessionName) {
    let held = lock(&daemon.browsers).remove(session);
    if let Some(connection) = held {
        block_on(connection.shutdown());
    }
}

struct SessionBindings<'a> {
    daemon: &'a Daemon,
    session: &'a SessionName,
}

fn core_generation(generation: bctl_cdp::Generation) -> Generation {
    Generation(generation.get())
}

fn store_error(error: StoreError) -> BindingStoreError {
    BindingStoreError(error.to_string())
}

impl BindingStore for SessionBindings<'_> {
    fn load(&self, generation: bctl_cdp::Generation) -> Result<Option<Binding>, BindingStoreError> {
        let selected = self.daemon.store().selected_target(self.session, core_generation(generation)).map_err(store_error)?;
        Ok(selected.map(|s| Binding { target: TargetId::new(s.target), url: SafeUrl::new(&s.url) }))
    }

    fn save(&self, generation: bctl_cdp::Generation, binding: &Binding) -> Result<(), BindingStoreError> {
        let selected = SelectedTarget { target: binding.target.to_string(), url: binding.url.to_string() };
        self.daemon.store().select_target(self.session, core_generation(generation), &selected).map_err(store_error)
    }

    fn release(&self, generation: bctl_cdp::Generation, target: &TargetId) -> Result<(), BindingStoreError> {
        self.daemon.store().release_target(self.session, core_generation(generation), target.as_str()).map_err(store_error)
    }
}
