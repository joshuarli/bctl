//! Typed CDP commands and events. Other crates add their own by implementing
//! [`Command`] and [`CdpEvent`].

use std::borrow::Cow;
use std::fmt;

use miniserde::{Deserialize, Serialize, de, make_place, ser};

/// Whether a command can change browser, page, or session state. A dispatched
/// [`Effect::Mutating`] command without an observed reply fails as
/// [`crate::CallError::UnknownOutcome`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    ReadOnly,
    Mutating,
}

/// A CDP method whose value is its `params` object.
pub trait Command: Serialize {
    const METHOD: &'static str;
    const EFFECT: Effect;
    type Returns: Deserialize;
}

/// A CDP event whose value is its `params` object.
pub trait CdpEvent: Deserialize {
    const METHOD: &'static str;
}

make_place!(Place);

macro_rules! cdp_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(id: impl Into<String>) -> $name {
                $name(id.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn begin(&self) -> ser::Fragment<'_> {
                ser::Fragment::Str(Cow::Borrowed(&self.0))
            }
        }

        impl Deserialize for $name {
            fn begin(out: &mut Option<Self>) -> &mut dyn de::Visitor {
                Place::new(out)
            }
        }

        impl de::Visitor for Place<$name> {
            fn string(&mut self, s: &str) -> miniserde::Result<()> {
                self.out = Some($name(s.to_owned()));
                Ok(())
            }
        }
    };
}

cdp_id!(SessionId);
cdp_id!(TargetId);
cdp_id!(FrameId);

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Empty {}

#[derive(Serialize, Debug, Default)]
pub struct GetVersion {}

#[derive(Deserialize, Debug, Clone)]
pub struct Version {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub product: String,
    pub revision: String,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
    #[serde(rename = "jsVersion")]
    pub js_version: String,
}

impl Command for GetVersion {
    const METHOD: &'static str = "Browser.getVersion";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Version;
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    #[serde(rename = "targetId")]
    pub target_id: TargetId,
    /// `page`, `iframe`, `worker`, `browser`, and others; the set is open.
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub url: String,
    pub attached: bool,
    #[serde(rename = "openerId")]
    pub opener_id: Option<TargetId>,
    #[serde(rename = "browserContextId")]
    pub browser_context_id: Option<String>,
}

#[derive(Serialize, Debug, Default)]
pub struct GetTargets {}

#[derive(Deserialize, Debug, Clone)]
pub struct Targets {
    #[serde(rename = "targetInfos")]
    pub target_infos: Vec<TargetInfo>,
}

impl Command for GetTargets {
    const METHOD: &'static str = "Target.getTargets";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Targets;
}

#[derive(Serialize, Debug)]
pub struct SetDiscoverTargets {
    pub discover: bool,
}

impl Command for SetDiscoverTargets {
    const METHOD: &'static str = "Target.setDiscoverTargets";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

/// Attaches in flattened mode: the target's commands and events travel on
/// this connection under the returned [`SessionId`].
#[derive(Serialize, Debug)]
pub struct AttachToTarget {
    #[serde(rename = "targetId")]
    target_id: TargetId,
    flatten: bool,
}

impl AttachToTarget {
    pub fn new(target_id: TargetId) -> AttachToTarget {
        AttachToTarget { target_id, flatten: true }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct Attached {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}

impl Command for AttachToTarget {
    const METHOD: &'static str = "Target.attachToTarget";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Attached;
}

#[derive(Serialize, Debug)]
pub struct DetachFromTarget {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}

impl Command for DetachFromTarget {
    const METHOD: &'static str = "Target.detachFromTarget";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Deserialize, Debug, Clone)]
pub struct AttachedToTarget {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[serde(rename = "targetInfo")]
    pub target_info: TargetInfo,
    #[serde(rename = "waitingForDebugger")]
    pub waiting_for_debugger: bool,
}

impl CdpEvent for AttachedToTarget {
    const METHOD: &'static str = "Target.attachedToTarget";
}

#[derive(Deserialize, Debug, Clone)]
pub struct DetachedFromTarget {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[serde(rename = "targetId")]
    pub target_id: Option<TargetId>,
}

impl CdpEvent for DetachedFromTarget {
    const METHOD: &'static str = "Target.detachedFromTarget";
}

#[derive(Deserialize, Debug, Clone)]
pub struct TargetCreated {
    #[serde(rename = "targetInfo")]
    pub target_info: TargetInfo,
}

impl CdpEvent for TargetCreated {
    const METHOD: &'static str = "Target.targetCreated";
}

#[derive(Deserialize, Debug, Clone)]
pub struct TargetDestroyed {
    #[serde(rename = "targetId")]
    pub target_id: TargetId,
}

impl CdpEvent for TargetDestroyed {
    const METHOD: &'static str = "Target.targetDestroyed";
}

#[derive(Deserialize, Debug, Clone)]
pub struct TargetInfoChanged {
    #[serde(rename = "targetInfo")]
    pub target_info: TargetInfo,
}

impl CdpEvent for TargetInfoChanged {
    const METHOD: &'static str = "Target.targetInfoChanged";
}

#[derive(Deserialize, Debug, Clone)]
pub struct Frame {
    pub id: FrameId,
    #[serde(rename = "parentId")]
    pub parent_id: Option<FrameId>,
    #[serde(rename = "loaderId")]
    pub loader_id: String,
    pub url: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct FrameNavigated {
    pub frame: Frame,
}

impl CdpEvent for FrameNavigated {
    const METHOD: &'static str = "Page.frameNavigated";
}

#[derive(Deserialize, Debug, Clone)]
pub struct FrameAttached {
    #[serde(rename = "frameId")]
    pub frame_id: FrameId,
    #[serde(rename = "parentFrameId")]
    pub parent_frame_id: FrameId,
}

impl CdpEvent for FrameAttached {
    const METHOD: &'static str = "Page.frameAttached";
}

#[derive(Deserialize, Debug, Clone)]
pub struct FrameDetached {
    #[serde(rename = "frameId")]
    pub frame_id: FrameId,
}

impl CdpEvent for FrameDetached {
    const METHOD: &'static str = "Page.frameDetached";
}
