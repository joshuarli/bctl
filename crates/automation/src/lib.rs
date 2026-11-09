//! bctl's native automation over one CDP connection per browser generation, as a library.
//!
//! Identity is layered: a browser generation, then a page target (its CDP `targetId`), then a frame and the
//! document it committed, then an element ref. [`Tabs`] lists, opens, closes, and selects page targets and keeps
//! the selection in a [`BindingStore`]; [`Pages`] reads accessibility [`Snapshot`]s of a target, resolves the
//! [`ElementRef`]s they hand out, each scoped to one [`DocumentScope`], and runs [`Action`]s on elements named by
//! a [`Locator`] with real input events.
//!
//! A host program owns one [`cdp::Connection`], opened with [`cdp::connect`] to the browser websocket a
//! `bctl cdp-url` capability names, and drives its [`Driver`](cdp::Driver) to completion on a thread of its
//! own. It supplies the two stores bctl's daemon keeps in SQLite: a [`BindingStore`] for the selected target
//! and a [`RefCounter`] that never hands out a ref number twice. Then:
//!
//! 1. [`Tabs::open`] or [`Tabs::select`] chooses a target, and [`Tabs::selected`] returns its [`TargetScope`],
//!    or [`TabError::TargetGone`] once it has closed;
//! 2. [`Pages::snapshot`] reads the target and gives each interactive node a ref;
//! 3. [`Pages::act`] runs an [`Action`] on a [`Locator`]: a ref, or a [`Query`] by CSS, role and name, label,
//!    placeholder, or test id.
//!
//! Every value is scoped to the connection: a ref from another connection, target, or document fails as
//! [`Staleness`], never by re-resolving similar text. `examples/drive.rs` drives a page this way.

mod actions;
mod commands;
mod keys;
mod pages;
mod refs;
mod scope;
mod snapshot;
mod tabs;
mod url;

use std::time::Duration;

pub use actions::{
    Acted, ActedOn, Action, ActionError, Candidate, ClickCount, Condition, Dialog, DialogResponse, FieldValue, Interrupt, LoadState, Locator, MAX_CANDIDATES,
    MAX_READ_CHARS, Occurrence, Outcome, Query, Read, Readiness, Scroll, Uninterrupted,
};
pub use keys::{Chord, Modifier, UnknownKey};
pub use pages::{Accessible, AttachError, Pages, RefError, ResolvedRef, SnapshotError, Staleness};
pub use refs::{BackendNodeId, ElementRef, MAX_LIVE_REFS, RefCounter, RefCounterError};
pub use scope::{DocumentId, DocumentScope, TargetScope};
pub use snapshot::{Checked, Coverage, MAX_DOCUMENTS, MAX_SNAPSHOT_BYTES, MAX_TEXT_CHARS, NodeState, NodeValue, Snapshot, SnapshotNode, SnapshotOptions};
pub use tabs::{Binding, BindingStore, BindingStoreError, Listing, PageTarget, Selection, TabError, Tabs};
pub use url::SafeUrl;

/// The CDP transport this crate drives, re-exported so a host program connects with the same version.
pub use bctl_cdp as cdp;

/// How long one target command may wait for the browser's reply.
pub const COMMAND_DEADLINE: Duration = Duration::from_secs(10);
