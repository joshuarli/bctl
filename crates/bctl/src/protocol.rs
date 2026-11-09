use miniserde::{Deserialize, Serialize};

/// One request per connection line. `op` is one of the `Op` names; the other fields depend on it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Request {
    pub op: String,
    pub session: Option<String>,
    pub profile: Option<String>,
    pub image: Option<String>,
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
        }
    }

    pub fn parse(s: &str) -> Option<Op> {
        [Op::Ping, Op::Up, Op::Down, Op::Rm, Op::Status, Op::Inspect, Op::Profiles, Op::Serve].into_iter().find(|op| op.as_str() == s)
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
            ErrorCode::Internal => "INTERNAL",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub remedy: Option<String>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>, remedy: Option<String>) -> ErrorBody {
        ErrorBody { code: code.as_str().to_string(), message: message.into(), remedy }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub name: String,
    pub profile: String,
    pub image: String,
    pub state: String,
    pub generation: u64,
    /// While starting, the readiness stage not yet reached.
    pub stage: Option<String>,
    pub failure: Option<String>,
    pub machine: Option<String>,
    pub cdp_url: Option<String>,
    pub vnc_port: Option<u16>,
    /// `interactive` or `view-only`, once the guest has reported it.
    pub input: Option<String>,
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
    /// For `serve`: a single-use dashboard login URL.
    pub url: Option<String>,
}

impl Response {
    pub fn empty() -> Response {
        Default::default()
    }

    pub fn failure(code: ErrorCode, message: impl Into<String>, remedy: Option<String>) -> Response {
        Response { error: Some(Box::new(ErrorBody::new(code, message, remedy))), ..Default::default() }
    }
}
