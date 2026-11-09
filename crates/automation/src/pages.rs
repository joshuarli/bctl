use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use bctl_cdp::protocol::{AttachToTarget, AttachedToTarget, CdpEvent, DetachFromTarget, DetachedFromTarget, FrameId, SessionId, TargetId};
use bctl_cdp::{CallError, Connection, Event, EventFilter, EventScope, Subscription, SubscriptionEnd};
use futures_lite::future::poll_once;
use miniserde::json;

use crate::commands::{
    AutoAttachFrames, AxTree, Counted, DescribeBackendNode, DescribeNode, FrameTree, GetDocumentRoot, GetFrameTree, GetFullAxTree, GetPartialAxTree,
    IsConnected, QuerySelector, ReleaseObjectGroup, ResolveNode,
};
use crate::refs::{BackendNodeId, ElementRef, Host, RefCounter, RefCounterError, RefTable};
use crate::snapshot::{self, Document, DomFacts, Drafts, FrameRead, MAX_DOCUMENTS, Snapshot, SnapshotOptions};
use crate::{DocumentId, DocumentScope, TargetScope};

const REF_OBJECT_GROUP: &str = "bctl-ref";

struct Attachment {
    page: SessionId,
    /// Keyed by the iframe's target id, which is also its frame id.
    oopifs: HashMap<TargetId, SessionId>,
    awaiting_auto_attach: Vec<SessionId>,
    events: Subscription,
}

enum Lost {
    Detached,
    Overflowed,
    Cdp(CallError),
}

impl Attachment {
    async fn open(cdp: &Counted<'_>, target: &TargetId) -> Result<Attachment, Lost> {
        let filter = EventFilter::new(&[AttachedToTarget::METHOD, DetachedFromTarget::METHOD], EventScope::Any);
        let events = cdp.connection.subscribe(filter).map_err(Lost::Cdp)?;
        let page = cdp.browser(&AttachToTarget::new(target.clone())).await.map_err(Lost::Cdp)?.session_id;
        let mut attachment = Attachment { page: page.clone(), oopifs: HashMap::new(), awaiting_auto_attach: vec![page], events };
        attachment.sync(cdp).await?;
        Ok(attachment)
    }

    fn owns(&self, session: Option<&SessionId>) -> bool {
        session.is_some_and(|s| *s == self.page || self.oopifs.values().any(|o| o == s))
    }

    fn apply(&mut self, event: &Event) -> Result<(), Lost> {
        if let Ok(attached) = event.decode::<AttachedToTarget>() {
            if self.owns(event.session()) && attached.target_info.kind == "iframe" {
                self.oopifs.insert(attached.target_info.target_id, attached.session_id.clone());
                self.awaiting_auto_attach.push(attached.session_id);
            }
        } else if let Ok(detached) = event.decode::<DetachedFromTarget>() {
            if detached.session_id == self.page {
                return Err(Lost::Detached);
            }
            self.oopifs.retain(|_, session| *session != detached.session_id);
            self.awaiting_auto_attach.retain(|session| *session != detached.session_id);
        }
        Ok(())
    }

    /// Auto-attach does not recurse: each new iframe session needs its own to attach nested iframes.
    async fn drain(&mut self) -> Result<(), Lost> {
        while let Some(next) = poll_once(self.events.next()).await {
            match next {
                Ok(event) => self.apply(&event)?,
                Err(SubscriptionEnd::Overflowed) => return Err(Lost::Overflowed),
                Err(SubscriptionEnd::Closed(termination)) => return Err(Lost::Cdp(CallError::Closed(termination))),
            }
        }
        Ok(())
    }

    async fn sync(&mut self, cdp: &Counted<'_>) -> Result<(), Lost> {
        loop {
            self.drain().await?;
            let Some(session) = self.awaiting_auto_attach.pop() else { return Ok(()) };
            match cdp.session(&session, &AutoAttachFrames::new()).await {
                Ok(_) => {}
                Err(CallError::SessionDetached { .. }) if session != self.page => {}
                Err(error) => return Err(Lost::Cdp(error)),
            }
        }
    }

    fn session_of(&self, host: &Host) -> Option<&SessionId> {
        match host {
            Host::Page => Some(&self.page),
            Host::Oopif(target) => self.oopifs.get(target),
        }
    }
}

/// Why bctl could not hold a current set of sessions on a target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachError {
    /// The target detached while it was being attached, as when it closes.
    Detached,
    /// Its iframes attached and detached faster than bctl could follow, on every try.
    Churning,
    Cdp(CallError),
}

impl fmt::Display for AttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttachError::Detached => f.write_str("the target detached while bctl attached to it"),
            AttachError::Churning => f.write_str("the target's iframes changed faster than bctl could follow"),
            AttachError::Cdp(error) => error.fmt(f),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// The target belongs to another browser generation than this connection's.
    OtherGeneration,
    InvalidScope { selector: String, reason: String },
    ScopeNotFound { selector: String },
    Attach(AttachError),
    Cdp(CallError),
    Counter(RefCounterError),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::OtherGeneration => f.write_str("the target belongs to another browser generation"),
            SnapshotError::InvalidScope { selector, reason } => write!(f, "{selector:?} is not a usable CSS selector: {reason}"),
            SnapshotError::ScopeNotFound { selector } => write!(f, "nothing in the page matches {selector:?}"),
            SnapshotError::Attach(error) => error.fmt(f),
            SnapshotError::Cdp(error) => error.fmt(f),
            SnapshotError::Counter(error) => write!(f, "cannot reserve element refs: {error}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<CallError> for SnapshotError {
    fn from(error: CallError) -> SnapshotError {
        SnapshotError::Cdp(error)
    }
}

/// Why a ref names no live node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Staleness {
    /// Not issued on this connection: from another session, an earlier generation or daemon, expired, or never
    /// issued.
    Unknown,
    /// Issued for another target than the one asked about.
    OtherTarget(TargetId),
    FrameGone,
    /// Its frame now shows another document.
    Navigated,
    /// Its node was removed from the document.
    NodeGone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefError {
    Stale { element: ElementRef, staleness: Staleness },
    Attach(AttachError),
    Cdp(CallError),
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefError::Stale { element, staleness } => {
                let why = match staleness {
                    Staleness::Unknown => "was not issued for this browser by this daemon, or has expired".to_string(),
                    Staleness::OtherTarget(target) => format!("belongs to target {target}, not the selected one"),
                    Staleness::FrameGone => "belongs to a frame that is gone".to_string(),
                    Staleness::Navigated => "belongs to a document its frame has navigated away from".to_string(),
                    Staleness::NodeGone => "names a node that is no longer in its document".to_string(),
                };
                write!(f, "ref {element} {why}")
            }
            RefError::Attach(error) => error.fmt(f),
            RefError::Cdp(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RefError {}

/// A ref that named a node still in its document when it was resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRef {
    pub element: ElementRef,
    pub document: DocumentScope,
    /// The flattened session that commands on the node must use.
    pub session: SessionId,
    pub node: BackendNodeId,
}

/// The browser-computed role and accessible name of a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accessible {
    pub role: String,
    pub name: String,
}

/// Snapshots and element refs over one connection. Refs live as long as this value: a new connection, as a new
/// generation or a restarted daemon opens, starts with none, so every earlier ref is [`Staleness::Unknown`].
pub struct Pages {
    connection: Connection,
    attachments: HashMap<TargetId, Attachment>,
    refs: RefTable,
}

struct HostFrames {
    session: SessionId,
    root: FrameId,
    documents: HashMap<FrameId, DocumentId>,
}

async fn frames(cdp: &Counted<'_>, session: &SessionId) -> Result<HostFrames, CallError> {
    let tree = cdp.session(session, &GetFrameTree {}).await?.frame_tree;
    let root = tree.frame.id.clone();
    let mut documents = HashMap::new();
    let mut stack: Vec<FrameTree> = vec![tree];
    while let Some(tree) = stack.pop() {
        documents.insert(tree.frame.id, DocumentId::new(tree.frame.loader_id));
        stack.extend(tree.child_frames.into_iter().flatten());
    }
    Ok(HostFrames { session: session.clone(), root, documents })
}

struct Job {
    host: Host,
    frame: FrameId,
    owner: Option<usize>,
    level: usize,
}

impl Pages {
    pub fn new(connection: Connection) -> Pages {
        Pages { connection, attachments: HashMap::new(), refs: RefTable::default() }
    }

    async fn attachment(&mut self, cdp: &Counted<'_>, target: &TargetId) -> Result<&Attachment, AttachError> {
        const TRIES: usize = 3;
        let mut closed = Vec::new();
        for (id, attachment) in self.attachments.iter_mut().filter(|(id, _)| *id != target) {
            if let Err(Lost::Detached) = attachment.drain().await {
                closed.push(id.clone());
            }
        }
        for id in closed {
            self.attachments.remove(&id);
        }
        for _ in 0..TRIES {
            let outcome = match self.attachments.get_mut(target) {
                Some(attachment) => attachment.sync(cdp).await,
                None => match Attachment::open(cdp, target).await {
                    Ok(attachment) => {
                        self.attachments.insert(target.clone(), attachment);
                        Ok(())
                    }
                    Err(Lost::Detached) => return Err(AttachError::Detached),
                    Err(lost) => Err(lost),
                },
            };
            match outcome {
                Ok(()) => return Ok(self.attachments.get(target).expect("attached")),
                Err(Lost::Cdp(error)) => return Err(AttachError::Cdp(error)),
                Err(Lost::Detached) => {
                    self.attachments.remove(target);
                }
                Err(Lost::Overflowed) => {
                    if let Some(stale) = self.attachments.remove(target) {
                        match cdp.browser(&DetachFromTarget { session_id: stale.page }).await {
                            Ok(_) | Err(CallError::Protocol { .. }) => {}
                            Err(error) => return Err(AttachError::Cdp(error)),
                        }
                    }
                }
            }
        }
        Err(AttachError::Churning)
    }

    /// Reads the target's accessibility tree, one bulk read per document, and gives each shown element a ref.
    /// Refs of documents found gone or replaced are forgotten.
    pub async fn snapshot(&mut self, target: &TargetScope, options: &SnapshotOptions, counter: &dyn RefCounter) -> Result<Snapshot, SnapshotError> {
        if target.generation != self.connection.generation() {
            return Err(SnapshotError::OtherGeneration);
        }
        let connection = self.connection.clone();
        let cdp = Counted::new(&connection);
        let attachment = self.attachment(&cdp, &target.target).await.map_err(SnapshotError::Attach)?;
        let page = attachment.page.clone();
        let oopifs = attachment.oopifs.clone();

        let mut hosts: HashMap<Host, HostFrames> = HashMap::new();
        let mut facts: HashMap<Host, DomFacts> = HashMap::new();
        hosts.insert(Host::Page, frames(&cdp, &page).await?);
        let main = hosts[&Host::Page].root.clone();
        let mut drafts = Drafts::default();
        let mut documents: Vec<Document> = Vec::new();
        let mut jobs = VecDeque::from([Job { host: Host::Page, frame: main.clone(), owner: None, level: 0 }]);
        while let Some(job) = jobs.pop_front() {
            if documents.len() == MAX_DOCUMENTS {
                break;
            }
            if !hosts.contains_key(&job.host) {
                let Some(session) = oopifs.get(match &job.host {
                    Host::Oopif(target) => target,
                    Host::Page => unreachable!("the page's frames are read first"),
                }) else {
                    continue;
                };
                hosts.insert(job.host.clone(), frames(&cdp, session).await?);
            }
            let host = &hosts[&job.host];
            let Some(document) = host.documents.get(&job.frame).cloned() else { continue };
            let session = host.session.clone();
            let tree: AxTree = cdp.session(&session, &GetFullAxTree { frame_id: job.frame.clone() }).await?;
            let index = documents.len();
            documents.push(Document {
                scope: DocumentScope { target: target.clone(), frame: job.frame.clone(), document },
                host: job.host.clone(),
            });
            let starts = match &options.scope {
                Some(selector) if job.frame == main && job.host == Host::Page => {
                    let subtree = scope_subtree(&cdp, &session, selector, &job.frame, facts.entry(job.host.clone()).or_default()).await?;
                    snapshot::roots_within(&tree.nodes, &subtree)
                }
                _ => {
                    let Some(root) = snapshot::root_of(&tree.nodes) else { continue };
                    let host_facts = facts.entry(job.host.clone()).or_default();
                    if snapshot::needs_dom(&tree.nodes)
                        && !host_facts.covered.contains(&job.frame)
                        && let Some(backend) = tree.nodes[root].backend_dom_node_id
                    {
                        let described = cdp.session(&session, &DescribeBackendNode { backend_node_id: backend, depth: -1, pierce: true }).await?;
                        host_facts.add(&described.node, job.frame.clone());
                    }
                    vec![root]
                }
            };
            let read = FrameRead { nodes: &tree.nodes, document: index, facts: facts.entry(job.host.clone()).or_default(), coverage: options.coverage, depth: options.depth };
            let found = read.read(&mut drafts, &starts, job.owner, job.level);
            for child in found {
                if options.depth.is_some_and(|limit| child.level >= limit.get()) {
                    continue;
                }
                let local = hosts[&job.host].documents.contains_key(&child.frame);
                let child_host = if local { job.host.clone() } else { Host::Oopif(TargetId::new(child.frame.as_str())) };
                if !local && !oopifs.contains_key(&TargetId::new(child.frame.as_str())) {
                    continue;
                }
                jobs.push_back(Job { host: child_host, frame: child.frame, owner: Some(child.owner), level: child.level });
            }
        }

        let listed: HashMap<Host, HashMap<FrameId, DocumentId>> = hosts.into_iter().map(|(host, frames)| (host, frames.documents)).collect();
        self.refs.retire(target, &listed);
        let (nodes, omitted) = snapshot::finish(&drafts, &documents, options.coverage, &mut self.refs, counter).map_err(SnapshotError::Counter)?;
        Ok(Snapshot { target: target.clone(), coverage: options.coverage, nodes, omitted, documents: documents.len(), cdp_calls: cdp.calls() })
    }

    /// The node `element` names, if it is still in the document it was issued in, on `target`. Refs are
    /// matched only by identity, never by role or name.
    pub async fn resolve(&mut self, target: &TargetScope, element: ElementRef) -> Result<ResolvedRef, RefError> {
        let stale = |staleness| Err(RefError::Stale { element, staleness });
        if target.generation != self.connection.generation() {
            return stale(Staleness::Unknown);
        }
        let Some(entry) = self.refs.get(element).cloned() else { return stale(Staleness::Unknown) };
        if entry.key.document.target != *target {
            return stale(Staleness::OtherTarget(entry.key.document.target.target.clone()));
        }
        let connection = self.connection.clone();
        let cdp = Counted::new(&connection);
        let attachment = self.attachment(&cdp, &target.target).await.map_err(RefError::Attach)?;
        let Some(session) = attachment.session_of(&entry.host).cloned() else {
            self.refs.forget(element);
            return stale(Staleness::FrameGone);
        };
        let scope = &entry.key.document;
        let current = frames(&cdp, &session).await.map_err(RefError::Cdp)?;
        let staleness = match current.documents.get(&scope.frame) {
            None => Some(Staleness::FrameGone),
            Some(document) if *document != scope.document => Some(Staleness::Navigated),
            Some(_) => match connected(&cdp, &session, entry.key.node).await.map_err(RefError::Cdp)? {
                true => None,
                false => Some(Staleness::NodeGone),
            },
        };
        if let Some(staleness) = staleness {
            self.refs.forget(element);
            return stale(staleness);
        }
        Ok(ResolvedRef { element, document: entry.key.document, session, node: entry.key.node })
    }

    /// The node's role and accessible name as the browser computes them now.
    pub async fn accessible(&self, resolved: &ResolvedRef) -> Result<Accessible, CallError> {
        let cdp = Counted::new(&self.connection);
        let tree = cdp.session(&resolved.session, &GetPartialAxTree { backend_node_id: resolved.node.0, fetch_relatives: false }).await?;
        let node = tree.nodes.iter().find(|n| n.backend_dom_node_id == Some(resolved.node.0)).or(tree.nodes.first());
        let text = |value: Option<&crate::commands::AxValue>| match value.and_then(|v| v.value.as_ref()) {
            Some(json::Value::String(s)) => snapshot::normalize(s),
            _ => String::new(),
        };
        let role = node.map(|n| text(n.role.as_ref())).unwrap_or_default();
        let role = if role == "Iframe" { "iframe".into() } else { role };
        Ok(Accessible { role, name: node.map(|n| text(n.name.as_ref())).unwrap_or_default() })
    }
}

async fn scope_subtree(
    cdp: &Counted<'_>,
    session: &SessionId,
    selector: &str,
    frame: &FrameId,
    facts: &mut DomFacts,
) -> Result<HashSet<BackendNodeId>, SnapshotError> {
    let document = cdp.session(session, &GetDocumentRoot {}).await?.root.node_id;
    let found = match cdp.session(session, &QuerySelector { node_id: document, selector: selector.to_string() }).await {
        Ok(found) => found.node_id,
        Err(CallError::Protocol { message, .. }) => return Err(SnapshotError::InvalidScope { selector: selector.to_string(), reason: message }),
        Err(error) => return Err(error.into()),
    };
    if found == 0 {
        return Err(SnapshotError::ScopeNotFound { selector: selector.to_string() });
    }
    let described = cdp.session(session, &DescribeNode { node_id: found, depth: -1, pierce: true }).await?;
    Ok(facts.add(&described.node, frame.clone()))
}

/// Whether the node is still in a document. A node the session cannot find has been collected.
async fn connected(cdp: &Counted<'_>, session: &SessionId, node: BackendNodeId) -> Result<bool, CallError> {
    let group = REF_OBJECT_GROUP.to_string();
    let object = match cdp.session(session, &ResolveNode { backend_node_id: node.0, object_group: group.clone() }).await {
        Ok(resolved) => resolved.object.object_id,
        Err(CallError::Protocol { .. }) => return Ok(false),
        Err(error) => return Err(error),
    };
    let Some(object) = object else { return Ok(false) };
    let called = cdp.session(session, &IsConnected::new(object)).await;
    cdp.session(session, &ReleaseObjectGroup { object_group: group }).await?;
    match called?.result.value {
        Some(json::Value::Bool(connected)) => Ok(connected),
        other => Err(CallError::MalformedReply { method: "Runtime.callFunctionOn", reason: format!("isConnected returned {other:?}") }),
    }
}
