use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use bctl_cdp::protocol::{CdpEvent, Command, Effect, Empty, Frame, FrameId, SessionId};
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

#[derive(Deserialize, Debug)]
pub(crate) struct Resolved {
    pub(crate) object: RemoteObject,
}

#[derive(Deserialize, Debug)]
pub(crate) struct RemoteObject {
    #[serde(rename = "objectId")]
    pub(crate) object_id: Option<String>,
    pub(crate) value: Option<json::Value>,
    #[serde(rename = "unserializableValue")]
    pub(crate) unserializable_value: Option<String>,
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

#[derive(Serialize, Debug)]
pub(crate) struct EnablePage {}

impl Command for EnablePage {
    const METHOD: &'static str = "Page.enable";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize, Debug)]
pub(crate) struct EnableLifecycleEvents {
    pub(crate) enabled: bool,
}

impl Command for EnableLifecycleEvents {
    const METHOD: &'static str = "Page.setLifecycleEventsEnabled";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

/// Chromium keeps one isolated world per frame and name, so asking again returns the same world's context
/// for the frame's current document. The page's own scripts cannot see into it or redefine what it calls.
#[derive(Serialize, Debug)]
pub(crate) struct CreateIsolatedWorld {
    #[serde(rename = "frameId")]
    pub(crate) frame_id: FrameId,
    #[serde(rename = "worldName")]
    pub(crate) world_name: String,
}

#[derive(Deserialize, Debug)]
pub(crate) struct IsolatedWorld {
    #[serde(rename = "executionContextId")]
    pub(crate) execution_context_id: i64,
}

impl Command for CreateIsolatedWorld {
    const METHOD: &'static str = "Page.createIsolatedWorld";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = IsolatedWorld;
}

#[derive(Serialize, Debug)]
pub(crate) struct ResolveNodeIn {
    #[serde(rename = "backendNodeId")]
    pub(crate) backend_node_id: i64,
    #[serde(rename = "executionContextId")]
    pub(crate) execution_context_id: i64,
    #[serde(rename = "objectGroup")]
    pub(crate) object_group: String,
}

impl Command for ResolveNodeIn {
    const METHOD: &'static str = "DOM.resolveNode";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Resolved;
}

#[derive(Deserialize, Debug)]
pub(crate) struct ExceptionDetails {
    pub(crate) text: String,
    pub(crate) exception: Option<ExceptionObject>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct ExceptionObject {
    pub(crate) description: Option<String>,
}

impl ExceptionDetails {
    pub(crate) fn message(&self) -> String {
        self.exception.as_ref().and_then(|e| e.description.clone()).unwrap_or_else(|| self.text.clone())
    }
}

#[derive(Deserialize, Debug)]
pub(crate) struct Evaluated {
    pub(crate) result: RemoteObject,
    #[serde(rename = "exceptionDetails")]
    pub(crate) exception_details: Option<ExceptionDetails>,
}

macro_rules! call_function {
    ($name:ident, $target_field:literal, $target_type:ty, $effect:expr) => {
        #[derive(Serialize, Debug)]
        pub(crate) struct $name {
            #[serde(rename = $target_field)]
            pub(crate) on: $target_type,
            #[serde(rename = "functionDeclaration")]
            pub(crate) function_declaration: String,
            pub(crate) arguments: Vec<json::Value>,
            #[serde(rename = "returnByValue")]
            pub(crate) return_by_value: bool,
            #[serde(rename = "awaitPromise")]
            pub(crate) await_promise: bool,
            #[serde(rename = "objectGroup")]
            pub(crate) object_group: String,
        }

        impl Command for $name {
            const METHOD: &'static str = "Runtime.callFunctionOn";
            const EFFECT: Effect = $effect;
            type Returns = Evaluated;
        }
    };
}

call_function!(InspectObject, "objectId", String, Effect::ReadOnly);
call_function!(ChangeObject, "objectId", String, Effect::Mutating);
call_function!(InspectContext, "executionContextId", i64, Effect::ReadOnly);

#[derive(Serialize, Debug)]
pub(crate) struct EvaluateInPage {
    pub(crate) expression: String,
    #[serde(rename = "returnByValue")]
    pub(crate) return_by_value: bool,
    #[serde(rename = "awaitPromise")]
    pub(crate) await_promise: bool,
}

impl Command for EvaluateInPage {
    const METHOD: &'static str = "Runtime.evaluate";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Evaluated;
}

#[derive(Serialize, Debug)]
pub(crate) struct GetOwnProperties {
    #[serde(rename = "objectId")]
    pub(crate) object_id: String,
    #[serde(rename = "ownProperties")]
    pub(crate) own_properties: bool,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Properties {
    pub(crate) result: Vec<Property>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Property {
    pub(crate) name: String,
    pub(crate) value: Option<RemoteObject>,
}

impl Command for GetOwnProperties {
    const METHOD: &'static str = "Runtime.getProperties";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Properties;
}

#[derive(Serialize, Debug)]
pub(crate) struct DescribeObject {
    #[serde(rename = "objectId")]
    pub(crate) object_id: String,
}

impl Command for DescribeObject {
    const METHOD: &'static str = "DOM.describeNode";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Described;
}

/// Shows the target's tab. A hidden tab neither paints nor answers input events promptly.
#[derive(Serialize, Debug)]
pub(crate) struct BringToFront {}

impl Command for BringToFront {
    const METHOD: &'static str = "Page.bringToFront";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize, Debug)]
pub(crate) struct ScrollIntoView {
    #[serde(rename = "objectId")]
    pub(crate) object_id: String,
}

impl Command for ScrollIntoView {
    const METHOD: &'static str = "DOM.scrollIntoViewIfNeeded";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize, Debug)]
pub(crate) struct GetContentQuads {
    #[serde(rename = "objectId")]
    pub(crate) object_id: String,
}

#[derive(Deserialize, Debug)]
pub(crate) struct ContentQuads {
    pub(crate) quads: Vec<Vec<f64>>,
}

impl Command for GetContentQuads {
    const METHOD: &'static str = "DOM.getContentQuads";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = ContentQuads;
}

#[derive(Serialize, Debug)]
pub(crate) struct GetFrameOwner {
    #[serde(rename = "frameId")]
    pub(crate) frame_id: FrameId,
}

#[derive(Deserialize, Debug)]
pub(crate) struct FrameOwner {
    #[serde(rename = "backendNodeId")]
    pub(crate) backend_node_id: i64,
}

impl Command for GetFrameOwner {
    const METHOD: &'static str = "DOM.getFrameOwner";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = FrameOwner;
}

#[derive(Serialize, Debug)]
pub(crate) struct GetBoxModel {
    #[serde(rename = "backendNodeId")]
    pub(crate) backend_node_id: i64,
}

#[derive(Deserialize, Debug)]
pub(crate) struct BoxModelReply {
    pub(crate) model: BoxModel,
}

#[derive(Deserialize, Debug)]
pub(crate) struct BoxModel {
    pub(crate) content: Vec<f64>,
}

impl Command for GetBoxModel {
    const METHOD: &'static str = "DOM.getBoxModel";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = BoxModelReply;
}

#[derive(Serialize, Debug)]
pub(crate) struct DispatchMouse {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) button: &'static str,
    pub(crate) buttons: u32,
    #[serde(rename = "clickCount")]
    pub(crate) click_count: u32,
}

impl Command for DispatchMouse {
    const METHOD: &'static str = "Input.dispatchMouseEvent";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize, Debug)]
pub(crate) struct DispatchWheel {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) x: f64,
    pub(crate) y: f64,
    #[serde(rename = "deltaX")]
    pub(crate) delta_x: f64,
    #[serde(rename = "deltaY")]
    pub(crate) delta_y: f64,
}

impl Command for DispatchWheel {
    const METHOD: &'static str = "Input.dispatchMouseEvent";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize, Debug)]
pub(crate) struct DispatchKey {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) modifiers: u32,
    pub(crate) key: String,
    pub(crate) code: String,
    #[serde(rename = "windowsVirtualKeyCode")]
    pub(crate) windows_virtual_key_code: u32,
    pub(crate) text: String,
    #[serde(rename = "unmodifiedText")]
    pub(crate) unmodified_text: String,
}

impl Command for DispatchKey {
    const METHOD: &'static str = "Input.dispatchKeyEvent";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize, Debug)]
pub(crate) struct InsertText {
    pub(crate) text: String,
}

impl Command for InsertText {
    const METHOD: &'static str = "Input.insertText";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize, Debug)]
pub(crate) struct HandleDialog {
    pub(crate) accept: bool,
    #[serde(rename = "promptText")]
    pub(crate) prompt_text: String,
}

impl Command for HandleDialog {
    const METHOD: &'static str = "Page.handleJavaScriptDialog";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize, Debug)]
pub(crate) struct Navigate {
    pub(crate) url: String,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Navigated {
    #[serde(rename = "frameId")]
    pub(crate) frame_id: FrameId,
    #[serde(rename = "loaderId")]
    pub(crate) loader_id: Option<String>,
    #[serde(rename = "errorText")]
    pub(crate) error_text: Option<String>,
}

impl Command for Navigate {
    const METHOD: &'static str = "Page.navigate";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Navigated;
}

#[derive(Serialize, Debug)]
pub(crate) struct CaptureScreenshot {
    pub(crate) format: &'static str,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Screenshot {
    pub(crate) data: String,
}

impl Command for CaptureScreenshot {
    const METHOD: &'static str = "Page.captureScreenshot";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Screenshot;
}

#[derive(Deserialize, Debug)]
pub(crate) struct DialogOpening {
    pub(crate) message: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(rename = "defaultPrompt")]
    pub(crate) default_prompt: Option<String>,
}

impl CdpEvent for DialogOpening {
    const METHOD: &'static str = "Page.javascriptDialogOpening";
}

#[derive(Deserialize, Debug)]
pub(crate) struct DialogClosed {}

impl CdpEvent for DialogClosed {
    const METHOD: &'static str = "Page.javascriptDialogClosed";
}

#[derive(Deserialize, Debug)]
pub(crate) struct LifecycleEvent {
    #[serde(rename = "frameId")]
    pub(crate) frame_id: FrameId,
    #[serde(rename = "loaderId")]
    pub(crate) loader_id: String,
    pub(crate) name: String,
}

impl CdpEvent for LifecycleEvent {
    const METHOD: &'static str = "Page.lifecycleEvent";
}
