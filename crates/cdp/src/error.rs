use std::fmt;

use crate::protocol::SessionId;

/// Why a connection ended. Every pending command and subscription of the
/// connection observes the same value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Termination {
    /// The owner called [`crate::Connection::shutdown`] or dropped every handle.
    Shutdown,
    PeerClosed { code: Option<u16>, reason: String },
    Socket(String),
    /// The peer violated RFC 6455 framing.
    Protocol(String),
    /// An inbound message reached [`crate::MAX_MESSAGE_BYTES`].
    MessageTooLarge,
    /// Nothing arrived within [`crate::PONG_DEADLINE`] of a ping.
    Unresponsive,
    /// The owner dropped the [`crate::Driver`] before it finished.
    DriverDropped,
}

impl fmt::Display for Termination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Termination::Shutdown => f.write_str("the connection was shut down"),
            Termination::PeerClosed { code: Some(code), reason } => write!(f, "the browser closed the connection ({code} {reason})"),
            Termination::PeerClosed { code: None, .. } => f.write_str("the browser closed the connection"),
            Termination::Socket(e) => write!(f, "the CDP socket failed: {e}"),
            Termination::Protocol(e) => write!(f, "the browser broke the websocket protocol: {e}"),
            Termination::MessageTooLarge => write!(f, "the browser sent a message of {} bytes or more", crate::MAX_MESSAGE_BYTES),
            Termination::Unresponsive => f.write_str("the browser stopped answering pings"),
            Termination::DriverDropped => f.write_str("the connection driver was dropped"),
        }
    }
}

/// What stood between a dispatched mutating command and its reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unobserved {
    Deadline,
    Disconnected(Termination),
    SessionDetached,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// A mutating command reached the socket and no reply was observed: it may
    /// or may not have taken effect. The connection never replays it.
    UnknownOutcome { method: &'static str, cause: Unobserved },
    /// The deadline passed for a read-only command, or for any command before
    /// it reached the socket. An undispatched command is never sent later.
    Timeout { method: &'static str },
    /// The connection ended with a read-only or undispatched command pending.
    Disconnected { method: &'static str, termination: Termination },
    /// The command's flattened session detached with a read-only or
    /// undispatched command pending.
    SessionDetached { method: &'static str, session: SessionId },
    /// The connection had already ended or begun shutting down.
    Closed(Termination),
    TooManyPending,
    RequestTooLarge { method: &'static str, bytes: usize },
    /// The browser answered with a CDP error.
    Protocol { method: &'static str, code: i64, message: String },
    /// The reply could not be decoded as the command's result.
    MalformedReply { method: &'static str, reason: String },
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::UnknownOutcome { method, cause } => {
                let cause = match cause {
                    Unobserved::Deadline => "its deadline passed".to_string(),
                    Unobserved::Disconnected(t) => t.to_string(),
                    Unobserved::SessionDetached => "its session detached".to_string(),
                };
                write!(f, "{method} was sent but its outcome is unknown: {cause}")
            }
            CallError::Timeout { method } => write!(f, "{method} timed out"),
            CallError::Disconnected { method, termination } => write!(f, "{method} failed: {termination}"),
            CallError::SessionDetached { method, session } => write!(f, "{method} failed: session {session} detached"),
            CallError::Closed(t) => write!(f, "the CDP connection is closed: {t}"),
            CallError::TooManyPending => write!(f, "{} CDP commands are already pending", crate::MAX_PENDING_COMMANDS),
            CallError::RequestTooLarge { method, bytes } => write!(f, "{method} is {bytes} bytes, over the CDP message limit"),
            CallError::Protocol { method, code, message } => write!(f, "{method} failed: {message} ({code})"),
            CallError::MalformedReply { method, reason } => write!(f, "{method} returned an unexpected reply: {reason}"),
        }
    }
}

impl std::error::Error for CallError {}

#[derive(Debug)]
pub enum ConnectError {
    Dial(std::io::Error),
    Handshake(h12tiny::web::WebSocketConnectError),
    Deadline,
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Dial(e) => write!(f, "could not reach the CDP endpoint: {e}"),
            ConnectError::Handshake(e) => write!(f, "CDP websocket handshake failed: {e}"),
            ConnectError::Deadline => write!(f, "CDP connection not established within {:?}", crate::CONNECT_DEADLINE),
        }
    }
}

impl std::error::Error for ConnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConnectError::Dial(e) => Some(e),
            ConnectError::Handshake(e) => Some(e),
            ConnectError::Deadline => None,
        }
    }
}

/// Why a [`crate::Subscription`] stopped yielding events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscriptionEnd {
    /// The subscriber fell [`crate::EVENT_QUEUE_CAPACITY`] events behind and
    /// later events were not delivered; state built from it is incomplete.
    Overflowed,
    Closed(Termination),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Method { expected: &'static str, actual: String },
    Malformed(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Method { expected, actual } => write!(f, "expected a {expected} event, got {actual}"),
            DecodeError::Malformed(method) => write!(f, "{method} has unexpected params"),
        }
    }
}

impl std::error::Error for DecodeError {}
