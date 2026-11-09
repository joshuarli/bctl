use std::num::NonZeroUsize;
use std::ops::Range;

use bctl_automation::{
    AttachError, Checked, Coverage, ElementRef, NodeValue, RefCounter, RefCounterError, RefError, SnapshotError, SnapshotNode, SnapshotOptions, TargetScope,
    Tabs,
};
use bctl_cdp::CallError;
use bctl_core::SessionName;
use futures_lite::future::block_on;

use crate::daemon::{Daemon, Refusal};
use crate::protocol::{ElementView, ErrorCode, NodeView, PageView, Request, Response, SnapshotRequest, SnapshotView};
use crate::tabs::{self, SessionBindings, failure};

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
            Ok(Response { page: Some(Box::new(PageView { snapshot: Some(view), element: None })), ..Response::empty() })
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
            Ok(Response { page: Some(Box::new(PageView { snapshot: None, element: Some(view) })), ..Response::empty() })
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
