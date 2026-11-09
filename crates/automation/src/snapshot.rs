use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;

use bctl_cdp::protocol::FrameId;
use miniserde::json;

use crate::commands::{AxNode, AxValue, DomNode};
use crate::refs::{BackendNodeId, ElementRef, Host, NodeKey, RefCounter, RefCounterError, RefTable};
use crate::{DocumentScope, TargetScope};

/// The most text one snapshot renders; nodes past it are counted in [`Snapshot::omitted`] instead.
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
/// The most documents (the page and its iframes) one snapshot reads.
pub const MAX_DOCUMENTS: usize = 32;
/// Accessible names and values longer than this many characters are cut and end in `…`.
pub const MAX_TEXT_CHARS: usize = 160;
const MAX_TREE_DEPTH: usize = 256;

/// Chromium's own roles for controls (`Iframe`, `PopUpButton`, …) are listed beside the ARIA ones it reports.
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "link",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "treeitem",
    "Iframe",
    "DisclosureTriangle",
    "ColorWell",
    "PopUpButton",
    "ToggleButton",
    "date",
    "DateTime",
    "InputTime",
];
const WRAPPER_ROLES: &[&str] = &["none", "presentation", "generic", "RootWebArea", "WebArea", "MenuListPopup", "LabelText"];
const TEXT_FIELD_ROLES: &[&str] = &["textbox", "searchbox", "spinbutton"];
const SKIPPED_ROLES: &[&str] = &["InlineTextBox", "LineBreak"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// Only controls a caller can act on, nested only under each other.
    Interactive,
    /// Every node with content, minus wrappers that carry none.
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotOptions {
    pub coverage: Coverage,
    /// Rendered levels to keep; the top level is 1.
    pub depth: Option<NonZeroUsize>,
    /// A CSS selector in the main frame's document; only the first match and its subtree are shown.
    pub scope: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub target: TargetScope,
    pub coverage: Coverage,
    pub nodes: Vec<SnapshotNode>,
    /// Nodes left out to keep the text within [`MAX_SNAPSHOT_BYTES`].
    pub omitted: usize,
    /// Documents read: the main frame's and each iframe's.
    pub documents: usize,
    pub cdp_calls: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotNode {
    /// Absent for text, and for a node whose document ended while the snapshot was read.
    pub element: Option<ElementRef>,
    /// The browser-computed role. Text is `text` and an iframe is `iframe`.
    pub role: String,
    pub name: String,
    pub value: Option<NodeValue>,
    pub state: NodeState,
    pub children: Vec<SnapshotNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeValue {
    Text(String),
    /// A password field's value, never read into the snapshot.
    Redacted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeState {
    pub disabled: bool,
    pub checked: Option<Checked>,
    pub selected: bool,
    pub expanded: Option<bool>,
    pub level: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checked {
    Checked,
    Unchecked,
    Mixed,
}

impl Snapshot {
    /// One line per node (`- button "Sign in" [ref=e3]`), indented two spaces per level.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for node in &self.nodes {
            render(node, 0, &mut out);
        }
        if self.nodes.is_empty() {
            out.push_str(match self.coverage {
                Coverage::Interactive => "(no interactive elements)\n",
                Coverage::Full => "(no content)\n",
            });
        }
        if self.omitted > 0 {
            out.push_str(&format!("- … {} more nodes not shown\n", self.omitted));
        }
        out
    }
}

fn render(node: &SnapshotNode, depth: usize, out: &mut String) {
    out.push_str(&line(node, depth));
    out.push('\n');
    for child in &node.children {
        render(child, depth + 1, out);
    }
}

fn line(node: &SnapshotNode, depth: usize) -> String {
    let mut line = format!("{:indent$}- {}", "", node.role, indent = depth * 2);
    if !node.name.is_empty() {
        line.push(' ');
        line.push_str(&json::to_string(&node.name));
    }
    let mut attributes = Vec::new();
    if let Some(element) = node.element {
        attributes.push(format!("ref={element}"));
    }
    let state = node.state;
    if let Some(level) = state.level {
        attributes.push(format!("level={level}"));
    }
    match state.checked {
        Some(Checked::Checked) => attributes.push("checked".into()),
        Some(Checked::Unchecked) => attributes.push("unchecked".into()),
        Some(Checked::Mixed) => attributes.push("mixed".into()),
        None => {}
    }
    if state.selected {
        attributes.push("selected".into());
    }
    match state.expanded {
        Some(true) => attributes.push("expanded".into()),
        Some(false) => attributes.push("collapsed".into()),
        None => {}
    }
    if state.disabled {
        attributes.push("disabled".into());
    }
    if !attributes.is_empty() {
        line.push_str(&format!(" [{}]", attributes.join(", ")));
    }
    match &node.value {
        Some(NodeValue::Text(text)) => {
            line.push_str(": ");
            line.push_str(&json::to_string(text));
        }
        Some(NodeValue::Redacted) => line.push_str(": <redacted>"),
        None => {}
    }
    line
}

pub(crate) fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_TEXT_CHARS * 4));
    let mut chars = 0;
    for word in text.split_whitespace() {
        if chars > 0 {
            out.push(' ');
            chars += 1;
        }
        for c in word.chars() {
            if chars == MAX_TEXT_CHARS {
                out.push('…');
                return out;
            }
            out.push(c);
            chars += 1;
        }
    }
    out
}

pub(crate) struct Document {
    pub(crate) scope: DocumentScope,
    pub(crate) host: Host,
}

#[derive(Default)]
pub(crate) struct DomFacts {
    pub(crate) passwords: HashSet<BackendNodeId>,
    pub(crate) frame_owners: HashMap<BackendNodeId, FrameId>,
    pub(crate) covered: HashSet<FrameId>,
}

impl DomFacts {
    pub(crate) fn add(&mut self, root: &DomNode, frame: FrameId) -> HashSet<BackendNodeId> {
        self.covered.insert(frame);
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let id = BackendNodeId(node.backend_node_id);
            seen.insert(id);
            if node.local_name.as_deref() == Some("input") && is_password(node.attributes.as_deref().unwrap_or_default()) {
                self.passwords.insert(id);
            }
            if let Some(frame) = &node.frame_id
                && node.local_name.as_deref().is_some_and(|name| name == "iframe" || name == "frame")
            {
                self.frame_owners.insert(id, frame.clone());
                if node.content_document.is_some() {
                    self.covered.insert(frame.clone());
                }
            }
            stack.extend(node.children.iter().flatten());
            stack.extend(node.shadow_roots.iter().flatten());
            stack.extend(node.content_document.as_deref());
        }
        seen
    }
}

fn is_password(attributes: &[String]) -> bool {
    attributes.chunks(2).any(|pair| matches!(pair, [name, value] if name.eq_ignore_ascii_case("type") && value.trim().eq_ignore_ascii_case("password")))
}

pub(crate) fn needs_dom(nodes: &[AxNode]) -> bool {
    nodes.iter().any(|node| !node.ignored && (role_of(node) == "Iframe" || value_of(node.value.as_ref()).is_some_and(|v| !v.is_empty())))
}

fn string_of(value: Option<&AxValue>) -> Option<&str> {
    match value?.value.as_ref()? {
        json::Value::String(s) => Some(s),
        _ => None,
    }
}

fn role_of(node: &AxNode) -> &str {
    string_of(node.role.as_ref()).unwrap_or("")
}

fn value_of(value: Option<&AxValue>) -> Option<String> {
    match value?.value.as_ref()? {
        json::Value::String(s) => Some(s.clone()),
        json::Value::Number(n) => Some(n.to_string()),
        json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn state_of(node: &AxNode) -> NodeState {
    let mut state = NodeState::default();
    for property in node.properties.iter().flatten() {
        let value = property.value.value.as_ref();
        let flag = matches!(value, Some(json::Value::Bool(true))) || matches!(value, Some(json::Value::String(s)) if s == "true");
        match property.name.as_str() {
            "disabled" => state.disabled = flag,
            "selected" => state.selected = flag,
            "expanded" => state.expanded = value.map(|_| flag),
            "checked" => {
                state.checked = match value {
                    Some(json::Value::String(s)) if s == "mixed" => Some(Checked::Mixed),
                    Some(_) if flag => Some(Checked::Checked),
                    Some(_) => Some(Checked::Unchecked),
                    None => None,
                }
            }
            "level" => {
                state.level = match value {
                    Some(json::Value::Number(n)) => n.to_string().parse().ok(),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    state
}

enum Class {
    Skipped,
    Wrapper,
    Text,
    Element { interactive: bool },
}

fn classify(node: &AxNode, coverage: Coverage) -> Class {
    let role = role_of(node);
    if SKIPPED_ROLES.contains(&role) {
        return Class::Skipped;
    }
    if node.ignored {
        return Class::Wrapper;
    }
    if role == "StaticText" {
        return match coverage {
            Coverage::Full => Class::Text,
            Coverage::Interactive => Class::Skipped,
        };
    }
    let interactive = INTERACTIVE_ROLES.contains(&role);
    let unnamed_group = role == "group" && string_of(node.name.as_ref()).is_none_or(|n| n.trim().is_empty());
    match coverage {
        Coverage::Interactive if !interactive => Class::Wrapper,
        Coverage::Full if WRAPPER_ROLES.contains(&role) || unnamed_group => Class::Wrapper,
        _ => Class::Element { interactive },
    }
}

fn shown_role(role: &str) -> &str {
    match role {
        "Iframe" => "iframe",
        "StaticText" => "text",
        role => role,
    }
}

pub(crate) struct Draft {
    text: bool,
    interactive: bool,
    role: String,
    name: String,
    value: Option<NodeValue>,
    state: NodeState,
    origin: Option<(usize, BackendNodeId)>,
    pub(crate) children: Vec<usize>,
}

pub(crate) struct ChildFrame {
    pub(crate) owner: usize,
    pub(crate) frame: FrameId,
    pub(crate) level: usize,
}

#[derive(Default)]
pub(crate) struct Drafts {
    pub(crate) nodes: Vec<Draft>,
    pub(crate) roots: Vec<usize>,
}

pub(crate) struct FrameRead<'a> {
    pub(crate) nodes: &'a [AxNode],
    pub(crate) document: usize,
    pub(crate) facts: &'a DomFacts,
    pub(crate) coverage: Coverage,
    pub(crate) depth: Option<NonZeroUsize>,
}

pub(crate) fn root_of(nodes: &[AxNode]) -> Option<usize> {
    nodes.iter().position(|n| role_of(n) == "RootWebArea").or((!nodes.is_empty()).then_some(0))
}

fn index_of(nodes: &[AxNode]) -> HashMap<&str, usize> {
    nodes.iter().enumerate().map(|(i, n)| (n.node_id.as_str(), i)).collect()
}

fn children_of<'a>(nodes: &'a [AxNode], index: &'a HashMap<&str, usize>, node: usize) -> impl Iterator<Item = usize> + 'a {
    nodes[node].child_ids.iter().flatten().filter_map(|c| index.get(c.as_str()).copied())
}

pub(crate) fn roots_within(nodes: &[AxNode], subtree: &HashSet<BackendNodeId>) -> Vec<usize> {
    let index = index_of(nodes);
    let inside = |i: usize| nodes[i].backend_dom_node_id.is_some_and(|b| subtree.contains(&BackendNodeId(b)));
    let mut roots = Vec::new();
    let mut visited = HashSet::new();
    let mut stack: Vec<usize> = root_of(nodes).into_iter().collect();
    while let Some(i) = stack.pop() {
        if !visited.insert(i) {
            continue;
        }
        if inside(i) {
            roots.push(i);
            continue;
        }
        let children: Vec<usize> = children_of(nodes, &index, i).collect();
        stack.extend(children.into_iter().rev());
    }
    roots
}

impl FrameRead<'_> {
    pub(crate) fn read(&self, drafts: &mut Drafts, starts: &[usize], parent: Option<usize>, level: usize) -> Vec<ChildFrame> {
        let mut reader = Reader { read: self, index: index_of(self.nodes), drafts, frames: Vec::new(), visited: HashSet::new() };
        for &start in starts {
            reader.visit(start, parent, level, 0);
        }
        reader.frames
    }
}

struct Reader<'r, 'a> {
    read: &'r FrameRead<'a>,
    index: HashMap<&'a str, usize>,
    drafts: &'r mut Drafts,
    frames: Vec<ChildFrame>,
    visited: HashSet<usize>,
}

impl Reader<'_, '_> {
    fn push(&mut self, draft: Draft, parent: Option<usize>) -> usize {
        let id = self.drafts.nodes.len();
        self.drafts.nodes.push(draft);
        match parent {
            Some(parent) => self.drafts.nodes[parent].children.push(id),
            None => self.drafts.roots.push(id),
        }
        id
    }

    fn visit_children(&mut self, index: usize, parent: Option<usize>, level: usize, tree_depth: usize) {
        let children: Vec<usize> = children_of(self.read.nodes, &self.index, index).collect();
        for child in children {
            self.visit(child, parent, level, tree_depth + 1);
        }
    }

    fn visit(&mut self, index: usize, parent: Option<usize>, level: usize, tree_depth: usize) {
        if tree_depth > MAX_TREE_DEPTH || !self.visited.insert(index) {
            return;
        }
        let read = self.read;
        let node = &read.nodes[index];
        let within = |level: usize| read.depth.is_none_or(|limit| level <= limit.get());
        match classify(node, read.coverage) {
            Class::Skipped => {}
            Class::Wrapper => self.visit_children(index, parent, level, tree_depth),
            Class::Text => {
                if within(level + 1) {
                    let name = string_of(node.name.as_ref()).unwrap_or("").to_string();
                    let draft = Draft { text: true, interactive: false, role: "text".into(), name, value: None, state: NodeState::default(), origin: None, children: Vec::new() };
                    self.push(draft, parent);
                }
            }
            Class::Element { interactive } => {
                let level = level + 1;
                if !within(level) {
                    return;
                }
                let role = role_of(node);
                let backend = node.backend_dom_node_id.map(BackendNodeId);
                let password = backend.is_some_and(|b| read.facts.passwords.contains(&b));
                let value = value_of(node.value.as_ref()).filter(|v| !v.is_empty());
                let value = if password { value.map(|_| NodeValue::Redacted) } else { value.map(|v| NodeValue::Text(normalize(&v))) };
                let draft = Draft {
                    text: false,
                    interactive,
                    role: shown_role(role).to_string(),
                    name: string_of(node.name.as_ref()).unwrap_or("").to_string(),
                    value,
                    state: state_of(node),
                    origin: backend.map(|b| (read.document, b)),
                    children: Vec::new(),
                };
                let id = self.push(draft, parent);
                if role == "Iframe"
                    && let Some(frame) = backend.and_then(|b| read.facts.frame_owners.get(&b))
                {
                    self.frames.push(ChildFrame { owner: id, frame: frame.clone(), level });
                }
                if !password && !TEXT_FIELD_ROLES.contains(&role) {
                    self.visit_children(index, Some(id), level, tree_depth);
                }
            }
        }
    }
}

enum Kept {
    Text(String),
    Node { draft: usize, name: String, children: Vec<Kept> },
}

impl Kept {
    fn count(&self) -> usize {
        match self {
            Kept::Text(_) => 1,
            Kept::Node { children, .. } => 1 + children.iter().map(Kept::count).sum::<usize>(),
        }
    }
}

fn prune(drafts: &Drafts, id: usize, coverage: Coverage) -> Option<Kept> {
    let draft = &drafts.nodes[id];
    let mut name = normalize(&draft.name);
    let mut children = prune_children(drafts, &draft.children, &name, coverage);
    let plain = !draft.interactive && draft.value.is_none();
    if coverage == Coverage::Full && plain && name.is_empty() && let [Kept::Text(text)] = children.as_slice() {
        name = text.clone();
        children.clear();
    }
    if plain && name.is_empty() && children.is_empty() {
        return None;
    }
    Some(Kept::Node { draft: id, name, children })
}

fn prune_children(drafts: &Drafts, ids: &[usize], parent_name: &str, coverage: Coverage) -> Vec<Kept> {
    let mut kept = Vec::new();
    let mut run = String::new();
    let flush = |run: &mut String, kept: &mut Vec<Kept>| {
        let text = normalize(run);
        run.clear();
        if !text.is_empty() && text != parent_name {
            kept.push(Kept::Text(text));
        }
    };
    for &id in ids {
        let draft = &drafts.nodes[id];
        if draft.text {
            run.push_str(&draft.name);
            continue;
        }
        flush(&mut run, &mut kept);
        kept.extend(prune(drafts, id, coverage));
    }
    flush(&mut run, &mut kept);
    let names: Vec<Option<String>> = kept.iter().map(|k| if let Kept::Node { name, .. } = k { Some(name.clone()) } else { None }).collect();
    let labels: Vec<bool> = (0..kept.len())
        .map(|i| match &kept[i] {
            Kept::Text(text) => [i.checked_sub(1), Some(i + 1)].into_iter().flatten().any(|j| names.get(j).and_then(Option::as_ref) == Some(text)),
            Kept::Node { .. } => false,
        })
        .collect();
    let mut labels = labels.into_iter();
    kept.retain(|_| !labels.next().expect("one flag per kept node"));
    kept
}

pub(crate) fn finish(
    drafts: &Drafts,
    documents: &[Document],
    coverage: Coverage,
    refs: &mut RefTable,
    counter: &dyn RefCounter,
) -> Result<(Vec<SnapshotNode>, usize), RefCounterError> {
    let kept = prune_children(drafts, &drafts.roots, "", coverage);
    let mut emitter = Emitter { drafts, documents, refs, counter, used: 0, omitted: 0 };
    let nodes = emitter.emit(kept, 0)?;
    Ok((nodes, emitter.omitted))
}

struct Emitter<'a> {
    drafts: &'a Drafts,
    documents: &'a [Document],
    refs: &'a mut RefTable,
    counter: &'a dyn RefCounter,
    used: usize,
    omitted: usize,
}

impl Emitter<'_> {
    fn emit(&mut self, kept: Vec<Kept>, depth: usize) -> Result<Vec<SnapshotNode>, RefCounterError> {
        let mut nodes = Vec::new();
        for kept in kept {
            if self.omitted > 0 {
                self.omitted += kept.count();
                continue;
            }
            let count = kept.count();
            let (mut node, children) = match kept {
                Kept::Text(text) => {
                    (SnapshotNode { element: None, role: "text".into(), name: text, value: None, state: NodeState::default(), children: Vec::new() }, Vec::new())
                }
                Kept::Node { draft, name, children } => {
                    let draft = &self.drafts.nodes[draft];
                    let element = match draft.origin {
                        Some((document, node)) => {
                            let document = &self.documents[document];
                            Some(self.refs.issue(NodeKey { document: document.scope.clone(), node }, document.host.clone(), self.counter)?)
                        }
                        None => None,
                    };
                    let node = SnapshotNode { element, role: draft.role.clone(), name, value: draft.value.clone(), state: draft.state, children: Vec::new() };
                    (node, children)
                }
            };
            let bytes = line(&node, depth).len() + 1;
            if self.used + bytes > MAX_SNAPSHOT_BYTES {
                self.omitted += count;
                continue;
            }
            self.used += bytes;
            node.children = self.emit(children, depth + 1)?;
            nodes.push(node);
        }
        Ok(nodes)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ops::Range;

    use bctl_cdp::Generation;
    use bctl_cdp::protocol::TargetId;

    use super::*;
    use crate::DocumentId;
    use crate::commands::AxTree;

    struct Counter(RefCell<u64>);

    impl RefCounter for Counter {
        fn reserve(&self, count: u64) -> Result<Range<u64>, RefCounterError> {
            let start = *self.0.borrow();
            *self.0.borrow_mut() += count;
            Ok(start..start + count)
        }
    }

    const FORM: &str = r#"{"nodes":[
        {"nodeId":"1","ignored":false,"role":{"value":"RootWebArea"},"name":{"value":"Form"},"childIds":["2"],"backendDOMNodeId":1},
        {"nodeId":"2","ignored":true,"role":{"value":"none"},"childIds":["3","4","5","9","10","11","13"],"backendDOMNodeId":2},
        {"nodeId":"3","ignored":false,"role":{"value":"heading"},"name":{"value":"Sign in"},"properties":[{"name":"level","value":{"value":1}}],"childIds":["30"],"backendDOMNodeId":3},
        {"nodeId":"30","ignored":false,"role":{"value":"StaticText"},"name":{"value":"Sign in"},"childIds":["31"],"backendDOMNodeId":30},
        {"nodeId":"31","ignored":false,"role":{"value":"InlineTextBox"},"name":{"value":"Sign in"}},
        {"nodeId":"4","ignored":false,"role":{"value":"LabelText"},"childIds":["40","41"],"backendDOMNodeId":4},
        {"nodeId":"40","ignored":false,"role":{"value":"StaticText"},"name":{"value":"Password "},"backendDOMNodeId":40},
        {"nodeId":"41","ignored":false,"role":{"value":"textbox"},"name":{"value":"Password "},"value":{"value":"•••••••"},"childIds":["42"],"backendDOMNodeId":41},
        {"nodeId":"42","ignored":false,"role":{"value":"StaticText"},"name":{"value":"•••••••"},"backendDOMNodeId":42},
        {"nodeId":"5","ignored":false,"role":{"value":"checkbox"},"name":{"value":" Remember"},"properties":[{"name":"checked","value":{"value":"true"}}],"backendDOMNodeId":5},
        {"nodeId":"9","ignored":false,"role":{"value":"button"},"name":{"value":"Later"},"properties":[{"name":"disabled","value":{"value":true}}],"childIds":["90"],"backendDOMNodeId":9},
        {"nodeId":"90","ignored":false,"role":{"value":"StaticText"},"name":{"value":"Later"},"backendDOMNodeId":90},
        {"nodeId":"10","ignored":false,"role":{"value":"generic"},"childIds":["100","101"],"backendDOMNodeId":10},
        {"nodeId":"100","ignored":false,"role":{"value":"StaticText"},"name":{"value":"Hello "},"backendDOMNodeId":100},
        {"nodeId":"101","ignored":false,"role":{"value":"StaticText"},"name":{"value":" world"},"backendDOMNodeId":101},
        {"nodeId":"11","ignored":false,"role":{"value":"paragraph"},"childIds":["110"],"backendDOMNodeId":11},
        {"nodeId":"110","ignored":false,"role":{"value":"StaticText"},"name":{"value":"bad�x"},"backendDOMNodeId":110},
        {"nodeId":"13","ignored":false,"role":{"value":"Iframe"},"name":{"value":"inner"},"backendDOMNodeId":13}
    ]}"#;

    fn snapshot(coverage: Coverage, depth: Option<usize>) -> (Vec<SnapshotNode>, Vec<ChildFrame>) {
        let tree: AxTree = json::from_str(FORM).unwrap();
        let mut facts = DomFacts::default();
        facts.passwords.insert(BackendNodeId(41));
        facts.frame_owners.insert(BackendNodeId(13), FrameId::new("CHILD"));
        let read = FrameRead { nodes: &tree.nodes, document: 0, facts: &facts, coverage, depth: depth.and_then(NonZeroUsize::new) };
        let mut drafts = Drafts::default();
        let frames = read.read(&mut drafts, &[root_of(&tree.nodes).unwrap()], None, 0);
        let target = TargetScope { generation: Generation::new(1), target: TargetId::new("T") };
        let scope = DocumentScope { target, frame: FrameId::new("T"), document: DocumentId::new("L") };
        let documents = [Document { scope, host: Host::Page }];
        let (nodes, omitted) = finish(&drafts, &documents, coverage, &mut RefTable::default(), &Counter(RefCell::new(1))).unwrap();
        assert_eq!(omitted, 0);
        (nodes, frames)
    }

    fn text(nodes: Vec<SnapshotNode>, coverage: Coverage) -> String {
        let target = TargetScope { generation: Generation::new(1), target: TargetId::new("T") };
        Snapshot { target, coverage, nodes, omitted: 0, documents: 1, cdp_calls: 0 }.text()
    }

    #[test]
    fn interactive_snapshots_show_only_controls_and_never_a_password() {
        let (nodes, frames) = snapshot(Coverage::Interactive, None);
        assert_eq!(
            text(nodes, Coverage::Interactive),
            "- textbox \"Password\" [ref=e1]: <redacted>\n- checkbox \"Remember\" [ref=e2, checked]\n- button \"Later\" [ref=e3, disabled]\n- iframe \"inner\" [ref=e4]\n"
        );
        assert_eq!(frames.iter().map(|f| f.frame.as_str()).collect::<Vec<_>>(), ["CHILD"]);
    }

    #[test]
    fn full_snapshots_drop_wrappers_duplicate_text_and_labels_but_keep_content() {
        let (nodes, _) = snapshot(Coverage::Full, None);
        assert_eq!(
            text(nodes, Coverage::Full),
            "- heading \"Sign in\" [ref=e1, level=1]\n- textbox \"Password\" [ref=e2]: <redacted>\n- checkbox \"Remember\" [ref=e3, checked]\n- button \"Later\" [ref=e4, disabled]\n- text \"Hello world\"\n- paragraph \"bad\u{fffd}x\" [ref=e5]\n- iframe \"inner\" [ref=e6]\n"
        );
    }

    #[test]
    fn a_depth_limit_stops_reading_below_it() {
        let (nodes, frames) = snapshot(Coverage::Full, Some(1));
        assert!(nodes.iter().all(|n| n.children.is_empty()));
        assert_eq!(frames.len(), 1, "the iframe itself is at the top level");
        assert_eq!(frames[0].level, 1);
    }

    #[test]
    fn names_are_collapsed_and_cut() {
        assert_eq!(normalize("  a \n\t b  "), "a b");
        let long = "x".repeat(MAX_TEXT_CHARS + 5);
        let cut = normalize(&long);
        assert_eq!(cut.chars().count(), MAX_TEXT_CHARS + 1);
        assert!(cut.ends_with('…'));
    }
}
