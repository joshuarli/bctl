use std::borrow::Cow;

use miniserde::{Deserialize, Serialize, json, ser};

/// One request per connection line. `op` is one of the `Op` names; the other fields depend on it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Request {
    pub op: String,
    pub session: Option<String>,
    pub profile: Option<String>,
    pub image: Option<String>,
    /// For `up`: private IPv4 ranges the guest may reach; empty keeps an existing session's.
    pub allow_private: Vec<String>,
    pub run: Option<i64>,
    pub job: Option<Box<JobRequest>>,
    /// For `tab-use` and `tab-close`: a CDP target id from `tab-list`.
    pub target: Option<String>,
    /// For `tab-new`: where the new target opens; absent opens `about:blank`. For `cdp-revoke`: the one URL to revoke.
    pub url: Option<String>,
    /// For `page-snapshot`; absent takes the default snapshot.
    pub snapshot: Option<Box<SnapshotRequest>>,
    /// For `page-resolve`: an element ref from `page-snapshot`, as `e12` or `@e12`.
    pub element: Option<String>,
    /// For the `page-*` actions (`page-click` and the rest).
    pub action: Option<Box<ActionRequest>>,
    /// For `page-batch`: the steps, each an object whose `action` names a page action (`click`, `fill`, ...)
    /// and whose other fields are that action's [`ActionRequest`] fields.
    pub batch: Option<Vec<json::Value>>,
}

/// A page action's element and arguments, as the CLI read them. Which fields apply depends on the op; the
/// daemon refuses a request that names more than one locator or a field the op does not take.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionRequest {
    /// An element ref, `e12` or `@e12`.
    pub element: Option<String>,
    pub css: Option<String>,
    /// A browser-computed role, optionally with `name`, its exact accessible name.
    pub role: Option<String>,
    pub name: Option<String>,
    pub label: Option<String>,
    pub placeholder: Option<String>,
    pub testid: Option<String>,
    /// Which match of a query, from 1.
    pub nth: Option<u32>,
    /// For `page-goto`: where to go. For `page-wait`: a URL pattern, `*` matching any run of characters.
    pub url: Option<String>,
    /// For `page-fill` and `page-type`: what to enter. For `page-wait`: text to appear. For `page-dialog
    /// accept`: a prompt's answer.
    pub text: Option<String>,
    /// For `page-press`: a key such as `Enter` or `Control+a`.
    pub key: Option<String>,
    /// For `page-select`: option values or labels.
    pub values: Vec<String>,
    /// For `page-get`: `text`, `title`, `url`, `attribute`, or `value`.
    pub read: Option<String>,
    /// For `page-get attribute`: the attribute's name.
    pub attribute: Option<String>,
    /// For `page-goto`: `commit`, `domcontentloaded`, `load` (the default), or `networkidle`. For `page-wait`:
    /// `domcontentloaded` or `load`.
    pub load: Option<String>,
    /// For `page-wait` with a locator: wait until nothing matches instead.
    pub gone: bool,
    /// How long the action may wait for its element, its page to load, or its condition. Screenshot, eval, and
    /// dialog take none.
    pub timeout_ms: Option<u64>,
    /// For `page-scroll` without a locator: wheel distances in CSS pixels.
    pub dx: Option<f64>,
    pub dy: Option<f64>,
    /// For `page-eval`.
    pub expression: Option<String>,
    /// For `page-dialog`: `accept` or `dismiss`.
    pub answer: Option<String>,
    /// For `page-screenshot`: the absolute path the PNG is written to.
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotRequest {
    /// Every node with content, not only interactive ones.
    pub full: bool,
    /// Rendered levels to keep, from 1.
    pub depth: Option<u32>,
    /// A CSS selector in the main frame; only its first match's subtree is shown.
    pub scope: Option<String>,
    /// Return the node tree instead of text.
    pub nodes: bool,
}

/// A job for `run`, resolved by the CLI: exactly one of `exec` and `grab`. `browser` is absent for a one-off
/// `bctl grab`, which runs on the session as it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub name: Option<String>,
    pub browser: Option<BrowserRequest>,
    pub exec: Option<ExecRequest>,
    pub grab: Option<GrabRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserRequest {
    pub profile: String,
    pub image: String,
    pub resources: ResourcesView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    /// Absolute: the CLI joins a job's relative `cwd` to the directory holding `bctl.toml`.
    pub cwd: String,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrabRequest {
    pub urls: Vec<String>,
    pub max_pages: Option<u32>,
    pub jobs: Option<u16>,
    pub robots: bool,
    pub max_runtime_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourcesView {
    pub width: u32,
    pub height: u32,
    pub cpus: u16,
    pub memory_mib: u32,
}

impl Request {
    pub fn empty() -> Request {
        Default::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Ping,
    Up,
    Down,
    Rm,
    Status,
    Inspect,
    Profiles,
    Serve,
    CdpUrl,
    CdpRevoke,
    Run,
    Runs,
    Logs,
    Pause,
    Resume,
    Cancel,
    TabList,
    TabNew,
    TabUse,
    TabClose,
    PageSnapshot,
    PageResolve,
    PageGoto,
    PageClick,
    PageDblclick,
    PageFill,
    PageType,
    PagePress,
    PageHover,
    PageScroll,
    PageSelect,
    PageCheck,
    PageUncheck,
    PageWait,
    PageGet,
    PageScreenshot,
    PageEval,
    PageDialog,
    PageBatch,
}

impl Op {
    /// The ops [`ActionRequest`] describes, which a batch may run.
    pub const PAGE_ACTIONS: [Op; 16] = [
        Op::PageGoto,
        Op::PageClick,
        Op::PageDblclick,
        Op::PageFill,
        Op::PageType,
        Op::PagePress,
        Op::PageHover,
        Op::PageScroll,
        Op::PageSelect,
        Op::PageCheck,
        Op::PageUncheck,
        Op::PageWait,
        Op::PageGet,
        Op::PageScreenshot,
        Op::PageEval,
        Op::PageDialog,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Op::Ping => "ping",
            Op::Up => "up",
            Op::Down => "down",
            Op::Rm => "rm",
            Op::Status => "status",
            Op::Inspect => "inspect",
            Op::Profiles => "profiles",
            Op::Serve => "serve",
            Op::CdpUrl => "cdp-url",
            Op::CdpRevoke => "cdp-revoke",
            Op::Run => "run",
            Op::Runs => "runs",
            Op::Logs => "logs",
            Op::Pause => "pause",
            Op::Resume => "resume",
            Op::Cancel => "cancel",
            Op::TabList => "tab-list",
            Op::TabNew => "tab-new",
            Op::TabUse => "tab-use",
            Op::TabClose => "tab-close",
            Op::PageSnapshot => "page-snapshot",
            Op::PageResolve => "page-resolve",
            Op::PageGoto => "page-goto",
            Op::PageClick => "page-click",
            Op::PageDblclick => "page-dblclick",
            Op::PageFill => "page-fill",
            Op::PageType => "page-type",
            Op::PagePress => "page-press",
            Op::PageHover => "page-hover",
            Op::PageScroll => "page-scroll",
            Op::PageSelect => "page-select",
            Op::PageCheck => "page-check",
            Op::PageUncheck => "page-uncheck",
            Op::PageWait => "page-wait",
            Op::PageGet => "page-get",
            Op::PageScreenshot => "page-screenshot",
            Op::PageEval => "page-eval",
            Op::PageDialog => "page-dialog",
            Op::PageBatch => "page-batch",
        }
    }

    pub fn parse(s: &str) -> Option<Op> {
        [
            Op::Ping,
            Op::Up,
            Op::Down,
            Op::Rm,
            Op::Status,
            Op::Inspect,
            Op::Profiles,
            Op::Serve,
            Op::CdpUrl,
            Op::CdpRevoke,
            Op::Run,
            Op::Runs,
            Op::Logs,
            Op::Pause,
            Op::Resume,
            Op::Cancel,
            Op::TabList,
            Op::TabNew,
            Op::TabUse,
            Op::TabClose,
            Op::PageSnapshot,
            Op::PageResolve,
            Op::PageGoto,
            Op::PageClick,
            Op::PageDblclick,
            Op::PageFill,
            Op::PageType,
            Op::PagePress,
            Op::PageHover,
            Op::PageScroll,
            Op::PageSelect,
            Op::PageCheck,
            Op::PageUncheck,
            Op::PageWait,
            Op::PageGet,
            Op::PageScreenshot,
            Op::PageEval,
            Op::PageDialog,
            Op::PageBatch,
        ]
        .into_iter()
        .find(|op| op.as_str() == s)
    }
}

/// Stable error codes. Scripts match on these; messages may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    BadRequest,
    NoSuchSession,
    Conflict,
    ProfileBusy,
    NotStopped,
    ImageMissing,
    RuntimeMissing,
    UnsupportedByRuntime,
    StartFailed,
    RuntimeError,
    DaemonUnreachable,
    NoSuchRun,
    RunActive,
    NotPausable,
    NotResumable,
    NotLive,
    SessionNotReady,
    TargetGone,
    NoTargetSelected,
    NoSuchTarget,
    LastTarget,
    StaleRef,
    ScopeNotFound,
    HumanInControl,
    TakenOver,
    Ambiguous,
    NotFound,
    NotVisible,
    Disabled,
    NotStable,
    Covered,
    Missed,
    Detached,
    NotActionable,
    DialogOpen,
    NoDialog,
    Timeout,
    NavigationFailed,
    EvalFailed,
    WriteFailed,
    UnknownOutcome,
    CdpFailed,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "BAD_REQUEST",
            ErrorCode::NoSuchSession => "NO_SUCH_SESSION",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::ProfileBusy => "PROFILE_BUSY",
            ErrorCode::NotStopped => "NOT_STOPPED",
            ErrorCode::ImageMissing => "IMAGE_MISSING",
            ErrorCode::RuntimeMissing => "RUNTIME_MISSING",
            ErrorCode::UnsupportedByRuntime => "UNSUPPORTED_BY_RUNTIME",
            ErrorCode::StartFailed => "START_FAILED",
            ErrorCode::RuntimeError => "RUNTIME_ERROR",
            ErrorCode::DaemonUnreachable => "DAEMON_UNREACHABLE",
            ErrorCode::NoSuchRun => "NO_SUCH_RUN",
            ErrorCode::RunActive => "RUN_ACTIVE",
            ErrorCode::NotPausable => "NOT_PAUSABLE",
            ErrorCode::NotResumable => "NOT_RESUMABLE",
            ErrorCode::NotLive => "NOT_LIVE",
            ErrorCode::SessionNotReady => "SESSION_NOT_READY",
            ErrorCode::TargetGone => "TARGET_GONE",
            ErrorCode::NoTargetSelected => "NO_TARGET_SELECTED",
            ErrorCode::NoSuchTarget => "NO_SUCH_TARGET",
            ErrorCode::LastTarget => "LAST_TARGET",
            ErrorCode::StaleRef => "STALE_REF",
            ErrorCode::ScopeNotFound => "SCOPE_NOT_FOUND",
            ErrorCode::HumanInControl => "HUMAN_IN_CONTROL",
            ErrorCode::TakenOver => "TAKEN_OVER",
            ErrorCode::Ambiguous => "AMBIGUOUS",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::NotVisible => "NOT_VISIBLE",
            ErrorCode::Disabled => "DISABLED",
            ErrorCode::NotStable => "NOT_STABLE",
            ErrorCode::Covered => "COVERED",
            ErrorCode::Missed => "MISSED",
            ErrorCode::Detached => "DETACHED",
            ErrorCode::NotActionable => "NOT_ACTIONABLE",
            ErrorCode::DialogOpen => "DIALOG_OPEN",
            ErrorCode::NoDialog => "NO_DIALOG",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::NavigationFailed => "NAVIGATION_FAILED",
            ErrorCode::EvalFailed => "EVAL_FAILED",
            ErrorCode::WriteFailed => "WRITE_FAILED",
            ErrorCode::UnknownOutcome => "UNKNOWN_OUTCOME",
            ErrorCode::CdpFailed => "CDP_FAILED",
            ErrorCode::Internal => "INTERNAL",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub remedy: Option<String>,
    /// For target errors: the CDP target id the request named or the session had selected.
    pub target: Option<String>,
    /// For target errors: every page target that exists, so a caller can choose one without another request.
    pub targets: Option<Vec<TabView>>,
    /// For `AMBIGUOUS`: the first matches, each with a fresh ref that names it.
    pub candidates: Option<Vec<CandidateView>>,
    /// For `DIALOG_OPEN`: the dialog the page waits on.
    pub dialog: Option<DialogView>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>, remedy: Option<String>) -> ErrorBody {
        ErrorBody { code: code.as_str().to_string(), message: message.into(), remedy, target: None, targets: None, candidates: None, dialog: None }
    }
}

/// One element an ambiguous locator matched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateView {
    #[serde(rename = "ref")]
    pub element: String,
    pub frame: String,
    /// The element's role and name, or its tag, id, and leading text.
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogView {
    /// `alert`, `confirm`, `prompt`, or `beforeunload`.
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    pub default_prompt: String,
}

/// One page target. `url` has no userinfo, query, or fragment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabView {
    pub id: String,
    pub title: String,
    pub url: String,
}

/// For `tab-*`: the session's page targets after the operation and the one native commands act on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabsView {
    pub selected: Option<String>,
    pub targets: Vec<TabView>,
}

/// For `page-*`: what the operation returned about the selected target.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageView {
    pub snapshot: Option<SnapshotView>,
    pub element: Option<ElementView>,
    pub action: Option<ActionView>,
    pub batch: Option<BatchView>,
}

/// For `page-batch`: the steps that ran, in order. A batch stops at its first failing step, so `ran` ends with
/// that step when one failed; steps after it never ran. Nothing a step did is undone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchView {
    /// The target every step acted on: the one selected when the batch began.
    pub target: String,
    /// How many steps the batch asked for.
    pub steps: u32,
    pub ran: Vec<StepView>,
    pub elapsed_ms: u64,
}

/// One step of a batch: its result, or the error that stopped the batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepView {
    pub action: String,
    pub result: Option<ActionView>,
    pub error: Option<ErrorBody>,
    pub elapsed_ms: u64,
}

/// For the `page-*` actions: what was done, where, and what it cost. Fields an action does not produce are null.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionView {
    /// The op without its `page-` prefix: `click`, `fill`, and so on.
    pub action: String,
    pub target: String,
    /// The frame of the element acted on.
    pub frame: Option<String>,
    /// The element's ref, when the request named it by ref.
    #[serde(rename = "ref")]
    pub element: Option<String>,
    /// For `check` and `uncheck`: false when the element was already in that state.
    pub changed: Option<bool>,
    /// For `select`: the values now selected.
    pub selected: Option<Vec<String>>,
    /// For `get text`, `title`, `url`, and `value`.
    pub text: Option<String>,
    /// For `get text`: the text was cut at its bound.
    pub truncated: Option<bool>,
    /// For `get value`: a password field's value is never read.
    pub redacted: Option<bool>,
    /// For `get attribute`: the attribute's value, or null when it is absent. For `eval`: the JSON result.
    pub value: Option<json::Value>,
    /// For `wait` with a locator: elements matching when the wait ended.
    pub matched: Option<u64>,
    /// For `screenshot`: where the PNG was written and its size.
    pub path: Option<String>,
    pub bytes: Option<u64>,
    /// For `dialog`: the dialog answered, if bctl saw it open.
    pub dialog: Option<DialogView>,
    /// CDP commands the action sent, not counting the check that the selected target exists.
    pub cdp_calls: u32,
    /// Time inside the action itself.
    pub action_ms: u64,
    /// Time the daemon spent on the whole request.
    pub elapsed_ms: u64,
}

/// For `page-snapshot`: the selected target's snapshot, as `text` or, if asked, as `nodes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotView {
    pub target: String,
    pub text: Option<String>,
    pub nodes: Option<Vec<NodeView>>,
    /// Nodes left out to keep the snapshot within its size bound.
    pub omitted: u64,
    /// Documents read: the main frame's and each iframe's.
    pub documents: u32,
    pub cdp_calls: u32,
    /// The size of the text form, whichever form was returned.
    pub text_bytes: u64,
}

/// One snapshot node. Absent fields are false or empty; `value` is absent and `redacted` true for a password.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeView {
    #[serde(rename = "ref")]
    pub element: Option<String>,
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub redacted: Option<bool>,
    pub disabled: Option<bool>,
    /// `true`, `false`, or `mixed`.
    pub checked: Option<String>,
    pub selected: Option<bool>,
    pub expanded: Option<bool>,
    pub level: Option<u32>,
    pub children: Option<Vec<NodeView>>,
}

impl Serialize for NodeView {
    fn begin(&self) -> ser::Fragment<'_> {
        ser::Fragment::Map(Box::new(NodeFields { node: self, next: 0 }))
    }
}

struct NodeFields<'a> {
    node: &'a NodeView,
    next: usize,
}

impl ser::Map for NodeFields<'_> {
    fn next(&mut self) -> Option<(Cow<'_, str>, &dyn Serialize)> {
        let n = self.node;
        loop {
            self.next += 1;
            let field: Option<(&'static str, &dyn Serialize)> = match self.next {
                1 => n.element.as_ref().map(|v| ("ref", v as &dyn Serialize)),
                2 => Some(("role", &n.role)),
                3 => n.name.as_ref().map(|v| ("name", v as &dyn Serialize)),
                4 => n.value.as_ref().map(|v| ("value", v as &dyn Serialize)),
                5 => n.redacted.as_ref().map(|v| ("redacted", v as &dyn Serialize)),
                6 => n.disabled.as_ref().map(|v| ("disabled", v as &dyn Serialize)),
                7 => n.checked.as_ref().map(|v| ("checked", v as &dyn Serialize)),
                8 => n.selected.as_ref().map(|v| ("selected", v as &dyn Serialize)),
                9 => n.expanded.as_ref().map(|v| ("expanded", v as &dyn Serialize)),
                10 => n.level.as_ref().map(|v| ("level", v as &dyn Serialize)),
                11 => n.children.as_ref().map(|v| ("children", v as &dyn Serialize)),
                _ => return None,
            };
            if let Some((name, value)) = field {
                return Some((Cow::Borrowed(name), value));
            }
        }
    }
}

/// For `page-resolve`: the live node a ref names, with its role and name as the browser computes them now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementView {
    #[serde(rename = "ref")]
    pub element: String,
    pub target: String,
    pub frame: String,
    pub role: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub name: String,
    pub profile: String,
    pub image: String,
    pub resources: ResourcesView,
    pub allow_private: Vec<String>,
    pub state: String,
    pub generation: u64,
    /// While starting, the readiness stage not yet reached.
    pub stage: Option<String>,
    pub failure: Option<String>,
    pub machine: Option<String>,
    pub vnc_port: Option<u16>,
    /// `interactive`, `view-only` or `headless`, once the guest has reported it.
    pub input: Option<String>,
    /// `headed` or `headless`, once the guest has reported its input.
    pub display: Option<String>,
    /// Whether `bctl pause` can checkpoint the session's VM, while it holds one.
    pub pausable: Option<bool>,
    /// The host directory the guest sees at `guest_files_dir`, kept until `bctl rm`.
    pub files_dir: String,
    /// Where the guest sees `files_dir`, which is also where the browser saves downloads.
    pub guest_files_dir: String,
}

/// One run and its attempts. Every resolved input is here, so `bctl runs ID --json` is the run's full record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunView {
    pub id: i64,
    pub session: String,
    /// The `bctl.toml` job, or null for a one-off `bctl grab`.
    pub job: Option<String>,
    pub kind: String,
    /// The current attempt's state, which is the run's state.
    pub state: String,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub profile: String,
    pub image: String,
    pub image_digest: String,
    pub browser_version: String,
    pub config: json::Value,
    pub input_digest: String,
    pub output_dir: String,
    pub log: String,
    pub created_ms: i64,
    pub progress: Option<ProgressView>,
    pub interaction: Option<InteractionView>,
    pub attempts: Vec<AttemptView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptView {
    pub number: u32,
    pub state: String,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub pid: Option<u32>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ProgressView {
    pub pages_done: u32,
    pub pages_queued: u32,
    pub assets_done: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionView {
    pub reason: String,
    pub url: String,
}

/// For `logs`: a run's log, bounded to the run's cap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogView {
    pub path: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileView {
    pub name: String,
    pub holder: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionsPayload {
    pub sessions: Vec<SessionView>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub error: Option<Box<ErrorBody>>,
    pub sessions: Vec<SessionView>,
    pub profiles: Vec<ProfileView>,
    pub runs: Vec<RunView>,
    /// For `serve`: a single-use dashboard login URL. For `cdp-url`: a new CDP control URL for the session.
    pub url: Option<String>,
    pub log: Option<Box<LogView>>,
    pub tabs: Option<Box<TabsView>>,
    pub page: Option<Box<PageView>>,
}

impl Response {
    pub fn empty() -> Response {
        Default::default()
    }

    pub fn failure(code: ErrorCode, message: impl Into<String>, remedy: Option<String>) -> Response {
        Response { error: Some(Box::new(ErrorBody::new(code, message, remedy))), ..Default::default() }
    }
}
