use std::fmt;

use bctl_cdp::Generation;
use bctl_cdp::protocol::{FrameId, TargetId};

/// One page target of one browser generation. A target id from another generation never names it, even if
/// the browser reused the id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TargetScope {
    pub generation: Generation,
    pub target: TargetId,
}

/// A document: the CDP `loaderId` a frame committed. A navigation commits a new one, so anything scoped to a
/// document (element refs) ends with it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocumentId(String);

impl DocumentId {
    pub fn new(loader_id: impl Into<String>) -> DocumentId {
        DocumentId(loader_id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One document in one frame of one target: the narrowest scope an element ref can belong to. The main
/// frame's id is the target id; an iframe, in-process or not, has its own.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocumentScope {
    pub target: TargetScope,
    pub frame: FrameId,
    pub document: DocumentId,
}
