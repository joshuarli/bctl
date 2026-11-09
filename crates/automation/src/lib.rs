//! bctl's native automation over one CDP connection per browser generation.
//!
//! Identity is layered: a browser generation, then a page target (its CDP `targetId`), then a frame and the
//! document it committed, then an element ref. [`Tabs`] lists, opens, closes, and selects page targets and keeps
//! the selection in a [`BindingStore`]; [`Pages`] reads accessibility [`Snapshot`]s of a target and resolves the
//! [`ElementRef`]s they hand out, each scoped to one [`DocumentScope`].

mod commands;
mod pages;
mod refs;
mod scope;
mod snapshot;
mod tabs;
mod url;

use std::time::Duration;

pub use pages::{Accessible, AttachError, Pages, RefError, ResolvedRef, SnapshotError, Staleness};
pub use refs::{BackendNodeId, ElementRef, Host, MAX_LIVE_REFS, REF_BLOCK, RefCounter, RefCounterError};
pub use scope::{DocumentId, DocumentScope, TargetScope};
pub use snapshot::{Checked, Coverage, MAX_DOCUMENTS, MAX_SNAPSHOT_BYTES, MAX_TEXT_CHARS, NodeState, NodeValue, Snapshot, SnapshotNode, SnapshotOptions};
pub use tabs::{Binding, BindingStore, BindingStoreError, Listing, PageTarget, Selection, TabError, Tabs};
pub use url::SafeUrl;

/// How long one target command may wait for the browser's reply.
pub const COMMAND_DEADLINE: Duration = Duration::from_secs(10);
