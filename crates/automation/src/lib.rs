//! bctl's native automation over one CDP connection per browser generation.
//!
//! Identity is layered: a browser generation, then a page target (its CDP `targetId`), then a frame and the
//! document it committed. [`Tabs`] lists, opens, closes, and selects page targets and keeps the selection in
//! a [`BindingStore`]; [`TargetScope`] and [`DocumentScope`] are what anything finer, such as element refs,
//! is scoped to.

mod scope;
mod tabs;
mod url;

use std::time::Duration;

pub use scope::{DocumentId, DocumentScope, TargetScope};
pub use tabs::{Binding, BindingStore, BindingStoreError, Listing, PageTarget, Selection, TabError, Tabs};
pub use url::SafeUrl;

/// How long one target command may wait for the browser's reply.
pub const COMMAND_DEADLINE: Duration = Duration::from_secs(10);
