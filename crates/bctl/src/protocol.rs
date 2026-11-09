use miniserde::{Deserialize, Serialize, json};

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
    /// For `tab-new`: where the new target opens; absent opens `about:blank`.
    pub url: Option<String>,
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
}

impl Op {
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
    ImageRequired,
    ImageMissing,
    RuntimeMissing,
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
    HumanInControl,
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
            ErrorCode::ImageRequired => "IMAGE_REQUIRED",
            ErrorCode::ImageMissing => "IMAGE_MISSING",
            ErrorCode::RuntimeMissing => "RUNTIME_MISSING",
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
            ErrorCode::HumanInControl => "HUMAN_IN_CONTROL",
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
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>, remedy: Option<String>) -> ErrorBody {
        ErrorBody { code: code.as_str().to_string(), message: message.into(), remedy, target: None, targets: None }
    }
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
    /// `interactive` or `view-only`, once the guest has reported it.
    pub input: Option<String>,
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
}

impl Response {
    pub fn empty() -> Response {
        Default::default()
    }

    pub fn failure(code: ErrorCode, message: impl Into<String>, remedy: Option<String>) -> Response {
        Response { error: Some(Box::new(ErrorBody::new(code, message, remedy))), ..Default::default() }
    }
}
