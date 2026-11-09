use crate::names::{Generation, ProfileName, SessionName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Stopped,
    Starting,
    Ready,
    Stopping,
    Failed,
}

impl SessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Stopped => "stopped",
            SessionState::Starting => "starting",
            SessionState::Ready => "ready",
            SessionState::Stopping => "stopping",
            SessionState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<SessionState> {
        [SessionState::Stopped, SessionState::Starting, SessionState::Ready, SessionState::Stopping, SessionState::Failed]
            .into_iter()
            .find(|state| state.as_str() == s)
    }

    /// Whether a VM for this session may exist, so its profile is held and the runtime must be consulted.
    pub fn holds_machine(self) -> bool {
        matches!(self, SessionState::Starting | SessionState::Ready | SessionState::Stopping)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptState {
    Starting,
    Running,
    NeedsInteraction,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl AttemptState {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptState::Starting => "starting",
            AttemptState::Running => "running",
            AttemptState::NeedsInteraction => "needs-interaction",
            AttemptState::Paused => "paused",
            AttemptState::Succeeded => "succeeded",
            AttemptState::Failed => "failed",
            AttemptState::Cancelled => "cancelled",
            AttemptState::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<AttemptState> {
        [
            AttemptState::Starting,
            AttemptState::Running,
            AttemptState::NeedsInteraction,
            AttemptState::Paused,
            AttemptState::Succeeded,
            AttemptState::Failed,
            AttemptState::Cancelled,
            AttemptState::Interrupted,
        ]
        .into_iter()
        .find(|state| state.as_str() == s)
    }

    /// An attempt that was live when its owner vanished; its external effects are unknown.
    pub fn is_live(self) -> bool {
        matches!(self, AttemptState::Starting | AttemptState::Running | AttemptState::NeedsInteraction)
    }
}

/// The states a live attempt moves between before it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    Starting,
    Running,
    NeedsInteraction,
}

impl From<LiveState> for AttemptState {
    fn from(state: LiveState) -> AttemptState {
        match state {
            LiveState::Starting => AttemptState::Starting,
            LiveState::Running => AttemptState::Running,
            LiveState::NeedsInteraction => AttemptState::NeedsInteraction,
        }
    }
}

/// How an attempt ended. These names are stable: scripts match on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    BrowserCrashed,
    SiteRejected,
    LoginRequired,
    VerificationFailed,
    JobError,
    Cancelled,
    Interrupted,
}

impl Outcome {
    const ALL: [Outcome; 8] = [
        Outcome::Succeeded,
        Outcome::BrowserCrashed,
        Outcome::SiteRejected,
        Outcome::LoginRequired,
        Outcome::VerificationFailed,
        Outcome::JobError,
        Outcome::Cancelled,
        Outcome::Interrupted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Succeeded => "succeeded",
            Outcome::BrowserCrashed => "browser-crashed",
            Outcome::SiteRejected => "site-rejected",
            Outcome::LoginRequired => "login-required",
            Outcome::VerificationFailed => "verification-failed",
            Outcome::JobError => "job-error",
            Outcome::Cancelled => "cancelled",
            Outcome::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<Outcome> {
        Outcome::ALL.into_iter().find(|outcome| outcome.as_str() == s)
    }

    pub fn attempt_state(self) -> AttemptState {
        match self {
            Outcome::Succeeded => AttemptState::Succeeded,
            Outcome::BrowserCrashed | Outcome::SiteRejected | Outcome::LoginRequired | Outcome::VerificationFailed | Outcome::JobError => {
                AttemptState::Failed
            }
            Outcome::Cancelled => AttemptState::Cancelled,
            Outcome::Interrupted => AttemptState::Interrupted,
        }
    }
}

/// How an attempt stopped: paused attempts can be resumed by a new attempt, ended ones carry their outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptEnd {
    Paused { detail: String },
    Ended { outcome: Outcome, detail: Option<String> },
}

/// The machine a session launches: the resources a `[browsers.*]` entry resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    pub viewport: [u32; 2],
    pub cpus: u16,
    pub memory_mib: u32,
}

impl Resources {
    pub const DEFAULT: Resources = Resources { viewport: [1280, 800], cpus: 2, memory_mib: 4096 };
}

/// An IPv4 range in CIDR notation that a session's guest may reach although private egress is blocked.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AllowedRange(String);

impl AllowedRange {
    pub fn parse(text: &str) -> Result<AllowedRange, String> {
        let invalid = || format!("{text:?} is not an IPv4 range like 100.96.0.0/30");
        let (address, prefix_text) = text.split_once('/').ok_or_else(invalid)?;
        address.parse::<std::net::Ipv4Addr>().map_err(|_| invalid())?;
        let prefix: u8 = prefix_text.parse().map_err(|_| invalid())?;
        if prefix > 32 || prefix.to_string() != prefix_text {
            return Err(invalid());
        }
        Ok(AllowedRange(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub name: SessionName,
    pub profile: ProfileName,
    pub image: String,
    pub resources: Resources,
    /// Sorted and without duplicates.
    pub allow_private: Vec<AllowedRange>,
    pub state: SessionState,
    pub generation: Generation,
    pub endpoints: Option<Endpoints>,
    pub failure: Option<String>,
}

/// The host loopback port forwarded to one generation of a session's VNC display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoints {
    pub vnc_port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Exec,
    Grab,
}

impl RunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RunKind::Exec => "exec",
            RunKind::Grab => "grab",
        }
    }

    pub fn parse(s: &str) -> Option<RunKind> {
        [RunKind::Exec, RunKind::Grab].into_iter().find(|kind| kind.as_str() == s)
    }
}

/// Everything a run resolved before its first attempt, recorded once and never changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRun {
    pub session: SessionName,
    /// `None` for a one-off `bctl grab`, which names no `bctl.toml` job.
    pub job: Option<String>,
    pub kind: RunKind,
    pub profile: ProfileName,
    pub image: String,
    pub image_digest: String,
    pub browser_version: String,
    /// The resolved job as JSON.
    pub config: String,
    pub input_digest: String,
    pub created_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub id: i64,
    pub spec: NewRun,
    pub previous: Vec<AttemptRecord>,
    pub current: AttemptRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub pages_done: u32,
    pub pages_queued: u32,
    pub assets_done: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interaction {
    pub reason: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRecord {
    pub id: i64,
    pub run: i64,
    pub number: u32,
    pub state: AttemptState,
    /// Set exactly when `state` is an ended state; it determines that state.
    pub outcome: Option<Outcome>,
    pub detail: Option<String>,
    /// Leader of the attempt's process group while it runs.
    pub pid: Option<u32>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub progress: Option<Progress>,
    /// The operator action the attempt is waiting on, while it is `NeedsInteraction`.
    pub interaction: Option<Interaction>,
}

/// What the runtime reports about one machine it knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineObservation {
    pub name: String,
    pub running: bool,
}

#[cfg(test)]
mod tests {
    use super::AllowedRange;

    #[test]
    fn an_allowed_range_is_one_ipv4_cidr() {
        assert_eq!(AllowedRange::parse("100.96.0.0/30").unwrap().as_str(), "100.96.0.0/30");
        for bad in ["100.96.0.0", "100.96.0.0/33", "100.96.0.0/030", "::1/128", "host/8", "10.0.0.0/8 ", ""] {
            assert!(AllowedRange::parse(bad).is_err(), "{bad:?}");
        }
    }
}
