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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub name: SessionName,
    pub profile: ProfileName,
    pub image: String,
    pub state: SessionState,
    pub generation: Generation,
    pub endpoints: Option<Endpoints>,
    pub failure: Option<String>,
}

/// Host loopback ports forwarded to one generation of a session: CDP and the VNC display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoints {
    pub cdp_port: u16,
    pub vnc_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRecord {
    pub id: i64,
    pub run: i64,
    pub state: AttemptState,
}

/// What the runtime reports about one machine it knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineObservation {
    pub name: String,
    pub running: bool,
}
