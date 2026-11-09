//! One Chrome DevTools Protocol connection to a browser websocket.
//!
//! A [`Connection`] is owned by one browser generation: it never reconnects,
//! so a command is dispatched at most once. Its [`Driver`] must be polled to
//! completion by the owner; it resolves with the connection's [`Termination`]
//! after every pending command has been answered or failed.

mod connection;
mod endpoint;
mod error;
pub mod protocol;
mod surrogates;

use std::time::Duration;

pub use connection::{Connection, Driver, Event, EventFilter, EventScope, Generation, Scope, Subscription, connect, open};
pub use endpoint::{Endpoint, EndpointError};
pub use error::{CallError, ConnectError, DecodeError, SubscriptionEnd, Termination, Unobserved};

/// The largest websocket message, in either direction. A larger inbound
/// message ends the connection; a larger command is refused before dispatch.
pub const MAX_MESSAGE_BYTES: usize = 32 << 20;
/// Commands awaiting a reply on one connection; further calls fail fast.
pub const MAX_PENDING_COMMANDS: usize = 256;
/// Undelivered events per subscription before it ends as overflowed.
pub const EVENT_QUEUE_CAPACITY: usize = 1024;
/// Dial plus websocket handshake.
pub const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
/// How long a shutdown waits for the browser to answer the close frame.
pub const CLOSE_DEADLINE: Duration = Duration::from_secs(2);
/// Idle time before the connection pings the browser.
pub const PING_INTERVAL: Duration = Duration::from_secs(5);
/// Silence after a ping that ends the connection as unresponsive.
pub const PONG_DEADLINE: Duration = Duration::from_secs(3);
