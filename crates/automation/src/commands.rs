use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use bctl_cdp::protocol::{Command, Effect, Empty, Frame, FrameId, SessionId};
use bctl_cdp::{CallError, Connection, Scope};
use miniserde::{Deserialize, Serialize, json};

use crate::COMMAND_DEADLINE;

pub(crate) struct Counted<'a> {
    pub(crate) connection: &'a Connection,
    calls: AtomicU32,
}

impl<'a> Counted<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Counted<'a> {
        Counted { connection, calls: AtomicU32::new(0) }
    }

    pub(crate) fn calls(&self) -> u32 {
        self.calls.load(Ordering::Relaxed)
    }

    pub(crate) async fn browser<C: Command>(&self, command: &C) -> Result<C::Returns, CallError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.connection.call(Scope::Browser, command, Instant::now() + COMMAND_DEADLINE).await
    }

    pub(crate) async fn session<C: Command>(&self, session: &SessionId, command: &C) -> Result<C::Returns, CallError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.connection.call(Scope::Session(session), command, Instant::now() + COMMAND_DEADLINE).await
    }
}

#[derive(Serialize, Debug)]
struct TargetFilter {
    #[serde(rename = "type")]
    kind: String,
}

/// Attaches, in flattened mode, to every out-of-process iframe of the session's target as it appears, and
/// to nothing else (no workers).
#[derive(Serialize, Debug)]
pub(crate) struct AutoAttachFrames {
    #[serde(rename = "autoAttach")]
    auto_attach: bool,
    #[serde(rename = "waitForDebuggerOnStart")]
    wait_for_debugger_on_start: bool,
    flatten: bool,
    filter: Vec<TargetFilter>,
}

impl AutoAttachFrames {
    pub(crate) fn new() -> AutoAttachFrames {
        AutoAttachFrames { auto_attach: true, wait_for_debugger_on_start: false, flatten: true, filter: vec![TargetFilter { kind: "iframe".into() }] }
    }
}

impl Command for AutoAttachFrames {
    const METHOD: &'static str = "Target.setAutoAttach";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize, Debug)]
pub(crate) struct GetFrameTree {}

#[derive(Deserialize, Debug)]
pub(crate) struct FrameTreeReply {
    #[serde(rename = "frameTree")]
    pub(crate) frame_tree: FrameTree,
}

#[derive(Deserialize, Debug)]
pub(crate) struct FrameTree {
    pub(crate) frame: Frame,
    #[serde(rename = "childFrames")]
    pub(crate) child_frames: Option<Vec<FrameTree>>,
}

impl Command for GetFrameTree {
    const METHOD: &'static str = "Page.getFrameTree";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = FrameTreeReply;
}

#[derive(Serialize, Debug)]
pub(crate) struct GetFullAxTree {
    #[serde(rename = "frameId")]
    pub(crate) frame_id: FrameId,
}

#[derive(Deserialize, Debug)]
pub(crate) struct AxTree {
    pub(crate) nodes: Vec<AxNode>,
}

impl Command for GetFullAxTree {
    const METHOD: &'static str = "Accessibility.getFullAXTree";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = AxTree;
}

#[derive(Serialize, Debug)]
pub(crate) struct GetPartialAxTree {
    #[serde(rename = "backendNodeId")]
    pub(crate) backend_node_id: i64,
    #[serde(rename = "fetchRelatives")]
    pub(crate) fetch_relatives: bool,
}

impl Command for GetPartialAxTree {
    const METHOD: &'static str = "Accessibility.getPartialAXTree";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = AxTree;
}

#[derive(Deserialize, Debug)]
pub(crate) struct AxNode {
    #[serde(rename = "nodeId")]
    pub(crate) node_id: String,
    pub(crate) ignored: bool,
    pub(crate) role: Option<AxValue>,
    pub(crate) name: Option<AxValue>,
    pub(crate) value: Option<AxValue>,
    pub(crate) properties: Option<Vec<AxProperty>>,
    #[serde(rename = "childIds")]
    pub(crate) child_ids: Option<Vec<String>>,
    #[serde(rename = "backendDOMNodeId")]
    pub(crate) backend_dom_node_id: Option<i64>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct AxValue {
    pub(crate) value: Option<json::Value>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct AxProperty {
    pub(crate) name: String,
    pub(crate) value: AxValue,
}

/// Describes a node and, with `depth: -1` and `pierce`, its whole subtree including shadow roots and
/// in-process iframe documents. Unlike `DOM.getDocument` it starts no node tracking.
#[derive(Serialize, Debug)]
pub(crate) struct DescribeBackendNode {
    #[serde(rename = "backendNodeId")]
    pub(crate) backend_node_id: i64,
    pub(crate) depth: i32,
    pub(crate) pierce: bool,
}

impl Command for DescribeBackendNode {
    const METHOD: &'static str = "DOM.describeNode";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Described;
}

#[derive(Serialize, Debug)]
pub(crate) struct DescribeNode {
    #[serde(rename = "nodeId")]
    pub(crate) node_id: i64,
    pub(crate) depth: i32,
    pub(crate) pierce: bool,
}

impl Command for DescribeNode {
    const METHOD: &'static str = "DOM.describeNode";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Described;
}

#[derive(Deserialize, Debug)]
pub(crate) struct Described {
    pub(crate) node: DomNode,
}

#[derive(Deserialize, Debug)]
pub(crate) struct DomNode {
    #[serde(rename = "backendNodeId")]
    pub(crate) backend_node_id: i64,
    #[serde(rename = "localName")]
    pub(crate) local_name: Option<String>,
    pub(crate) attributes: Option<Vec<String>>,
    pub(crate) children: Option<Vec<DomNode>>,
    #[serde(rename = "shadowRoots")]
    pub(crate) shadow_roots: Option<Vec<DomNode>>,
    #[serde(rename = "contentDocument")]
    pub(crate) content_document: Option<Box<DomNode>>,
    #[serde(rename = "frameId")]
    pub(crate) frame_id: Option<FrameId>,
}

#[derive(Serialize, Debug)]
pub(crate) struct GetDocumentRoot {}

#[derive(Deserialize, Debug)]
pub(crate) struct DocumentRoot {
    pub(crate) root: NodeHandle,
}

#[derive(Deserialize, Debug)]
pub(crate) struct NodeHandle {
    #[serde(rename = "nodeId")]
    pub(crate) node_id: i64,
}

impl Command for GetDocumentRoot {
    const METHOD: &'static str = "DOM.getDocument";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = DocumentRoot;
}

#[derive(Serialize, Debug)]
pub(crate) struct QuerySelector {
    #[serde(rename = "nodeId")]
    pub(crate) node_id: i64,
    pub(crate) selector: String,
}

impl Command for QuerySelector {
    const METHOD: &'static str = "DOM.querySelector";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = NodeHandle;
}

#[derive(Serialize, Debug)]
pub(crate) struct ResolveNode {
    #[serde(rename = "backendNodeId")]
    pub(crate) backend_node_id: i64,
    #[serde(rename = "objectGroup")]
    pub(crate) object_group: String,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Resolved {
    pub(crate) object: RemoteObject,
}

#[derive(Deserialize, Debug)]
pub(crate) struct RemoteObject {
    #[serde(rename = "objectId")]
    pub(crate) object_id: Option<String>,
    pub(crate) value: Option<json::Value>,
}

impl Command for ResolveNode {
    const METHOD: &'static str = "DOM.resolveNode";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Resolved;
}

/// Whether the node behind `object_id` is still in a document. The function only reads.
#[derive(Serialize, Debug)]
pub(crate) struct IsConnected {
    #[serde(rename = "objectId")]
    object_id: String,
    #[serde(rename = "functionDeclaration")]
    function_declaration: String,
    #[serde(rename = "returnByValue")]
    return_by_value: bool,
}

impl IsConnected {
    pub(crate) fn new(object_id: String) -> IsConnected {
        IsConnected { object_id, function_declaration: "function() { return this.isConnected; }".into(), return_by_value: true }
    }
}

#[derive(Deserialize, Debug)]
pub(crate) struct Called {
    pub(crate) result: RemoteObject,
}

impl Command for IsConnected {
    const METHOD: &'static str = "Runtime.callFunctionOn";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Called;
}

#[derive(Serialize, Debug)]
pub(crate) struct ReleaseObjectGroup {
    #[serde(rename = "objectGroup")]
    pub(crate) object_group: String,
}

impl Command for ReleaseObjectGroup {
    const METHOD: &'static str = "Runtime.releaseObjectGroup";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Empty;
}
