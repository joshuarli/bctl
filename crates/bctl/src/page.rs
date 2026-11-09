use std::fs;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

use bctl_automation::{
    Acted, Action, ActionError, AttachError, Interrupt, Checked, Chord, ClickCount, Condition, Coverage, Dialog, DialogResponse, ElementRef, FieldValue, LoadState,
    Locator, NodeValue, Occurrence, Outcome, Pages, Query, Read, Readiness, RefCounter, RefCounterError, RefError, Scroll, SnapshotError, SnapshotNode,
    SnapshotOptions, TargetScope, Tabs,
};
use bctl_cdp::CallError;
use bctl_core::SessionName;
use futures_lite::future::block_on;
use miniserde::json;

use crate::authority::{Effect, InFlight};
use crate::daemon::{Daemon, Refusal};
use crate::protocol::{
    ActionRequest, ActionView, BatchView, CandidateView, DialogView, ElementView, ErrorBody, ErrorCode, NodeView, Op, PageView, Request, Response,
    SnapshotRequest, SnapshotView, StepView,
};
use crate::tabs::{self, SessionBindings, failure};

const GOTO_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
pub(crate) enum PageCommand {
    Snapshot,
    Resolve,
}

struct StoredRefCounter<'a> {
    daemon: &'a Daemon,
}

impl RefCounter for StoredRefCounter<'_> {
    fn reserve(&self, count: u64) -> Result<Range<u64>, RefCounterError> {
        self.daemon.store().reserve_element_refs(count).map_err(|error| RefCounterError(error.to_string()))
    }
}

pub(crate) fn handle(daemon: &Daemon, session: &SessionName, command: PageCommand, request: &Request) -> Result<Response, Refusal> {
    let _in_flight = tabs::begin(daemon, session, Effect::Reads)?;
    let browser = tabs::browser(daemon, session)?;
    let store = SessionBindings { daemon, session };
    let target = block_on(Tabs::new(&browser.connection, &store).selected()).map_err(|error| tabs::tab_failure(session, error))?;
    let mut pages = browser.pages.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match command {
        PageCommand::Snapshot => {
            let asked = request.snapshot.as_deref().cloned().unwrap_or_default();
            let options = options(&asked)?;
            let snapshot = block_on(pages.snapshot(&target, &options, &StoredRefCounter { daemon })).map_err(|e| snapshot_failure(session, &target, e))?;
            let text = snapshot.text();
            let view = SnapshotView {
                target: target.target.to_string(),
                text_bytes: text.len() as u64,
                text: (!asked.nodes).then_some(text),
                nodes: asked.nodes.then(|| snapshot.nodes.into_iter().map(node_view).collect()),
                omitted: snapshot.omitted as u64,
                documents: snapshot.documents as u32,
                cdp_calls: snapshot.cdp_calls,
            };
            Ok(Response { page: Some(Box::new(PageView { snapshot: Some(view), ..PageView::default() })), ..Response::empty() })
        }
        PageCommand::Resolve => {
            let text = request.element.as_deref().unwrap_or_default();
            let element = ElementRef::parse(text).ok_or_else(|| {
                failure(ErrorCode::BadRequest, format!("{text:?} is not an element ref"), Some("refs look like e12, as `bctl page snapshot` shows them".into()), None, None)
            })?;
            let resolved = block_on(pages.resolve(&target, element)).map_err(|e| ref_failure(session, &target, e))?;
            let accessible = block_on(pages.accessible(&resolved)).map_err(|e| cdp_failure(session, &target, e))?;
            let view = ElementView {
                element: element.to_string(),
                target: target.target.to_string(),
                frame: resolved.document.frame.to_string(),
                role: accessible.role,
                name: accessible.name,
            };
            Ok(Response { page: Some(Box::new(PageView { element: Some(view), ..PageView::default() })), ..Response::empty() })
        }
    }
}

fn options(asked: &SnapshotRequest) -> Result<SnapshotOptions, Refusal> {
    let bad = |message: String| failure(ErrorCode::BadRequest, message, None, None, None);
    let depth = match asked.depth {
        None => None,
        Some(depth) => Some(NonZeroUsize::new(depth as usize).ok_or_else(|| bad("--depth must be at least 1".into()))?),
    };
    if asked.scope.as_deref().is_some_and(|s| s.trim().is_empty()) {
        return Err(bad("--scope needs a CSS selector".into()).into());
    }
    Ok(SnapshotOptions { coverage: if asked.full { Coverage::Full } else { Coverage::Interactive }, depth, scope: asked.scope.clone() })
}

fn node_view(node: SnapshotNode) -> NodeView {
    let state = node.state;
    NodeView {
        element: node.element.map(|e| e.to_string()),
        role: node.role,
        name: (!node.name.is_empty()).then_some(node.name),
        redacted: matches!(node.value, Some(NodeValue::Redacted)).then_some(true),
        value: match node.value {
            Some(NodeValue::Text(text)) => Some(text),
            Some(NodeValue::Redacted) | None => None,
        },
        disabled: state.disabled.then_some(true),
        checked: state.checked.map(|checked| {
            match checked {
                Checked::Checked => "true",
                Checked::Unchecked => "false",
                Checked::Mixed => "mixed",
            }
            .to_string()
        }),
        selected: state.selected.then_some(true),
        expanded: state.expanded,
        level: state.level,
        children: (!node.children.is_empty()).then(|| node.children.into_iter().map(node_view).collect()),
    }
}

fn page_failure(target: &TargetScope, code: ErrorCode, message: String, remedy: Option<String>) -> Response {
    failure(code, message, remedy, Some(&target.target), None)
}

fn cdp_failure(session: &SessionName, target: &TargetScope, error: CallError) -> Response {
    match error {
        CallError::UnknownOutcome { .. } => page_failure(
            target,
            ErrorCode::UnknownOutcome,
            error.to_string(),
            Some(format!("bctl does not retry it; see what happened with `bctl tab list {session}`")),
        ),
        error => page_failure(target, ErrorCode::CdpFailed, error.to_string(), Some(format!("see `bctl inspect {session}`"))),
    }
}

fn attach_failure(session: &SessionName, target: &TargetScope, error: AttachError) -> Response {
    match error {
        AttachError::Detached => page_failure(
            target,
            ErrorCode::TargetGone,
            format!("the selected target {} went away: {error}", target.target),
            Some(format!("see `bctl tab list {session}`")),
        ),
        AttachError::Cdp(error) => cdp_failure(session, target, error),
        AttachError::Churning => page_failure(target, ErrorCode::CdpFailed, error.to_string(), Some("retry once the page settles".into())),
    }
}

fn snapshot_failure(session: &SessionName, target: &TargetScope, error: SnapshotError) -> Response {
    let message = error.to_string();
    match error {
        SnapshotError::InvalidScope { .. } => page_failure(target, ErrorCode::BadRequest, message, None),
        SnapshotError::ScopeNotFound { .. } => {
            page_failure(target, ErrorCode::ScopeNotFound, message, Some(format!("check the selector against `bctl page snapshot {session} --full`")))
        }
        SnapshotError::Attach(error) => attach_failure(session, target, error),
        SnapshotError::Cdp(error) => cdp_failure(session, target, error),
        SnapshotError::OtherGeneration | SnapshotError::Counter(_) => page_failure(target, ErrorCode::Internal, message, None),
    }
}

fn ref_failure(session: &SessionName, target: &TargetScope, error: RefError) -> Response {
    let message = error.to_string();
    match error {
        RefError::Stale { .. } => {
            page_failure(target, ErrorCode::StaleRef, message, Some(format!("take a new snapshot with `bctl page snapshot {session}` and use its refs")))
        }
        RefError::Attach(error) => attach_failure(session, target, error),
        RefError::Cdp(error) => cdp_failure(session, target, error),
    }
}

fn bad(message: impl Into<String>) -> Refusal {
    failure(ErrorCode::BadRequest, message, None, None, None).into()
}

fn locator(asked: &ActionRequest) -> Result<Option<Locator>, Refusal> {
    let text = |flag: &str, value: &Option<String>| -> Result<Option<String>, Refusal> {
        match value {
            Some(value) if value.trim().is_empty() => Err(bad(format!("--{flag} needs a value"))),
            value => Ok(value.clone()),
        }
    };
    let mut queries = Vec::new();
    if let Some(css) = text("css", &asked.css)? {
        queries.push(Query::Css(css));
    }
    match (text("role", &asked.role)?, text("name", &asked.name)?) {
        (Some(role), name) => queries.push(Query::Role { role, name }),
        (None, Some(_)) => return Err(bad("--name needs --role")),
        (None, None) => {}
    }
    if let Some(label) = text("label", &asked.label)? {
        queries.push(Query::Label(label));
    }
    if let Some(placeholder) = text("placeholder", &asked.placeholder)? {
        queries.push(Query::Placeholder(placeholder));
    }
    if let Some(id) = text("testid", &asked.testid)? {
        queries.push(Query::TestId(id));
    }
    let element = match asked.element.as_deref() {
        Some(text) => Some(ElementRef::parse(text).ok_or_else(|| bad(format!("{text:?} is not an element ref; refs look like @e12")))?),
        None => None,
    };
    let occurrence = match asked.nth {
        None => None,
        Some(n) => Some(Occurrence(NonZeroUsize::new(n as usize).ok_or_else(|| bad("--nth counts from 1"))?)),
    };
    let one_locator = "name the element once: a ref, or one of --css, --role, --label, --placeholder, --testid";
    match (element, queries.len(), occurrence) {
        (Some(_), 1.., _) | (None, 2.., _) => Err(bad(one_locator)),
        (Some(_), 0, Some(_)) | (None, 0, Some(_)) => Err(bad("--nth picks a match of --css, --role, --label, --placeholder, or --testid")),
        (Some(element), 0, None) => Ok(Some(Locator::Ref(element))),
        (None, 0, None) => Ok(None),
        (None, 1, occurrence) => Ok(Some(Locator::Query { query: queries.remove(0), occurrence })),
    }
}

fn required(locator: Option<Locator>, op: Op) -> Result<Locator, Refusal> {
    locator.ok_or_else(|| bad(format!("{} needs an element: a ref like @e12, or --css, --role, --label, --placeholder, or --testid", op.as_str())))
}

fn query(locator: Option<Locator>) -> Result<Option<Query>, Refusal> {
    match locator {
        None => Ok(None),
        Some(Locator::Query { query, occurrence: None }) => Ok(Some(query)),
        Some(_) => Err(bad("wait takes --css, --role, --label, --placeholder, or --testid without --nth; a ref already names a live element")),
    }
}

fn timeout(asked: &ActionRequest, default: Duration) -> Result<Duration, Refusal> {
    match asked.timeout_ms.map(Duration::from_millis) {
        None => Ok(default),
        Some(timeout) if timeout.is_zero() || timeout > MAX_TIMEOUT => Err(bad(format!("--timeout must be between 1 and {} ms", MAX_TIMEOUT.as_millis()))),
        Some(timeout) => Ok(timeout),
    }
}

fn needed<'a>(value: &'a Option<String>, what: &str) -> Result<&'a str, Refusal> {
    value.as_deref().ok_or_else(|| bad(format!("{what} is required")))
}

fn action(op: Op, asked: &ActionRequest) -> Result<Action, Refusal> {
    let locator = locator(asked)?;
    Ok(match op {
        Op::PageGoto => {
            let readiness = match asked.load.as_deref() {
                None | Some("load") => Readiness::Load,
                Some("commit") => Readiness::Commit,
                Some("domcontentloaded") => Readiness::DomContentLoaded,
                Some("networkidle") => Readiness::NetworkIdle,
                Some(other) => return Err(bad(format!("--wait {other:?} is not commit, domcontentloaded, load, or networkidle"))),
            };
            Action::Navigate { url: needed(&asked.url, "a URL")?.to_string(), readiness, timeout: timeout(asked, GOTO_TIMEOUT)? }
        }
        Op::PageClick => Action::Click { locator: required(locator, op)?, count: ClickCount::Single },
        Op::PageDblclick => Action::Click { locator: required(locator, op)?, count: ClickCount::Double },
        Op::PageHover => Action::Hover(required(locator, op)?),
        Op::PageFill => Action::Fill { locator: required(locator, op)?, text: needed(&asked.text, "the text to fill")?.to_string() },
        Op::PageType => Action::Type { into: locator, text: needed(&asked.text, "the text to type")?.to_string() },
        Op::PagePress => Action::Press { into: locator, chord: Chord::parse(needed(&asked.key, "a key")?).map_err(|e| bad(e.to_string()))? },
        Op::PageScroll => match (locator, asked.dx, asked.dy) {
            (Some(locator), None, None) => Action::Scroll(Scroll::IntoView(locator)),
            (None, dx, dy) if dx.is_some() || dy.is_some() => {
                let (dx, dy) = (dx.unwrap_or(0.0), dy.unwrap_or(0.0));
                if !dx.is_finite() || !dy.is_finite() {
                    return Err(bad("--dx and --dy must be finite"));
                }
                Action::Scroll(Scroll::By { dx, dy })
            }
            _ => return Err(bad("scroll takes an element to scroll into view, or --dx and --dy, not both")),
        },
        Op::PageSelect => {
            if asked.values.is_empty() {
                return Err(bad("select needs at least one option value or label"));
            }
            Action::Select { locator: required(locator, op)?, values: asked.values.clone() }
        }
        Op::PageCheck => Action::SetChecked { locator: required(locator, op)?, checked: true },
        Op::PageUncheck => Action::SetChecked { locator: required(locator, op)?, checked: false },
        Op::PageWait => {
            let query = query(locator)?;
            let condition = match (query, &asked.text, &asked.url, asked.load.as_deref(), asked.gone) {
                (Some(query), None, None, None, false) => Condition::Present(query),
                (Some(query), None, None, None, true) => Condition::Absent(query),
                (None, Some(text), None, None, false) => Condition::Text(text.clone()),
                (None, None, Some(url), None, false) => Condition::Url(url.clone()),
                (None, None, None, Some("domcontentloaded"), false) => Condition::Loaded(LoadState::DomContentLoaded),
                (None, None, None, Some("load"), false) => Condition::Loaded(LoadState::Load),
                (None, None, None, Some(other), false) => return Err(bad(format!("--load {other:?} is not domcontentloaded or load"))),
                _ => return Err(bad("wait for one thing: an element (optionally --gone), --text, --url, or --load")),
            };
            Action::Wait { condition, timeout: timeout(asked, WAIT_TIMEOUT)? }
        }
        Op::PageGet => match (asked.read.as_deref(), locator) {
            (Some("text"), locator) => Action::Read(Read::Text(locator)),
            (Some("title"), None) => Action::Read(Read::Title),
            (Some("url"), None) => Action::Read(Read::Url),
            (Some("attribute"), Some(locator)) => Action::Read(Read::Attribute { locator, name: needed(&asked.attribute, "the attribute name")?.to_string() }),
            (Some("value"), Some(locator)) => Action::Read(Read::Value(locator)),
            (Some("title" | "url"), Some(_)) => return Err(bad("get title and get url read the page, not an element")),
            (Some("attribute" | "value"), None) => return Err(bad("get attribute and get value need an element")),
            _ => return Err(bad("get reads text, title, url, attribute NAME, or value")),
        },
        Op::PageScreenshot => {
            let path = needed(&asked.path, "a path for the PNG")?;
            if !Path::new(path).is_absolute() {
                return Err(bad("the screenshot path must be absolute"));
            }
            Action::Screenshot
        }
        Op::PageEval => Action::Evaluate(needed(&asked.expression, "an expression")?.to_string()),
        Op::PageDialog => match asked.answer.as_deref() {
            Some("accept") => Action::Dialog(DialogResponse::Accept { text: asked.text.clone() }),
            Some("dismiss") if asked.text.is_none() => Action::Dialog(DialogResponse::Dismiss),
            Some("dismiss") => return Err(bad("--text answers a prompt you accept")),
            _ => return Err(bad("dialog takes accept or dismiss")),
        },
        _ => unreachable!("{} is not a page action", op.as_str()),
    })
}

fn write_png(path: &Path, png: &[u8]) -> Result<(), String> {
    let name = path.file_name().ok_or_else(|| format!("{} names no file", path.display()))?;
    let partial = path.with_file_name(format!(".{}.partial", name.to_string_lossy()));
    fs::write(&partial, png).map_err(|e| format!("cannot write {}: {e}", partial.display()))?;
    fs::rename(&partial, path).map_err(|e| format!("cannot move the screenshot to {}: {e}", path.display()))
}

fn dialog_view(dialog: Dialog) -> DialogView {
    DialogView { kind: dialog.kind, message: dialog.message, default_prompt: dialog.default_prompt }
}

pub(crate) fn act(daemon: &Daemon, session: &SessionName, op: Op, request: &Request) -> Result<Response, Refusal> {
    let started = Instant::now();
    let asked = request.action.as_deref().cloned().unwrap_or_default();
    let action = action(op, &asked)?;
    let in_flight = tabs::begin(daemon, session, effect(&action))?;
    let browser = tabs::browser(daemon, session)?;
    let store = SessionBindings { daemon, session };
    let target = block_on(Tabs::new(&browser.connection, &store).selected()).map_err(|error| tabs::tab_failure(session, error))?;
    let mut pages = browser.pages.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut view = run_step(daemon, session, &target, &mut pages, &Step { op, asked, action }, &in_flight)?;
    drop(pages);
    view.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(Response { page: Some(Box::new(PageView { action: Some(view), ..PageView::default() })), ..Response::empty() })
}

/// One page action, checked and ready to run.
struct Step {
    op: Op,
    asked: ActionRequest,
    action: Action,
}

fn effect(action: &Action) -> Effect {
    if action.mutates() { Effect::Mutates } else { Effect::Reads }
}

fn run_step(daemon: &Daemon, session: &SessionName, target: &TargetScope, pages: &mut Pages, step: &Step, in_flight: &InFlight<'_>) -> Result<ActionView, Refusal> {
    let Step { op, asked, action } = step;
    let acted = block_on(pages.act(target, action, &StoredRefCounter { daemon }, in_flight)).map_err(|e| action_failure(session, target, e))?;
    let Acted { on, outcome, cdp_calls, elapsed, .. } = acted;
    let mut view = ActionView {
        action: op_name(*op),
        target: target.target.to_string(),
        frame: on.as_ref().map(|on| on.frame.to_string()),
        element: on.and_then(|on| on.element).map(|e| e.to_string()),
        changed: None,
        selected: None,
        text: None,
        truncated: None,
        redacted: None,
        value: None,
        matched: None,
        path: None,
        bytes: None,
        dialog: None,
        cdp_calls,
        action_ms: elapsed.as_millis() as u64,
        elapsed_ms: 0,
    };
    match outcome {
        Outcome::Done => {}
        Outcome::Checked { changed } => view.changed = Some(changed),
        Outcome::Selected(values) => view.selected = Some(values),
        Outcome::Text { text, truncated } => {
            view.text = Some(text);
            view.truncated = Some(truncated);
        }
        Outcome::Attribute(value) => view.value = Some(value.map_or(json::Value::Null, json::Value::String)),
        Outcome::Value(FieldValue::Text(text)) => view.text = Some(text),
        Outcome::Value(FieldValue::Redacted) => view.redacted = Some(true),
        Outcome::Evaluated(value) => view.value = Some(value),
        Outcome::Matched(matches) => view.matched = Some(matches as u64),
        Outcome::Png(png) => {
            let path = needed(&asked.path, "a path for the PNG")?;
            write_png(Path::new(path), &png).map_err(|message| page_failure(target, ErrorCode::WriteFailed, message, None))?;
            view.path = Some(path.to_string());
            view.bytes = Some(png.len() as u64);
        }
        Outcome::DialogHandled(dialog) => view.dialog = dialog.map(dialog_view),
    }
    Ok(view)
}

/// The most steps one batch may hold.
pub(crate) const MAX_BATCH_STEPS: usize = 1000;

/// Reads one batch step: an object whose `action` names a page action and whose other fields are
/// [`ActionRequest`] fields. Unknown fields are refused rather than ignored.
fn plan(index: usize, step: &json::Value) -> Result<Step, Refusal> {
    let at = |message: String| bad(format!("batch step {}: {message}", index + 1));
    let json::Value::Object(fields) = step else { return Err(at("is not a JSON object".into())) };
    let verb = match fields.get("action") {
        Some(json::Value::String(verb)) => verb,
        _ => return Err(at("needs an \"action\" naming a page action, such as \"click\"".into())),
    };
    let op = Op::parse(&format!("page-{verb}")).filter(|op| Op::PAGE_ACTIONS.contains(op)).ok_or_else(|| at(format!("{verb:?} is not a page action")))?;
    let json::Value::Object(mut merged) = json::from_str::<json::Value>(&json::to_string(&ActionRequest::default())).expect("an ActionRequest is an object") else {
        unreachable!("an ActionRequest serializes as an object")
    };
    for (key, value) in fields.iter().filter(|(key, _)| *key != "action") {
        if !merged.contains_key(key) {
            return Err(at(format!("{verb} takes no field {key:?}")));
        }
        merged.insert(key.clone(), value.clone());
    }
    let asked: ActionRequest = json::from_str(&json::to_string(&json::Value::Object(merged))).map_err(|_| at(format!("a {verb} field has the wrong type")))?;
    let action = action(op, &asked).map_err(|refusal| {
        let message = refusal.0.error.as_ref().map(|e| e.message.clone()).unwrap_or_default();
        at(format!("{verb}: {message}"))
    })?;
    Ok(Step { op, asked, action })
}

/// Runs a batch's steps in order over the session's one connection, on the target selected when it began, and
/// stops at the first that fails. No other native command on the session runs between its steps. It is not a
/// transaction: what earlier steps did stays done.
pub(crate) fn batch(daemon: &Daemon, session: &SessionName, request: &Request) -> Result<Response, Refusal> {
    let started = Instant::now();
    let steps = request.batch.as_deref().unwrap_or_default();
    if steps.is_empty() || steps.len() > MAX_BATCH_STEPS {
        return Err(bad(format!("a batch holds 1 to {MAX_BATCH_STEPS} steps; this one has {}", steps.len())));
    }
    let planned = steps.iter().enumerate().map(|(index, step)| plan(index, step)).collect::<Result<Vec<_>, _>>()?;
    let mutates = planned.iter().any(|step| step.action.mutates());
    let in_flight = tabs::begin(daemon, session, if mutates { Effect::Mutates } else { Effect::Reads })?;
    let browser = tabs::browser(daemon, session)?;
    let store = SessionBindings { daemon, session };
    let target = block_on(Tabs::new(&browser.connection, &store).selected()).map_err(|error| tabs::tab_failure(session, error))?;
    let mut pages = browser.pages.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut batch = BatchView { target: target.target.to_string(), steps: planned.len() as u32, ran: Vec::new(), elapsed_ms: 0 };
    let mut failed = None;
    for (index, step) in planned.iter().enumerate() {
        let step_started = Instant::now();
        let ran = if in_flight.interrupted() { Err(taken_over(session, &target, "the batch").into()) } else { run_step(daemon, session, &target, &mut pages, step, &in_flight) };
        let elapsed_ms = step_started.elapsed().as_millis() as u64;
        let action = op_name(step.op);
        match ran {
            Ok(mut view) => {
                view.elapsed_ms = elapsed_ms;
                batch.ran.push(StepView { action, result: Some(view), error: None, elapsed_ms });
            }
            Err(refusal) => {
                let error = *refusal.0.error.expect("a refused step carries its error");
                let mut stopped = error.clone();
                stopped.message = format!("batch step {} of {} ({action}) failed: {}", index + 1, planned.len(), error.message);
                batch.ran.push(StepView { action, result: None, error: Some(error), elapsed_ms });
                failed = Some(stopped);
                break;
            }
        }
    }
    drop(pages);
    batch.elapsed_ms = started.elapsed().as_millis() as u64;
    let response = Response { error: failed.map(Box::new), page: Some(Box::new(PageView { batch: Some(batch), ..PageView::default() })), ..Response::empty() };
    if response.error.is_some() { Err(response.into()) } else { Ok(response) }
}

fn op_name(op: Op) -> String {
    op.as_str().trim_start_matches("page-").to_string()
}

fn taken_over(session: &SessionName, target: &TargetScope, what: &str) -> Response {
    page_failure(
        target,
        ErrorCode::TakenOver,
        format!("a person took control of session {session}, so {what} stopped"),
        Some("resume automation in the dashboard, then retry".into()),
    )
}

fn action_failure(session: &SessionName, target: &TargetScope, error: ActionError) -> Response {
    let message = error.to_string();
    let snapshot = || Some(format!("see what the page shows with `bctl page snapshot {session}`"));
    match error {
        ActionError::InvalidLocator { .. } => page_failure(target, ErrorCode::BadRequest, message, None),
        ActionError::NotFound { .. } => page_failure(target, ErrorCode::NotFound, message, snapshot()),
        ActionError::Ambiguous { candidates, .. } => {
            let mut response = page_failure(target, ErrorCode::Ambiguous, message, Some("name one: use a candidate's ref, or add --nth N".into()));
            let body: &mut ErrorBody = response.error.as_deref_mut().expect("a failure has an error");
            body.candidates = Some(
                candidates.into_iter().map(|c| CandidateView { element: c.element.to_string(), frame: c.frame.to_string(), summary: c.summary }).collect(),
            );
            response
        }
        ActionError::Ref(error) => ref_failure(session, target, error),
        ActionError::NotVisible { .. } => page_failure(target, ErrorCode::NotVisible, message, snapshot()),
        ActionError::Disabled { .. } => page_failure(target, ErrorCode::Disabled, message, None),
        ActionError::Covered { .. } => page_failure(target, ErrorCode::Covered, message, Some("close or move what covers it first".into())),
        ActionError::Missed { .. } => page_failure(
            target,
            ErrorCode::Missed,
            message,
            Some("the page changed under the pointer; bctl does not click again, so check what happened before retrying".into()),
        ),
        ActionError::Detached { .. } => page_failure(target, ErrorCode::Detached, message, snapshot()),
        ActionError::NotActionable { .. } => page_failure(target, ErrorCode::NotActionable, message, None),
        ActionError::DialogOpen(dialog) => {
            let remedy = Some(format!("answer it with `bctl page dialog {session} accept` or `bctl page dialog {session} dismiss`"));
            let mut response = page_failure(target, ErrorCode::DialogOpen, message, remedy);
            response.error.as_deref_mut().expect("a failure has an error").dialog = Some(dialog_view(dialog));
            response
        }
        ActionError::NoDialog => page_failure(target, ErrorCode::NoDialog, message, None),
        ActionError::Timeout { .. } => page_failure(target, ErrorCode::Timeout, message, Some("wait longer with --timeout MS".into())),
        ActionError::Interrupted { .. } => taken_over(session, target, &message),
        ActionError::NavigationFailed { .. } => page_failure(target, ErrorCode::NavigationFailed, message, None),
        ActionError::Script { .. } => page_failure(target, ErrorCode::EvalFailed, message, None),
        ActionError::Attach(error) => attach_failure(session, target, error),
        ActionError::Cdp(error) => cdp_failure(session, target, error),
        ActionError::OtherGeneration | ActionError::Helper { .. } | ActionError::Counter(_) => page_failure(target, ErrorCode::Internal, message, None),
    }
}
