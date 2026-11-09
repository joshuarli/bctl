use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::num::NonZeroU64;
use std::ops::Range;

use bctl_cdp::protocol::{FrameId, TargetId};

use crate::{DocumentId, DocumentScope, TargetScope};

/// Refs taken from a [`RefCounter`] at a time.
pub const REF_BLOCK: u64 = 256;
/// Live refs one connection keeps; past this the oldest expire and fail as stale.
pub const MAX_LIVE_REFS: usize = 50_000;

/// An element ref as shown in a snapshot (`e12`). Its number is never handed out twice by one counter, so a
/// ref names at most one node in its whole life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementRef(NonZeroU64);

impl ElementRef {
    /// Accepts `e12` or `@e12`.
    pub fn parse(text: &str) -> Option<ElementRef> {
        let digits = text.strip_prefix('@').unwrap_or(text).strip_prefix('e')?;
        if digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse::<NonZeroU64>().ok().map(ElementRef)
    }

    pub fn number(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for ElementRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "e{}", self.0)
    }
}

/// A DOM node's CDP `backendNodeId`: stable for the node's life within its renderer process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BackendNodeId(pub i64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefCounterError(pub String);

impl fmt::Display for RefCounterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Durable source of ref numbers. `reserve` returns numbers no earlier call returned, even in an earlier
/// process, so refs are never reused across daemon restarts.
pub trait RefCounter {
    fn reserve(&self, count: u64) -> Result<Range<u64>, RefCounterError>;
}

/// The CDP target whose renderer holds a frame's document: the page itself, or an out-of-process iframe's
/// own target (whose id is its frame id).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Host {
    Page,
    Oopif(TargetId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NodeKey {
    pub(crate) document: DocumentScope,
    pub(crate) node: BackendNodeId,
}

#[derive(Clone, Debug)]
pub(crate) struct RefEntry {
    pub(crate) key: NodeKey,
    pub(crate) host: Host,
}

#[derive(Default)]
pub(crate) struct RefTable {
    entries: BTreeMap<ElementRef, RefEntry>,
    by_node: HashMap<NodeKey, ElementRef>,
    unused: Range<u64>,
}

impl RefTable {
    pub(crate) fn issue(&mut self, key: NodeKey, host: Host, counter: &dyn RefCounter) -> Result<ElementRef, RefCounterError> {
        if let Some(element) = self.by_node.get(&key) {
            return Ok(*element);
        }
        if self.unused.is_empty() {
            let block = counter.reserve(REF_BLOCK)?;
            if block.start == 0 || block.is_empty() {
                return Err(RefCounterError(format!("the ref counter returned {block:?}")));
            }
            self.unused = block;
        }
        let number = self.unused.next().and_then(NonZeroU64::new).expect("the block is non-empty and starts above zero");
        let element = ElementRef(number);
        self.by_node.insert(key.clone(), element);
        self.entries.insert(element, RefEntry { key, host });
        while self.entries.len() > MAX_LIVE_REFS {
            let (_, oldest) = self.entries.pop_first().expect("the table is over its limit");
            self.by_node.remove(&oldest.key);
        }
        Ok(element)
    }

    pub(crate) fn get(&self, element: ElementRef) -> Option<&RefEntry> {
        self.entries.get(&element)
    }

    pub(crate) fn forget(&mut self, element: ElementRef) {
        if let Some(entry) = self.entries.remove(&element) {
            self.by_node.remove(&entry.key);
        }
    }

    pub(crate) fn retire(&mut self, target: &TargetScope, listed: &HashMap<Host, HashMap<FrameId, DocumentId>>) {
        let ended: Vec<ElementRef> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                let scope = &entry.key.document;
                scope.target == *target
                    && listed.get(&entry.host).is_some_and(|frames| frames.get(&scope.frame) != Some(&scope.document))
            })
            .map(|(element, _)| *element)
            .collect();
        for element in ended {
            self.forget(element);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use bctl_cdp::Generation;

    use super::*;

    struct Counter(RefCell<u64>);

    impl RefCounter for Counter {
        fn reserve(&self, count: u64) -> Result<Range<u64>, RefCounterError> {
            let start = *self.0.borrow();
            *self.0.borrow_mut() += count;
            Ok(start..start + count)
        }
    }

    fn key(frame: &str, loader: &str, node: i64) -> NodeKey {
        let target = TargetScope { generation: Generation::new(1), target: TargetId::new("T") };
        NodeKey { document: DocumentScope { target, frame: FrameId::new(frame), document: DocumentId::new(loader) }, node: BackendNodeId(node) }
    }

    #[test]
    fn refs_parse_only_in_their_shown_form() {
        assert_eq!(ElementRef::parse("e12").map(ElementRef::number), Some(12));
        assert_eq!(ElementRef::parse("@e7").map(|e| e.to_string()), Some("e7".into()));
        for bad in ["", "e", "e0", "e01", "12", "E1", "e-1", "e+1", "e1x", "@@e1", "e99999999999999999999"] {
            assert_eq!(ElementRef::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_node_keeps_its_ref_within_its_document_and_gets_a_new_one_in_the_next() {
        let counter = Counter(RefCell::new(1));
        let mut table = RefTable::default();
        let first = table.issue(key("F", "L1", 5), Host::Page, &counter).unwrap();
        let other = table.issue(key("F", "L1", 6), Host::Page, &counter).unwrap();
        assert_eq!(table.issue(key("F", "L1", 5), Host::Page, &counter).unwrap(), first);
        let next_document = table.issue(key("F", "L2", 5), Host::Page, &counter).unwrap();
        assert!(first < other && other < next_document, "numbers only grow");
    }

    #[test]
    fn retiring_a_document_forgets_its_refs_and_only_those() {
        let counter = Counter(RefCell::new(1));
        let mut table = RefTable::default();
        let old = table.issue(key("F", "L1", 5), Host::Page, &counter).unwrap();
        let child = table.issue(key("C", "L9", 5), Host::Page, &counter).unwrap();
        let remote = table.issue(key("R", "L3", 5), Host::Oopif(TargetId::new("R")), &counter).unwrap();
        let target = key("F", "L1", 5).document.target;
        let listed = HashMap::from([(Host::Page, HashMap::from([(FrameId::new("F"), DocumentId::new("L2")), (FrameId::new("C"), DocumentId::new("L9"))]))]);
        table.retire(&target, &listed);
        assert!(table.get(old).is_none(), "the frame shows a new document");
        assert!(table.get(child).is_some());
        assert!(table.get(remote).is_some(), "its host was not listed");
        let gone = HashMap::from([(Host::Page, HashMap::new())]);
        table.retire(&target, &gone);
        assert!(table.get(child).is_none(), "its frame is gone");
    }

    #[test]
    fn the_oldest_refs_expire_past_the_limit_and_numbers_are_never_reused() {
        let counter = Counter(RefCell::new(1));
        let mut table = RefTable::default();
        let first = table.issue(key("F", "L", 0), Host::Page, &counter).unwrap();
        for node in 1..=MAX_LIVE_REFS as i64 {
            table.issue(key("F", "L", node), Host::Page, &counter).unwrap();
        }
        assert_eq!(table.len(), MAX_LIVE_REFS);
        assert!(table.get(first).is_none());
        let again = table.issue(key("F", "L", 0), Host::Page, &counter).unwrap();
        assert!(again > first, "an expired node gets a fresh number");
    }
}
