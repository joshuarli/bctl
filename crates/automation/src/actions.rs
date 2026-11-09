use std::collections::{HashMap, HashSet};
use std::fmt;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use async_io::Timer;
use bctl_cdp::protocol::{CdpEvent, Command, DetachedFromTarget, FrameId, SessionId, TargetId};
use bctl_cdp::{CallError, EventFilter, EventScope, SubscriptionEnd};
use futures_lite::future;
use miniserde::json;

use crate::commands::{
    CaptureScreenshot, ChangeObject, Counted, DescribeObject, DialogOpening, DispatchKey, DispatchMouse, DispatchWheel, EvaluateInPage, Evaluated,
    GetBoxModel, GetContentQuads, GetFrameOwner, GetOwnProperties, HandleDialog, InsertText, InspectContext, InspectObject, LifecycleEvent, Navigate,
    BringToFront, CreateIsolatedWorld, GetFullAxTree, ReleaseObjectGroup, ResolveNodeIn, ScrollIntoView,
};
use crate::keys::{self, Chord, Key};
use crate::pages::{AttachError, Attachment, HostFrames, Lost, Pages, RefError, Staleness, frames};
use crate::refs::{BackendNodeId, ElementRef, Host, NodeKey, RefCounter, RefCounterError, RefTable};
use crate::snapshot::MAX_DOCUMENTS;
use crate::{DocumentScope, SafeUrl, TargetScope};

const WORLD: &str = "bctl";
const OBJECT_GROUP: &str = "bctl-action";
/// Matches an ambiguous locator lists, each with a fresh ref.
pub const MAX_CANDIDATES: usize = 10;
/// The most characters `get text` returns; longer text is cut and marked truncated.
pub const MAX_READ_CHARS: usize = 100_000;
const POLL: Duration = Duration::from_millis(100);
// Chromium discards input to an iframe for 500 ms after the iframe moves or first appears, as a defence
// against clickjacking, so a click into a frame younger than this would silently do nothing.
const FRAME_INPUT_GRACE: Duration = Duration::from_millis(500);

/// What a locator searches for. Text is compared exactly after collapsing whitespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    Css(String),
    /// The browser-computed role and, if given, accessible name, so implicit ARIA roles match.
    Role { role: String, name: Option<String> },
    /// Form controls named by a `<label>`, `aria-label`, or `aria-labelledby`.
    Label(String),
    Placeholder(String),
    /// `data-testid`.
    TestId(String),
}

/// Which match of a query to use, from 1. Matches are numbered in document order within a frame; frames
/// follow the page's frame tree, with out-of-process iframes after the page's own frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Occurrence(pub NonZeroUsize);

/// The element an action names. A query that matches more than one element fails as ambiguous unless an
/// occurrence is given; bctl never picks the first match on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Locator {
    Ref(ElementRef),
    Query { query: Query, occurrence: Option<Occurrence> },
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let quoted = |s: &str| json::to_string(s);
        match self {
            Query::Css(css) => write!(f, "css {}", quoted(css)),
            Query::Role { role, name: Some(name) } => write!(f, "role {role} {}", quoted(name)),
            Query::Role { role, name: None } => write!(f, "role {role}"),
            Query::Label(label) => write!(f, "label {}", quoted(label)),
            Query::Placeholder(text) => write!(f, "placeholder {}", quoted(text)),
            Query::TestId(id) => write!(f, "testid {}", quoted(id)),
        }
    }
}

impl fmt::Display for Locator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Locator::Ref(element) => write!(f, "@{element}"),
            Locator::Query { query, occurrence: None } => query.fmt(f),
            Locator::Query { query, occurrence: Some(n) } => write!(f, "{query} (match {})", n.0),
        }
    }
}

/// How far a navigation must get before `goto` returns. Network idle is never the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Commit,
    DomContentLoaded,
    Load,
    NetworkIdle,
}

impl Readiness {
    fn lifecycle_name(self) -> &'static str {
        match self {
            Readiness::Commit => "commit",
            Readiness::DomContentLoaded => "DOMContentLoaded",
            Readiness::Load => "load",
            Readiness::NetworkIdle => "networkIdle",
        }
    }
}

/// A document's `readyState` to wait for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    DomContentLoaded,
    Load,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickCount {
    Single,
    Double,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Scroll {
    IntoView(Locator),
    /// A mouse wheel turn at the viewport's centre, in CSS pixels.
    By { dx: f64, dy: f64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    /// At least one element matches.
    Present(Query),
    /// No element matches.
    Absent(Query),
    /// The main frame's rendered text contains this.
    Text(String),
    /// The main frame's URL matches; `*` matches any run of characters.
    Url(String),
    Loaded(LoadState),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// The element's rendered text, or the main frame's.
    Text(Option<Locator>),
    Title,
    /// The main frame's full URL, for the caller only; bctl never stores it.
    Url,
    Attribute { locator: Locator, name: String },
    /// A form control's current value.
    Value(Locator),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogResponse {
    /// For a prompt, `text` is the answer; absent answers with the prompt's default.
    Accept { text: Option<String> },
    Dismiss,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Navigate { url: String, readiness: Readiness, timeout: Duration },
    Click { locator: Locator, count: ClickCount },
    Hover(Locator),
    /// Replaces the field's content with `text`, as one insertion.
    Fill { locator: Locator, text: String },
    /// Types `text` key by key at the end of the element's content, or into whatever has focus.
    Type { into: Option<Locator>, text: String },
    Press { into: Option<Locator>, chord: Chord },
    Scroll(Scroll),
    Select { locator: Locator, values: Vec<String> },
    SetChecked { locator: Locator, checked: bool },
    Wait { condition: Condition, timeout: Duration },
    Read(Read),
    Screenshot,
    /// Runs the caller's expression in the page's own world and returns its JSON value.
    Evaluate(String),
    Dialog(DialogResponse),
}

impl Action {
    /// Whether the action can change the page. Mutating actions are the ones a person's control refuses.
    pub fn mutates(&self) -> bool {
        !matches!(self, Action::Wait { .. } | Action::Read(_) | Action::Screenshot)
    }

    /// Whether the action sends real input events, which only a shown tab handles; its tab is brought to the
    /// front first.
    pub fn sends_input(&self) -> bool {
        matches!(
            self,
            Action::Click { .. } | Action::Hover(_) | Action::Fill { .. } | Action::Type { .. } | Action::Press { .. } | Action::Scroll(_) | Action::SetChecked { .. }
        )
    }
}

/// Asks a long action to stop early. [`Pages::act`] asks between the polls of a wait and while a navigation it
/// started loads; it never stops an action part way through sending input.
pub trait Interrupt {
    fn interrupted(&self) -> bool;
}

/// Never asks an action to stop.
pub struct Uninterrupted;

impl Interrupt for Uninterrupted {
    fn interrupted(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    /// `alert`, `confirm`, `prompt`, or `beforeunload`, as the browser reports it.
    pub kind: String,
    pub message: String,
    pub default_prompt: String,
}

impl From<DialogOpening> for Dialog {
    fn from(opening: DialogOpening) -> Dialog {
        Dialog { kind: opening.kind, message: opening.message, default_prompt: opening.default_prompt.unwrap_or_default() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldValue {
    Text(String),
    /// A password field's value, never read.
    Redacted,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Done,
    Checked { changed: bool },
    Selected(Vec<String>),
    Text { text: String, truncated: bool },
    Attribute(Option<String>),
    Value(FieldValue),
    Evaluated(json::Value),
    /// Elements matching a waited-for query when the wait ended.
    Matched(usize),
    Png(Vec<u8>),
    /// The dialog answered, if bctl had seen it open.
    DialogHandled(Option<Dialog>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActedOn {
    pub frame: FrameId,
    /// Set when the element was named by ref.
    pub element: Option<ElementRef>,
}

#[derive(Clone, Debug)]
pub struct Acted {
    pub target: TargetScope,
    pub on: Option<ActedOn>,
    pub outcome: Outcome,
    pub cdp_calls: u32,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub element: ElementRef,
    pub frame: FrameId,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionError {
    OtherGeneration,
    InvalidLocator { locator: String, reason: String },
    NotFound { locator: String, matches: usize },
    Ambiguous { locator: String, matches: usize, candidates: Vec<Candidate> },
    Ref(RefError),
    NotVisible { locator: String, reason: String },
    Disabled { locator: String },
    Covered { locator: String, by: String },
    /// The click was dispatched but the page delivered its press to another element.
    Missed { locator: String, by: String },
    Detached { locator: String },
    NotActionable { locator: String, reason: String },
    /// A JavaScript dialog is open, or the action opened one; the page waits for it to be answered.
    DialogOpen(Dialog),
    NoDialog,
    Timeout { waiting_for: String, waited: Duration },
    /// The caller's [`Interrupt`] asked the action to stop while it waited. A navigation's command had already
    /// been sent.
    Interrupted { waiting_for: String, waited: Duration },
    NavigationFailed { url: SafeUrl, reason: String },
    /// The caller's expression threw.
    Script { exception: String },
    /// bctl's own page helper failed, which is a bctl bug or a page that broke the DOM it relies on.
    Helper { reason: String },
    Attach(AttachError),
    Cdp(CallError),
    Counter(RefCounterError),
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionError::OtherGeneration => f.write_str("the target belongs to another browser generation"),
            ActionError::InvalidLocator { locator, reason } => write!(f, "{locator} is not a usable locator: {reason}"),
            ActionError::NotFound { locator, matches: 0 } => write!(f, "nothing matches {locator}"),
            ActionError::NotFound { locator, matches } => write!(f, "{locator} asks for a match past the {matches} there are"),
            ActionError::Ambiguous { locator, matches, .. } => write!(f, "{locator} matches {matches} elements"),
            ActionError::Ref(error) => error.fmt(f),
            ActionError::NotVisible { locator, reason } => write!(f, "{locator} is not visible: {reason}"),
            ActionError::Disabled { locator } => write!(f, "{locator} is disabled"),
            ActionError::Covered { locator, by } => write!(f, "{locator} is covered by {by}, which would get the click"),
            ActionError::Missed { locator, by } => write!(f, "the click meant for {locator} was delivered to {by} instead"),
            ActionError::Detached { locator } => write!(f, "{locator} left its document before bctl could act on it"),
            ActionError::NotActionable { locator, reason } => write!(f, "{locator} {reason}"),
            ActionError::DialogOpen(dialog) => write!(f, "the page shows a {} dialog: {}", dialog.kind, json::to_string(&dialog.message)),
            ActionError::NoDialog => f.write_str("the page shows no dialog"),
            ActionError::Timeout { waiting_for, waited } => write!(f, "{waiting_for} did not happen within {} ms", waited.as_millis()),
            ActionError::Interrupted { waiting_for, waited } => write!(f, "waiting for {waiting_for} was interrupted after {} ms", waited.as_millis()),
            ActionError::NavigationFailed { url, reason } => write!(f, "navigating to {url} failed: {reason}"),
            ActionError::Script { exception } => write!(f, "the expression threw: {exception}"),
            ActionError::Helper { reason } => write!(f, "bctl's page helper failed: {reason}"),
            ActionError::Attach(error) => error.fmt(f),
            ActionError::Cdp(error) => error.fmt(f),
            ActionError::Counter(error) => write!(f, "cannot reserve element refs: {error}"),
        }
    }
}

impl std::error::Error for ActionError {}

impl From<Lost> for ActionError {
    fn from(lost: Lost) -> ActionError {
        ActionError::Attach(lost.into())
    }
}

impl ActionError {
    pub(crate) fn into_ref_error(self) -> RefError {
        match self {
            ActionError::Ref(error) => error,
            ActionError::Attach(error) => RefError::Attach(error),
            ActionError::Cdp(error) => RefError::Cdp(error),
            other => RefError::Cdp(CallError::MalformedReply { method: "Runtime.callFunctionOn", reason: other.to_string() }),
        }
    }
}

fn subscription_end(end: SubscriptionEnd) -> ActionError {
    match end {
        SubscriptionEnd::Overflowed => ActionError::Attach(AttachError::Churning),
        SubscriptionEnd::Closed(termination) => ActionError::Cdp(CallError::Closed(termination)),
    }
}

const ACTIONABLE: &str = "function(needsEnabled) {
  if (this.nodeType !== Node.ELEMENT_NODE) return 'not-element';
  if (!this.isConnected) return 'detached';
  if (!this.checkVisibility({ visibilityProperty: true })) return 'hidden';
  const box = this.getBoundingClientRect();
  if (box.width === 0 || box.height === 0) return 'empty';
  if (needsEnabled && (this.matches(':disabled') || this.closest('[aria-disabled=\"true\"]'))) return 'disabled';
  return 'ok';
}";

const SETTLED: &str = "function() {
  return new Promise(resolve => {
    let last = null;
    let frames = 0;
    const step = () => {
      const box = this.getBoundingClientRect();
      const same = last && box.x === last.x && box.y === last.y && box.width === last.width && box.height === last.height;
      if (same) return resolve('settled');
      if (++frames > 10) return resolve('moving');
      last = box;
      requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  });
}";

const ARM_PRESS: &str = "function() {
  const target = this;
  const record = { landed: false, elsewhere: null };
  addEventListener('mousedown', event => {
    const hit = event.composedPath()[0];
    if (event.composedPath().includes(target)) record.landed = true;
    else record.elsewhere = hit.localName + (hit.id ? '#' + hit.id : '');
  }, { capture: true, once: true });
  globalThis.bctlPress = record;
  return 'armed';
}";

const PRESS_LANDED: &str = "function() {
  const record = globalThis.bctlPress;
  delete globalThis.bctlPress;
  return record.landed ? 'landed' : record.elsewhere ? 'elsewhere:' + record.elsewhere : 'nothing';
}";

const DOCUMENT_AGE: &str = "function() { return performance.now(); }";

const DESCRIBE: &str = "function() {
  const text = (this.innerText ?? this.textContent ?? '').replace(/\\s+/g, ' ').trim().slice(0, 40);
  return this.localName + (this.id ? '#' + this.id : '') + (text ? ' \"' + text + '\"' : '');
}";

const HIT: &str = "function(x, y) {
  let hit = document.elementFromPoint(x, y);
  if (!hit) return 'outside';
  while (hit.shadowRoot) {
    const inner = hit.shadowRoot.elementFromPoint(x, y);
    if (!inner || inner === hit) break;
    hit = inner;
  }
  for (let node = hit; node; node = node.parentNode || node.host) if (node === this) return 'hit';
  const text = (hit.innerText ?? hit.textContent ?? '').replace(/\\s+/g, ' ').trim().slice(0, 40);
  return 'covered:' + hit.localName + (hit.id ? '#' + hit.id : '') + (text ? ' \"' + text + '\"' : '');
}";

const SEARCH: &str = "function(kind, value, limit) {
  const norm = s => (s || '').replace(/\\s+/g, ' ').trim();
  const all = selector => Array.from(document.querySelectorAll(selector));
  const labelText = label => {
    let text = '';
    const walk = node => {
      for (const child of node.childNodes) {
        if (child.nodeType === Node.TEXT_NODE) text += child.data;
        else if (child.nodeType === Node.ELEMENT_NODE && !child.matches('input, select, textarea, button')) walk(child);
      }
    };
    walk(label);
    return norm(text);
  };
  let found;
  if (kind === 'css') found = all(value);
  else if (kind === 'testid') found = all('[data-testid]').filter(e => e.getAttribute('data-testid') === value);
  else if (kind === 'placeholder') found = all('[placeholder]').filter(e => norm(e.getAttribute('placeholder')) === value);
  else if (kind === 'label') {
    const set = new Set();
    for (const label of all('label')) if (label.control && labelText(label) === value) set.add(label.control);
    for (const e of all('[aria-label]')) if (norm(e.getAttribute('aria-label')) === value) set.add(e);
    for (const e of all('[aria-labelledby]')) {
      const ids = e.getAttribute('aria-labelledby').split(/\\s+/);
      if (norm(ids.map(id => document.getElementById(id)).filter(Boolean).map(n => n.textContent).join(' ')) === value) set.add(e);
    }
    found = Array.from(set).sort((a, b) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) ? -1 : 1);
  } else throw new Error('unknown locator kind ' + kind);
  const shown = found.slice(0, limit);
  shown.total = found.length;
  return shown;
}";

const FILL_PREPARE: &str = "function() {
  const textTypes = ['text', 'search', 'url', 'tel', 'password', 'email', 'number'];
  const valueTypes = ['date', 'time', 'datetime-local', 'month', 'week', 'color', 'range'];
  if (this.localName === 'input') {
    if (valueTypes.includes(this.type)) return this.readOnly ? 'readonly' : 'value';
    if (!textTypes.includes(this.type)) return 'type:' + this.type;
    if (this.readOnly) return 'readonly';
    this.focus();
    this.select();
    return 'text';
  }
  if (this.localName === 'textarea') {
    if (this.readOnly) return 'readonly';
    this.focus();
    this.select();
    return 'text';
  }
  if (this.isContentEditable) {
    this.focus();
    const range = document.createRange();
    range.selectNodeContents(this);
    const selection = getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    return 'text';
  }
  return 'element:' + this.localName;
}";

const SET_VALUE: &str = "function(value) {
  this.focus();
  this.value = value;
  if (this.value !== value) return 'rejected';
  this.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return 'ok';
}";

const FOCUS_END: &str = "function() {
  this.focus();
  if ((this.localName === 'input' || this.localName === 'textarea') && this.selectionStart !== null) {
    this.setSelectionRange(this.value.length, this.value.length);
  } else if (this.isContentEditable) {
    const range = document.createRange();
    range.selectNodeContents(this);
    range.collapse(false);
    const selection = getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  }
  return this.matches(':focus, :focus-within') ? 'focused' : 'unfocusable';
}";

const SELECT: &str = "function(values) {
  if (this.localName !== 'select') return 'element:' + this.localName;
  const norm = s => s.replace(/\\s+/g, ' ').trim();
  const options = Array.from(this.options);
  const chosen = [];
  for (const value of values) {
    const option = options.find(o => o.value === value) || options.find(o => norm(o.label) === value);
    if (!option) return 'missing:' + value;
    if (option.disabled) return 'disabled:' + value;
    if (!chosen.includes(option)) chosen.push(option);
  }
  if (chosen.length > 1 && !this.multiple) return 'single';
  this.focus();
  for (const option of options) option.selected = chosen.includes(option);
  this.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return chosen.map(o => o.value);
}";

const CHECKED_STATE: &str = "function() {
  if (this.localName === 'input' && (this.type === 'checkbox' || this.type === 'radio')) return this.type + ':' + this.checked;
  const role = this.getAttribute('role');
  if (['checkbox', 'switch', 'menuitemcheckbox'].includes(role)) return 'checkbox:' + (this.getAttribute('aria-checked') === 'true');
  if (['radio', 'menuitemradio'].includes(role)) return 'radio:' + (this.getAttribute('aria-checked') === 'true');
  return 'element:' + this.localName;
}";

const ELEMENT_TEXT: &str = "function(limit) {
  const text = this.innerText ?? this.textContent ?? '';
  return [text.slice(0, limit), text.length > limit];
}";

const PAGE_TEXT: &str = "function(limit) {
  const text = document.body ? document.body.innerText : '';
  return [text.slice(0, limit), text.length > limit];
}";

const ATTRIBUTE: &str = "function(name) { return this.getAttribute(name); }";

const FIELD_VALUE: &str = "function() {
  if (!['input', 'textarea', 'select'].includes(this.localName)) return ['element', this.localName];
  if (this.localName === 'input' && this.type === 'password') return ['redacted'];
  return ['value', this.value];
}";

const TITLE: &str = "function() { return document.title; }";
const LOCATION: &str = "function() { return location.href; }";
const READY_STATE: &str = "function() { return document.readyState; }";
const HAS_TEXT: &str = "function(text) { return !!document.body && document.body.innerText.includes(text); }";
const VIEWPORT_CENTRE: &str = "function() { return [innerWidth / 2, innerHeight / 2]; }";
const IS_CONNECTED: &str = "function() { return this.isConnected; }";

fn string(value: &str) -> json::Value {
    json::Value::String(value.to_string())
}

fn number(value: f64) -> json::Value {
    json::Value::Number(json::Number::F64(value))
}

fn argument(value: json::Value) -> json::Value {
    let mut object = json::Object::new();
    object.insert("value".to_string(), value);
    json::Value::Object(object)
}

fn as_str(value: &json::Value) -> Option<&str> {
    match value {
        json::Value::String(s) => Some(s),
        _ => None,
    }
}

fn as_f64(value: &json::Value) -> Option<f64> {
    match value {
        json::Value::Number(json::Number::F64(n)) => Some(*n),
        json::Value::Number(json::Number::I64(n)) => Some(*n as f64),
        json::Value::Number(json::Number::U64(n)) => Some(*n as f64),
        _ => None,
    }
}

fn helper_reply(what: &str, value: &json::Value) -> ActionError {
    ActionError::Helper { reason: format!("{what} returned {}", json::to_string(value)) }
}

/// The centre of the first quad with an area, as `DOM.getContentQuads` lists them: four corners, clockwise.
fn centre(quads: &[Vec<f64>]) -> Option<(f64, f64)> {
    quads.iter().filter(|q| q.len() == 8).find_map(|q| {
        let area = ((q[0] * q[3] - q[2] * q[1]) + (q[2] * q[5] - q[4] * q[3]) + (q[4] * q[7] - q[6] * q[5]) + (q[6] * q[1] - q[0] * q[7])).abs() / 2.0;
        (area >= 1.0).then(|| ((q[0] + q[2] + q[4] + q[6]) / 4.0, (q[1] + q[3] + q[5] + q[7]) / 4.0))
    })
}

fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let value = |b: u8| match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index == bytes.len() / 4 - 1;
        let padding = chunk.iter().rev().take_while(|b| **b == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return None;
        }
        let mut word = 0u32;
        for b in &chunk[..4 - padding] {
            word = word << 6 | u32::from(value(*b)?);
        }
        word <<= 6 * padding as u32;
        out.extend_from_slice(&word.to_be_bytes()[1..4 - padding]);
    }
    Some(out)
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Doc {
    host: Host,
    session: SessionId,
    scope: DocumentScope,
}

#[derive(Clone)]
struct Held {
    session: SessionId,
    object: String,
}

pub(crate) struct Node {
    pub(crate) host: Host,
    pub(crate) scope: DocumentScope,
    held: Held,
    element: Option<ElementRef>,
}

impl Node {
    pub(crate) fn session(&self) -> &SessionId {
        &self.held.session
    }
}

struct DomSearch {
    kind: &'static str,
    value: String,
    limit: usize,
}

impl DomSearch {
    fn of(query: &Query, limit: usize) -> Option<DomSearch> {
        let (kind, value) = match query {
            Query::Css(css) => ("css", css.clone()),
            Query::Label(label) => ("label", normalized(label)),
            Query::Placeholder(text) => ("placeholder", normalized(text)),
            Query::TestId(id) => ("testid", id.clone()),
            Query::Role { .. } => return None,
        };
        Some(DomSearch { kind, value, limit })
    }
}

enum Handle {
    Object(String),
    Backend(i64),
}

struct Found {
    doc: usize,
    handle: Handle,
    summary: Option<String>,
}

enum Raced<T> {
    Replied(Result<T, CallError>),
    Dialog(Result<Dialog, SubscriptionEnd>),
}

pub(crate) struct Act<'a> {
    cdp: &'a Counted<'a>,
    target: &'a TargetScope,
    attachment: &'a mut Attachment,
    refs: &'a mut RefTable,
    hosts: HashMap<Host, HostFrames>,
    worlds: HashMap<(SessionId, FrameId), i64>,
    holding: HashSet<SessionId>,
}

impl<'a> Act<'a> {
    pub(crate) fn new(cdp: &'a Counted<'a>, target: &'a TargetScope, attachment: &'a mut Attachment, refs: &'a mut RefTable) -> Act<'a> {
        Act { cdp, target, attachment, refs, hosts: HashMap::new(), worlds: HashMap::new(), holding: HashSet::new() }
    }

    fn lost(&self, error: CallError) -> ActionError {
        match error {
            CallError::SessionDetached { session, .. } if session == self.attachment.page => ActionError::Attach(AttachError::Detached),
            error => ActionError::Cdp(error),
        }
    }

    async fn call<C: Command>(&self, session: &SessionId, command: &C) -> Result<C::Returns, ActionError> {
        self.cdp.session(session, command).await.map_err(|e| self.lost(e))
    }

    /// A dialog pauses the page, which then answers nothing until the dialog closes, so a mutating command
    /// fails as soon as one opens.
    async fn dispatch<C: Command>(&mut self, session: &SessionId, command: &C) -> Result<C::Returns, ActionError> {
        let cdp = self.cdp;
        let dialogs = &mut self.attachment.dialogs;
        let watch = async {
            loop {
                match dialogs.next().await {
                    Ok(event) => {
                        if let Ok(opening) = event.decode::<DialogOpening>() {
                            return Raced::Dialog(Ok(Dialog::from(opening)));
                        }
                    }
                    Err(end) => return Raced::Dialog(Err(end)),
                }
            }
        };
        let raced = future::or(async { Raced::Replied(cdp.session(session, command).await) }, watch).await;
        match raced {
            Raced::Replied(result) => result.map_err(|e| self.lost(e)),
            Raced::Dialog(Ok(dialog)) => {
                self.attachment.dialog = Some(dialog.clone());
                Err(ActionError::DialogOpen(dialog))
            }
            Raced::Dialog(Err(end)) => Err(subscription_end(end)),
        }
    }

    pub(crate) async fn release(&mut self) {
        for session in std::mem::take(&mut self.holding) {
            let _released_or_left_for_the_next_action = self.cdp.session(&session, &ReleaseObjectGroup { object_group: OBJECT_GROUP.into() }).await;
        }
    }

    fn forget_frames(&mut self) {
        self.hosts.clear();
        self.worlds.clear();
    }

    async fn host_frames(&mut self, host: &Host) -> Result<Option<&HostFrames>, ActionError> {
        if !self.hosts.contains_key(host) {
            let Some(session) = self.attachment.session_of(host).cloned() else { return Ok(None) };
            match frames(self.cdp, &session).await {
                Ok(frames) => {
                    self.hosts.insert(host.clone(), frames);
                }
                Err(CallError::SessionDetached { .. }) if *host != Host::Page => return Ok(None),
                Err(error) => return Err(self.lost(error)),
            }
        }
        Ok(self.hosts.get(host))
    }

    fn hosts_in_order(&self) -> Vec<Host> {
        let mut oopifs: Vec<&TargetId> = self.attachment.oopifs.keys().collect();
        oopifs.sort_by_key(|t| t.as_str());
        std::iter::once(Host::Page).chain(oopifs.into_iter().map(|t| Host::Oopif(t.clone()))).collect()
    }

    async fn documents(&mut self) -> Result<Vec<Doc>, ActionError> {
        let mut docs = Vec::new();
        for host in self.hosts_in_order() {
            let target = self.target.clone();
            let Some(frames) = self.host_frames(&host).await? else { continue };
            for frame in &frames.preorder {
                let scope = DocumentScope { target: target.clone(), frame: frame.clone(), document: frames.documents[frame].clone() };
                docs.push(Doc { host: host.clone(), session: frames.session.clone(), scope });
            }
        }
        docs.truncate(MAX_DOCUMENTS);
        Ok(docs)
    }

    async fn world(&mut self, session: &SessionId, frame: &FrameId) -> Result<i64, ActionError> {
        let key = (session.clone(), frame.clone());
        if let Some(context) = self.worlds.get(&key) {
            return Ok(*context);
        }
        let context = self.call(session, &CreateIsolatedWorld { frame_id: frame.clone(), world_name: WORLD.into() }).await?.execution_context_id;
        self.worlds.insert(key, context);
        Ok(context)
    }

    async fn main_world(&mut self) -> Result<(SessionId, i64), ActionError> {
        let page = self.attachment.page.clone();
        let Some(frames) = self.host_frames(&Host::Page).await? else { unreachable!("the page always has a session") };
        let root = frames.root.clone();
        let context = self.world(&page, &root).await?;
        Ok((page, context))
    }

    fn value(&self, what: &str, evaluated: Evaluated) -> Result<json::Value, ActionError> {
        if let Some(exception) = evaluated.exception_details {
            return Err(ActionError::Helper { reason: format!("{what}: {}", exception.message()) });
        }
        Ok(evaluated.result.value.unwrap_or(json::Value::Null))
    }

    async fn inspect(&mut self, held: &Held, what: &str, function: &str, arguments: Vec<json::Value>) -> Result<json::Value, ActionError> {
        let command = InspectObject {
            on: held.object.clone(),
            function_declaration: function.into(),
            arguments: arguments.into_iter().map(argument).collect(),
            return_by_value: true,
            await_promise: false,
            object_group: OBJECT_GROUP.into(),
        };
        let evaluated = self.call(&held.session, &command).await?;
        self.value(what, evaluated)
    }

    async fn change(&mut self, held: &Held, what: &str, function: &str, arguments: Vec<json::Value>) -> Result<json::Value, ActionError> {
        let command = ChangeObject {
            on: held.object.clone(),
            function_declaration: function.into(),
            arguments: arguments.into_iter().map(argument).collect(),
            return_by_value: true,
            await_promise: false,
            object_group: OBJECT_GROUP.into(),
        };
        let evaluated = self.dispatch(&held.session, &command).await?;
        self.value(what, evaluated)
    }

    async fn inspect_in(&mut self, session: &SessionId, context: i64, what: &str, function: &str, arguments: Vec<json::Value>) -> Result<json::Value, ActionError> {
        let command = InspectContext {
            on: context,
            function_declaration: function.into(),
            arguments: arguments.into_iter().map(argument).collect(),
            return_by_value: true,
            await_promise: false,
            object_group: OBJECT_GROUP.into(),
        };
        let evaluated = self.call(session, &command).await?;
        self.value(what, evaluated)
    }

    async fn object_in(&mut self, session: &SessionId, context: i64, function: &str, arguments: Vec<json::Value>) -> Result<Evaluated, ActionError> {
        let command = InspectContext {
            on: context,
            function_declaration: function.into(),
            arguments: arguments.into_iter().map(argument).collect(),
            return_by_value: false,
            await_promise: false,
            object_group: OBJECT_GROUP.into(),
        };
        self.holding.insert(session.clone());
        self.call(session, &command).await
    }

    async fn resolve_in(&mut self, session: &SessionId, frame: &FrameId, backend: i64) -> Result<Option<String>, ActionError> {
        let context = self.world(session, frame).await?;
        self.holding.insert(session.clone());
        let command = ResolveNodeIn { backend_node_id: backend, execution_context_id: context, object_group: OBJECT_GROUP.into() };
        match self.call(session, &command).await {
            Ok(resolved) => Ok(resolved.object.object_id),
            Err(ActionError::Cdp(CallError::Protocol { .. })) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn resolve_ref(&mut self, element: ElementRef) -> Result<(Node, BackendNodeId), ActionError> {
        let stale = |staleness| ActionError::Ref(RefError::Stale { element, staleness });
        let Some(entry) = self.refs.get(element).cloned() else { return Err(stale(Staleness::Unknown)) };
        if entry.key.document.target != *self.target {
            return Err(stale(Staleness::OtherTarget(entry.key.document.target.target.clone())));
        }
        let scope = entry.key.document.clone();
        let current = match self.host_frames(&entry.host).await? {
            None => None,
            Some(frames) => Some((frames.session.clone(), frames.documents.get(&scope.frame).cloned())),
        };
        let staleness = match &current {
            None | Some((_, None)) => Some(Staleness::FrameGone),
            Some((_, Some(document))) if *document != scope.document => Some(Staleness::Navigated),
            Some(_) => None,
        };
        if let Some(staleness) = staleness {
            self.refs.forget(element);
            return Err(stale(staleness));
        }
        let session = current.expect("the frame is current").0;
        let object = self.resolve_in(&session, &scope.frame, entry.key.node.0).await?;
        let node = object.map(|object| Node { host: entry.host.clone(), scope, held: Held { session: session.clone(), object }, element: Some(element) });
        let connected = match &node {
            Some(node) => matches!(self.inspect(&node.held, "isConnected", IS_CONNECTED, Vec::new()).await?, json::Value::Bool(true)),
            None => false,
        };
        match node {
            Some(node) if connected => Ok((node, entry.key.node)),
            _ => {
                self.refs.forget(element);
                Err(stale(Staleness::NodeGone))
            }
        }
    }

    async fn search(&mut self, query: &Query, limit: usize) -> Result<(Vec<Doc>, Vec<Found>, usize), ActionError> {
        let docs = self.documents().await?;
        let mut per_doc: Vec<Vec<Found>> = Vec::new();
        let mut total = 0;
        for (index, doc) in docs.iter().enumerate() {
            let found = match (query, DomSearch::of(query, limit)) {
                (Query::Role { role, name }, _) => self.search_accessibility(index, doc, role, name.as_deref()).await?,
                (_, None) => Vec::new(),
                (_, Some(search)) => {
                    let context = match self.world(&doc.session, &doc.scope.frame).await {
                        Ok(context) => context,
                        Err(ActionError::Cdp(CallError::Protocol { .. })) => {
                            per_doc.push(Vec::new());
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let (found, matches) = self.search_dom(index, doc, context, query, &search).await?;
                    total += matches;
                    found
                }
            };
            per_doc.push(found);
        }
        if let Query::Role { .. } = query {
            let mut seen: HashSet<(SessionId, i64)> = HashSet::new();
            for (index, found) in per_doc.iter_mut().enumerate().rev() {
                found.retain(|f| match f.handle {
                    Handle::Backend(backend) => seen.insert((docs[index].session.clone(), backend)),
                    Handle::Object(_) => true,
                });
            }
            total = per_doc.iter().map(Vec::len).sum();
        }
        Ok((docs, per_doc.into_iter().flatten().collect(), total))
    }

    /// `Accessibility.queryAXTree` never answers in a hidden tab; the full tree does, and stops at child frames.
    async fn search_accessibility(&mut self, index: usize, doc: &Doc, role: &str, name: Option<&str>) -> Result<Vec<Found>, ActionError> {
        let tree = match self.call(&doc.session, &GetFullAxTree { frame_id: doc.scope.frame.clone() }).await {
            Ok(tree) => tree,
            Err(ActionError::Cdp(CallError::Protocol { .. })) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let name = name.map(normalized);
        let text = |value: Option<&crate::commands::AxValue>| match value.and_then(|v| v.value.as_ref()) {
            Some(json::Value::String(s)) => normalized(s),
            _ => String::new(),
        };
        let mut found = Vec::new();
        for node in tree.nodes.iter().filter(|n| !n.ignored) {
            let Some(backend) = node.backend_dom_node_id else { continue };
            if text(node.role.as_ref()) != role {
                continue;
            }
            let computed = text(node.name.as_ref());
            if name.as_ref().is_some_and(|wanted| *wanted != computed) {
                continue;
            }
            let shown = if computed.is_empty() { String::new() } else { format!(" {}", json::to_string(&computed)) };
            found.push(Found { doc: index, handle: Handle::Backend(backend), summary: Some(format!("{role}{shown}")) });
        }
        Ok(found)
    }

    async fn search_dom(&mut self, index: usize, doc: &Doc, context: i64, query: &Query, search: &DomSearch) -> Result<(Vec<Found>, usize), ActionError> {
        let arguments = vec![string(search.kind), string(&search.value), number(search.limit as f64)];
        let evaluated = self.object_in(&doc.session, context, SEARCH, arguments).await?;
        if let Some(exception) = evaluated.exception_details {
            return Err(match query {
                Query::Css(_) => ActionError::InvalidLocator { locator: query.to_string(), reason: exception.message() },
                _ => ActionError::Helper { reason: exception.message() },
            });
        }
        let Some(array) = evaluated.result.object_id else { return Err(ActionError::Helper { reason: "the search returned no list".into() }) };
        let properties = self.call(&doc.session, &GetOwnProperties { object_id: array, own_properties: true }).await?.result;
        let mut items: Vec<(usize, String)> = Vec::new();
        let mut matches = None;
        for property in properties {
            let Some(value) = property.value else { continue };
            if property.name == "total" {
                matches = value.value.as_ref().and_then(as_f64).map(|n| n as usize);
            } else if let (Ok(position), Some(object)) = (property.name.parse::<usize>(), value.object_id) {
                items.push((position, object));
            }
        }
        items.sort_unstable_by_key(|(position, _)| *position);
        let matches = matches.ok_or_else(|| ActionError::Helper { reason: "the search returned no count".into() })?;
        Ok((items.into_iter().map(|(_, object)| Found { doc: index, handle: Handle::Object(object), summary: None }).collect(), matches))
    }

    async fn candidate(&mut self, docs: &[Doc], found: &Found, counter: &dyn RefCounter) -> Result<Candidate, ActionError> {
        let doc = &docs[found.doc];
        let backend = match &found.handle {
            Handle::Backend(backend) => *backend,
            Handle::Object(object) => self.call(&doc.session, &DescribeObject { object_id: object.clone() }).await?.node.backend_node_id,
        };
        let summary = match (&found.summary, &found.handle) {
            (Some(summary), _) => summary.clone(),
            (None, Handle::Object(object)) => {
                let held = Held { session: doc.session.clone(), object: object.clone() };
                let described = self.inspect(&held, "describe", DESCRIBE, Vec::new()).await?;
                as_str(&described).ok_or_else(|| helper_reply("describe", &described))?.to_string()
            }
            (None, Handle::Backend(_)) => String::new(),
        };
        let key = NodeKey { document: doc.scope.clone(), node: BackendNodeId(backend) };
        let element = self.refs.issue(key, doc.host.clone(), counter).map_err(ActionError::Counter)?;
        Ok(Candidate { element, frame: doc.scope.frame.clone(), summary })
    }

    async fn locate(&mut self, locator: &Locator, counter: &dyn RefCounter) -> Result<Node, ActionError> {
        let (query, occurrence) = match locator {
            Locator::Ref(element) => return Ok(self.resolve_ref(*element).await?.0),
            Locator::Query { query, occurrence } => (query, occurrence),
        };
        let limit = occurrence.map_or(MAX_CANDIDATES, |n| n.0.get().max(MAX_CANDIDATES));
        let (docs, mut found, matches) = self.search(query, limit).await?;
        let index = match occurrence {
            None if matches == 1 => 0,
            None if matches == 0 => return Err(ActionError::NotFound { locator: locator.to_string(), matches }),
            None => {
                let mut candidates = Vec::new();
                for f in found.iter().take(MAX_CANDIDATES) {
                    candidates.push(self.candidate(&docs, f, counter).await?);
                }
                return Err(ActionError::Ambiguous { locator: locator.to_string(), matches, candidates });
            }
            Some(n) if n.0.get() <= found.len() => n.0.get() - 1,
            Some(_) => return Err(ActionError::NotFound { locator: locator.to_string(), matches }),
        };
        let chosen = found.swap_remove(index);
        let doc = &docs[chosen.doc];
        let object = match chosen.handle {
            Handle::Object(object) => object,
            Handle::Backend(backend) => match self.resolve_in(&doc.session.clone(), &doc.scope.frame.clone(), backend).await? {
                Some(object) => object,
                None => return Err(ActionError::Detached { locator: locator.to_string() }),
            },
        };
        Ok(Node { host: doc.host.clone(), scope: doc.scope.clone(), held: Held { session: doc.session.clone(), object }, element: None })
    }

    async fn actionable(&mut self, node: &Node, locator: &Locator, needs_enabled: bool) -> Result<(), ActionError> {
        let state = self.inspect(&node.held, "the actionability check", ACTIONABLE, vec![json::Value::Bool(needs_enabled)]).await?;
        let locator = locator.to_string();
        match as_str(&state) {
            Some("ok") => Ok(()),
            Some("detached") => Err(ActionError::Detached { locator }),
            Some("hidden") => Err(ActionError::NotVisible { locator, reason: "its style hides it (display, visibility, or content-visibility)".into() }),
            Some("empty") => Err(ActionError::NotVisible { locator, reason: "it has no size".into() }),
            Some("disabled") => Err(ActionError::Disabled { locator }),
            Some("not-element") => Err(ActionError::NotActionable { locator, reason: "is not an element".into() }),
            _ => Err(helper_reply("the actionability check", &state)),
        }
    }

    async fn hit(&mut self, held: &Held, (x, y): (f64, f64), locator: &Locator, frame_owner: bool) -> Result<(), ActionError> {
        let result = self.inspect(held, "the hit test", HIT, vec![number(x), number(y)]).await?;
        let locator = locator.to_string();
        match as_str(&result) {
            Some("hit") => Ok(()),
            Some("outside") if frame_owner => Err(ActionError::NotVisible { locator, reason: "its frame is scrolled out of view".into() }),
            Some("outside") => Err(ActionError::NotVisible { locator, reason: "it is outside the viewport even after scrolling".into() }),
            Some(covered) if covered.starts_with("covered:") => {
                let by = covered["covered:".len()..].to_string();
                Err(ActionError::Covered { locator, by: if frame_owner { format!("{by}, over its frame") } else { by } })
            }
            _ => Err(helper_reply("the hit test", &result)),
        }
    }

    async fn host_of(&mut self, frame: &FrameId) -> Result<Option<Host>, ActionError> {
        for host in self.hosts_in_order() {
            if let Some(frames) = self.host_frames(&host).await?
                && frames.documents.contains_key(frame)
            {
                return Ok(Some(host));
            }
        }
        Ok(None)
    }

    async fn frame_owner(&mut self, parent_session: &SessionId, frame: &FrameId) -> Result<i64, ActionError> {
        Ok(self.call(parent_session, &GetFrameOwner { frame_id: frame.clone() }).await?.backend_node_id)
    }

    /// Where the viewport of `frame` starts within its session's local root frame. Quads and box models a
    /// session reports are in its local root's coordinates; DOM hit tests in a child frame take the frame's own.
    async fn origin(&mut self, host: &Host, frame: &FrameId, locator: &Locator) -> Result<(f64, f64), ActionError> {
        let Some(frames) = self.host_frames(host).await? else { return Err(ActionError::Detached { locator: locator.to_string() }) };
        if frames.root == *frame {
            return Ok((0.0, 0.0));
        }
        let session = frames.session.clone();
        let owner = self.frame_owner(&session, frame).await?;
        self.content_origin(&session, owner, locator).await
    }

    async fn content_origin(&mut self, session: &SessionId, owner: i64, locator: &Locator) -> Result<(f64, f64), ActionError> {
        match self.call(session, &GetBoxModel { backend_node_id: owner }).await {
            Ok(reply) if reply.model.content.len() == 8 => Ok((reply.model.content[0], reply.model.content[1])),
            Ok(_) | Err(ActionError::Cdp(CallError::Protocol { .. })) => {
                Err(ActionError::NotVisible { locator: locator.to_string(), reason: "its frame is not displayed".into() })
            }
            Err(error) => Err(error),
        }
    }

    async fn hit_point(&mut self, node: &Node, locator: &Locator) -> Result<(f64, f64), ActionError> {
        let not_visible = |reason: &str| ActionError::NotVisible { locator: locator.to_string(), reason: reason.to_string() };
        let detached = || ActionError::Detached { locator: locator.to_string() };
        match self.dispatch(&node.held.session, &ScrollIntoView { object_id: node.held.object.clone() }).await {
            Ok(_) => {}
            Err(ActionError::Cdp(CallError::Protocol { .. })) => return Err(not_visible("it cannot be scrolled into view")),
            Err(error) => return Err(error),
        }
        let settled = InspectObject {
            on: node.held.object.clone(),
            function_declaration: SETTLED.into(),
            arguments: Vec::new(),
            return_by_value: true,
            await_promise: true,
            object_group: OBJECT_GROUP.into(),
        };
        let settled = self.call(&node.held.session, &settled).await?;
        match self.value("the settle check", settled)? {
            json::Value::String(s) if s == "settled" => {}
            json::Value::String(s) if s == "moving" => {
                return Err(ActionError::NotActionable { locator: locator.to_string(), reason: "kept moving for 10 frames".into() });
            }
            other => return Err(helper_reply("the settle check", &other)),
        }
        let quads = match self.call(&node.held.session, &GetContentQuads { object_id: node.held.object.clone() }).await {
            Ok(quads) => quads.quads,
            Err(ActionError::Cdp(CallError::Protocol { .. })) => return Err(not_visible("it has no layout box")),
            Err(error) => return Err(error),
        };
        let (mut x, mut y) = centre(&quads).ok_or_else(|| not_visible("it has no area on screen"))?;
        let mut host = node.host.clone();
        let mut frame = node.scope.frame.clone();
        let (ox, oy) = self.origin(&host, &frame, locator).await?;
        self.hit(&node.held, (x - ox, y - oy), locator, false).await?;
        loop {
            let Some(frames) = self.host_frames(&host).await? else { return Err(detached()) };
            let Some(parent) = frames.parents.get(&frame).cloned().flatten() else {
                if host == Host::Page {
                    return Ok((x, y));
                }
                return Err(ActionError::Helper { reason: format!("the out-of-process frame {frame} reports no parent frame") });
            };
            let in_process = frames.documents.contains_key(&parent);
            let session = frames.session.clone();
            let (parent_host, parent_session, owner) = if in_process {
                let owner = self.frame_owner(&session, &frame).await?;
                (host.clone(), session, owner)
            } else {
                let parent_host = self.host_of(&parent).await?.ok_or_else(detached)?;
                let parent_session = self.attachment.session_of(&parent_host).cloned().ok_or_else(detached)?;
                let owner = self.frame_owner(&parent_session, &frame).await?;
                let (cx, cy) = self.content_origin(&parent_session, owner, locator).await?;
                x += cx;
                y += cy;
                (parent_host, parent_session, owner)
            };
            let (ox, oy) = self.origin(&parent_host, &parent, locator).await?;
            let Some(object) = self.resolve_in(&parent_session, &parent, owner).await? else { return Err(detached()) };
            self.hit(&Held { session: parent_session, object }, (x - ox, y - oy), locator, true).await?;
            host = parent_host;
            frame = parent;
        }
    }

    async fn mouse(&mut self, kind: &'static str, x: f64, y: f64, pressed: bool, click_count: u32) -> Result<(), ActionError> {
        let page = self.attachment.page.clone();
        let button = if kind == "mouseMoved" { "none" } else { "left" };
        let event = DispatchMouse { kind, x, y, button, buttons: u32::from(pressed), click_count };
        self.dispatch(&page, &event).await.map(drop)
    }

    async fn click(&mut self, node: &Node, (x, y): (f64, f64), count: ClickCount, locator: &Locator) -> Result<(), ActionError> {
        let Some(page_frames) = self.host_frames(&Host::Page).await? else { unreachable!("the page always has a session") };
        if !(node.host == Host::Page && node.scope.frame == page_frames.root) {
            let age = self.inspect(&node.held, "the frame's age", DOCUMENT_AGE, Vec::new()).await?;
            let age = as_f64(&age).ok_or_else(|| helper_reply("the frame's age", &age))?;
            if let Some(wait) = FRAME_INPUT_GRACE.checked_sub(Duration::from_secs_f64(age.max(0.0) / 1000.0)) {
                Timer::after(wait).await;
            }
        }
        let armed = self.inspect(&node.held, "arming the press check", ARM_PRESS, Vec::new()).await?;
        if as_str(&armed) != Some("armed") {
            return Err(helper_reply("arming the press check", &armed));
        }
        self.mouse("mouseMoved", x, y, false, 0).await?;
        self.mouse("mousePressed", x, y, true, 1).await?;
        let landed = self.inspect(&node.held, "the press check", PRESS_LANDED, Vec::new()).await;
        self.mouse("mouseReleased", x, y, false, 1).await?;
        let landed = landed?;
        match as_str(&landed) {
            Some("landed") => {}
            Some("nothing") => {
                return Err(ActionError::NotActionable {
                    locator: locator.to_string(),
                    reason: "did not get the click: the browser delivered no press to its frame".into(),
                });
            }
            Some(elsewhere) if elsewhere.starts_with("elsewhere:") => {
                return Err(ActionError::Missed { locator: locator.to_string(), by: elsewhere["elsewhere:".len()..].to_string() });
            }
            _ => return Err(helper_reply("the press check", &landed)),
        }
        if count == ClickCount::Double {
            self.mouse("mousePressed", x, y, true, 2).await?;
            self.mouse("mouseReleased", x, y, false, 2).await?;
        }
        Ok(())
    }

    async fn key_event(&mut self, kind: &'static str, key: &Key, modifiers: u32, text: &str) -> Result<(), ActionError> {
        let page = self.attachment.page.clone();
        let event = DispatchKey {
            kind,
            modifiers,
            key: key.key.clone(),
            code: key.code.clone(),
            windows_virtual_key_code: key.virtual_code,
            text: text.to_string(),
            unmodified_text: text.to_string(),
        };
        self.dispatch(&page, &event).await.map(drop)
    }

    async fn press_chord(&mut self, chord: &Chord) -> Result<(), ActionError> {
        let mut held = 0;
        for modifier in &chord.modifiers {
            held |= modifier.bit();
            self.key_event("rawKeyDown", &modifier.key(), held, "").await?;
        }
        let modifiers = chord.modifier_bits();
        let text = chord.typed_text().to_string();
        self.key_event(if text.is_empty() { "rawKeyDown" } else { "keyDown" }, &chord.key, modifiers, &text).await?;
        self.key_event("keyUp", &chord.key, modifiers, "").await?;
        for modifier in chord.modifiers.iter().rev() {
            held &= !modifier.bit();
            self.key_event("keyUp", &modifier.key(), held, "").await?;
        }
        Ok(())
    }

    async fn insert(&mut self, text: &str) -> Result<(), ActionError> {
        let page = self.attachment.page.clone();
        self.dispatch(&page, &InsertText { text: text.to_string() }).await.map(drop)
    }

    async fn type_text(&mut self, text: &str) -> Result<(), ActionError> {
        let mut inserting = String::new();
        for c in text.chars() {
            match keys::of_char(c).filter(|_| c.is_ascii()) {
                Some(key) => {
                    if !inserting.is_empty() {
                        self.insert(&std::mem::take(&mut inserting)).await?;
                    }
                    self.press_chord(&Chord { modifiers: Vec::new(), key }).await?;
                }
                None => inserting.push(c),
            }
        }
        if !inserting.is_empty() {
            self.insert(&inserting).await?;
        }
        Ok(())
    }

    async fn focus_end(&mut self, node: &Node, locator: &Locator) -> Result<(), ActionError> {
        let focused = self.change(&node.held, "focus", FOCUS_END, Vec::new()).await?;
        match as_str(&focused) {
            Some("focused") => Ok(()),
            Some("unfocusable") => Err(ActionError::NotActionable { locator: locator.to_string(), reason: "cannot take keyboard focus".into() }),
            _ => Err(helper_reply("focus", &focused)),
        }
    }

    async fn checked_state(&mut self, node: &Node, locator: &Locator) -> Result<(bool, bool), ActionError> {
        let state = self.inspect(&node.held, "the checked state", CHECKED_STATE, Vec::new()).await?;
        let text = as_str(&state).ok_or_else(|| helper_reply("the checked state", &state))?;
        match text.split_once(':') {
            Some(("checkbox", checked)) => Ok((false, checked == "true")),
            Some(("radio", checked)) => Ok((true, checked == "true")),
            Some(("element", name)) => Err(ActionError::NotActionable { locator: locator.to_string(), reason: format!("is a <{name}>, not a checkbox, radio, or switch") }),
            _ => Err(helper_reply("the checked state", &state)),
        }
    }

    async fn read_text(&mut self, node: Option<&Node>) -> Result<Outcome, ActionError> {
        let limit = vec![number(MAX_READ_CHARS as f64)];
        let read = match node {
            Some(node) => self.inspect(&node.held, "text", ELEMENT_TEXT, limit).await?,
            None => {
                let (session, context) = self.main_world().await?;
                self.inspect_in(&session, context, "text", PAGE_TEXT, limit).await?
            }
        };
        match &read {
            json::Value::Array(pair) => match pair.as_slice() {
                [json::Value::String(text), json::Value::Bool(truncated)] => Ok(Outcome::Text { text: text.clone(), truncated: *truncated }),
                _ => Err(helper_reply("text", &read)),
            },
            _ => Err(helper_reply("text", &read)),
        }
    }

    async fn main_string(&mut self, what: &str, function: &str) -> Result<String, ActionError> {
        let (session, context) = self.main_world().await?;
        let value = self.inspect_in(&session, context, what, function, Vec::new()).await?;
        as_str(&value).map(str::to_string).ok_or_else(|| helper_reply(what, &value))
    }

    async fn holds(&mut self, condition: &Condition) -> Result<Option<Outcome>, ActionError> {
        Ok(match condition {
            Condition::Present(query) => Some(self.search(query, 1).await?.2).filter(|n| *n > 0).map(Outcome::Matched),
            Condition::Absent(query) => (self.search(query, 1).await?.2 == 0).then_some(Outcome::Matched(0)),
            Condition::Text(text) => {
                let (session, context) = self.main_world().await?;
                let found = self.inspect_in(&session, context, "text", HAS_TEXT, vec![string(text)]).await?;
                matches!(found, json::Value::Bool(true)).then_some(Outcome::Done)
            }
            Condition::Url(pattern) => glob(pattern, &self.main_string("the URL", LOCATION).await?).then_some(Outcome::Done),
            Condition::Loaded(state) => {
                let ready = self.main_string("the ready state", READY_STATE).await?;
                let loaded = match state {
                    LoadState::DomContentLoaded => ready != "loading",
                    LoadState::Load => ready == "complete",
                };
                loaded.then_some(Outcome::Done)
            }
        })
    }

    async fn wait(&mut self, condition: &Condition, timeout: Duration, interrupt: &dyn Interrupt) -> Result<Outcome, ActionError> {
        let started = Instant::now();
        let deadline = started + timeout;
        let waiting_for = || match condition {
            Condition::Present(query) => format!("an element matching {query}"),
            Condition::Absent(query) => format!("no element matching {query}"),
            Condition::Text(text) => format!("the text {}", json::to_string(text)),
            Condition::Url(pattern) => format!("a URL matching {}", json::to_string(pattern)),
            Condition::Loaded(LoadState::DomContentLoaded) => "DOMContentLoaded".to_string(),
            Condition::Loaded(LoadState::Load) => "the load event".to_string(),
        };
        loop {
            if let Some(dialog) = self.attachment.current_dialog().await.map_err(subscription_end)? {
                return Err(ActionError::DialogOpen(dialog));
            }
            let held = match self.holds(condition).await {
                Ok(held) => held,
                Err(ActionError::Cdp(CallError::Protocol { .. })) => None,
                Err(error) => return Err(error),
            };
            if let Some(outcome) = held {
                return Ok(outcome);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(ActionError::Timeout { waiting_for: waiting_for(), waited: now - started });
            }
            if interrupt.interrupted() {
                return Err(ActionError::Interrupted { waiting_for: waiting_for(), waited: now - started });
            }
            Timer::at(deadline.min(now + POLL)).await;
            self.release().await;
            self.forget_frames();
            self.attachment.sync(self.cdp).await?;
        }
    }

    async fn navigate(&mut self, url: &str, readiness: Readiness, timeout: Duration, interrupt: &dyn Interrupt) -> Result<Outcome, ActionError> {
        let started = Instant::now();
        let deadline = started + timeout;
        let page = self.attachment.page.clone();
        let filter = EventFilter::new(&[LifecycleEvent::METHOD, DetachedFromTarget::METHOD], EventScope::Any);
        let mut events = self.cdp.connection.subscribe(filter).map_err(ActionError::Cdp)?;
        let navigated = self.dispatch(&page, &Navigate { url: url.to_string() }).await?;
        if let Some(reason) = navigated.error_text.filter(|e| !e.is_empty()) {
            return Err(ActionError::NavigationFailed { url: SafeUrl::new(url), reason });
        }
        let (Some(loader), false) = (navigated.loader_id, readiness == Readiness::Commit) else { return Ok(Outcome::Done) };
        let wanted = readiness.lifecycle_name();
        let awaited = || format!("the {wanted} event of the navigation to {}", SafeUrl::new(url));
        loop {
            let next = future::or(async { Some(events.next().await) }, async {
                Timer::at(deadline.min(Instant::now() + POLL)).await;
                None
            });
            let dialogs = &mut self.attachment.dialogs;
            let opened = async {
                loop {
                    match dialogs.next().await {
                        Ok(event) => {
                            if let Ok(opening) = event.decode::<DialogOpening>() {
                                return Err(Ok(Dialog::from(opening)));
                            }
                        }
                        Err(end) => return Err(Err(end)),
                    }
                }
            };
            let raced: Result<Option<_>, Result<Dialog, SubscriptionEnd>> = future::or(async { Ok(next.await) }, opened).await;
            let event = match raced {
                Ok(Some(Ok(event))) => event,
                Ok(Some(Err(end))) => return Err(subscription_end(end)),
                Ok(None) if Instant::now() >= deadline => return Err(ActionError::Timeout { waiting_for: awaited(), waited: started.elapsed() }),
                Ok(None) if interrupt.interrupted() => return Err(ActionError::Interrupted { waiting_for: awaited(), waited: started.elapsed() }),
                Ok(None) => continue,
                Err(Ok(dialog)) => {
                    self.attachment.dialog = Some(dialog.clone());
                    return Err(ActionError::DialogOpen(dialog));
                }
                Err(Err(end)) => return Err(subscription_end(end)),
            };
            if let Ok(detached) = event.decode::<DetachedFromTarget>() {
                if detached.session_id == page {
                    return Err(ActionError::Attach(AttachError::Detached));
                }
            } else if event.session() == Some(&page)
                && let Ok(lifecycle) = event.decode::<LifecycleEvent>()
                && lifecycle.frame_id == navigated.frame_id
                && lifecycle.loader_id == loader
                && lifecycle.name == wanted
            {
                return Ok(Outcome::Done);
            }
        }
    }

    async fn fill(&mut self, node: &Node, locator: &Locator, text: &str) -> Result<(), ActionError> {
        let prepared = self.change(&node.held, "fill", FILL_PREPARE, Vec::new()).await?;
        let not = |reason: String| ActionError::NotActionable { locator: locator.to_string(), reason };
        match as_str(&prepared) {
            Some("text") if text.is_empty() => {
                let delete = Chord::parse("Delete").expect("Delete is a key");
                self.press_chord(&delete).await
            }
            Some("text") => self.insert(text).await,
            Some("value") => {
                let set = self.change(&node.held, "fill", SET_VALUE, vec![string(text)]).await?;
                match as_str(&set) {
                    Some("ok") => Ok(()),
                    Some("rejected") => Err(not(format!("does not accept the value {}", json::to_string(text)))),
                    _ => Err(helper_reply("fill", &set)),
                }
            }
            Some("readonly") => Err(not("is read-only".into())),
            Some(other) if other.starts_with("type:") => Err(not(format!("is an <input type={}>, which takes no text", &other[5..]))),
            Some(other) if other.starts_with("element:") => {
                Err(not(format!("is a <{}>, not an input, textarea, or contenteditable element", &other[8..])))
            }
            _ => Err(helper_reply("fill", &prepared)),
        }
    }

    async fn select(&mut self, node: &Node, locator: &Locator, values: &[String]) -> Result<Vec<String>, ActionError> {
        let wanted = json::Value::Array(values.iter().map(String::as_str).map(string).collect());
        let result = self.change(&node.held, "select", SELECT, vec![wanted]).await?;
        let not = |reason: String| ActionError::NotActionable { locator: locator.to_string(), reason };
        match &result {
            json::Value::Array(chosen) => chosen.iter().map(|v| as_str(v).map(str::to_string).ok_or_else(|| helper_reply("select", &result))).collect(),
            json::Value::String(error) => Err(match error.split_once(':') {
                Some(("element", name)) => not(format!("is a <{name}>, not a <select>")),
                Some(("missing", value)) => {
                    ActionError::NotFound { locator: format!("an option with value or label {} in {locator}", json::to_string(value)), matches: 0 }
                }
                Some(("disabled", value)) => not(format!("has option {} disabled", json::to_string(value))),
                None if error == "single" => not("takes one option, not several".into()),
                _ => helper_reply("select", &result),
            }),
            _ => Err(helper_reply("select", &result)),
        }
    }

    async fn run(&mut self, action: &Action, counter: &dyn RefCounter, interrupt: &dyn Interrupt) -> Result<(Option<Node>, Outcome), ActionError> {
        if action.sends_input() {
            let page = self.attachment.page.clone();
            self.dispatch(&page, &BringToFront {}).await?;
        }
        match action {
            Action::Navigate { url, readiness, timeout } => Ok((None, self.navigate(url, *readiness, *timeout, interrupt).await?)),
            Action::Click { locator, count } => {
                let node = self.locate(locator, counter).await?;
                self.actionable(&node, locator, true).await?;
                let point = self.hit_point(&node, locator).await?;
                self.click(&node, point, *count, locator).await?;
                Ok((Some(node), Outcome::Done))
            }
            Action::Hover(locator) => {
                let node = self.locate(locator, counter).await?;
                self.actionable(&node, locator, false).await?;
                let (x, y) = self.hit_point(&node, locator).await?;
                self.mouse("mouseMoved", x, y, false, 0).await?;
                Ok((Some(node), Outcome::Done))
            }
            Action::Fill { locator, text } => {
                let node = self.locate(locator, counter).await?;
                self.actionable(&node, locator, true).await?;
                self.fill(&node, locator, text).await?;
                Ok((Some(node), Outcome::Done))
            }
            Action::Type { into, text } => {
                let node = self.focus_target(into.as_ref(), counter).await?;
                self.type_text(text).await?;
                Ok((node, Outcome::Done))
            }
            Action::Press { into, chord } => {
                let node = self.focus_target(into.as_ref(), counter).await?;
                self.press_chord(chord).await?;
                Ok((node, Outcome::Done))
            }
            Action::Scroll(Scroll::IntoView(locator)) => {
                let node = self.locate(locator, counter).await?;
                self.actionable(&node, locator, false).await?;
                self.dispatch(&node.held.session, &ScrollIntoView { object_id: node.held.object.clone() }).await?;
                Ok((Some(node), Outcome::Done))
            }
            Action::Scroll(Scroll::By { dx, dy }) => {
                let (session, context) = self.main_world().await?;
                let centre = self.inspect_in(&session, context, "the viewport", VIEWPORT_CENTRE, Vec::new()).await?;
                let point = match &centre {
                    json::Value::Array(pair) => pair.iter().filter_map(as_f64).collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                let [x, y] = point[..] else { return Err(helper_reply("the viewport", &centre)) };
                let page = self.attachment.page.clone();
                self.dispatch(&page, &DispatchWheel { kind: "mouseWheel", x, y, delta_x: *dx, delta_y: *dy }).await?;
                Ok((None, Outcome::Done))
            }
            Action::Select { locator, values } => {
                let node = self.locate(locator, counter).await?;
                self.actionable(&node, locator, true).await?;
                let selected = self.select(&node, locator, values).await?;
                Ok((Some(node), Outcome::Selected(selected)))
            }
            Action::SetChecked { locator, checked } => {
                let node = self.locate(locator, counter).await?;
                self.actionable(&node, locator, true).await?;
                let (radio, before) = self.checked_state(&node, locator).await?;
                if before == *checked {
                    return Ok((Some(node), Outcome::Checked { changed: false }));
                }
                if radio && !checked {
                    return Err(ActionError::NotActionable { locator: locator.to_string(), reason: "is a radio button, which is unchecked by checking another".into() });
                }
                let point = self.hit_point(&node, locator).await?;
                self.click(&node, point, ClickCount::Single, locator).await?;
                if let Some(dialog) = self.attachment.current_dialog().await.map_err(subscription_end)? {
                    return Err(ActionError::DialogOpen(dialog));
                }
                let (_, after) = self.checked_state(&node, locator).await?;
                if after != *checked {
                    return Err(ActionError::NotActionable { locator: locator.to_string(), reason: "did not change state when clicked".into() });
                }
                Ok((Some(node), Outcome::Checked { changed: true }))
            }
            Action::Wait { condition, timeout } => Ok((None, self.wait(condition, *timeout, interrupt).await?)),
            Action::Read(read) => self.read(read, counter).await,
            Action::Screenshot => {
                let page = self.attachment.page.clone();
                let shot = self.call(&page, &CaptureScreenshot { format: "png" }).await?;
                let png = decode_base64(&shot.data)
                    .ok_or_else(|| ActionError::Cdp(CallError::MalformedReply { method: CaptureScreenshot::METHOD, reason: "the image is not base64".into() }))?;
                Ok((None, Outcome::Png(png)))
            }
            Action::Evaluate(expression) => {
                let page = self.attachment.page.clone();
                let evaluated = self.dispatch(&page, &EvaluateInPage { expression: expression.clone(), return_by_value: true, await_promise: true }).await?;
                if let Some(exception) = evaluated.exception_details {
                    return Err(ActionError::Script { exception: exception.message() });
                }
                let result = evaluated.result;
                let value = match (result.value, result.unserializable_value) {
                    (Some(value), _) => value,
                    (None, Some(unserializable)) => json::Value::String(unserializable),
                    (None, None) => json::Value::Null,
                };
                Ok((None, Outcome::Evaluated(value)))
            }
            Action::Dialog(response) => {
                let open = self.attachment.current_dialog().await.map_err(subscription_end)?;
                let (accept, prompt_text) = match response {
                    DialogResponse::Accept { text: Some(text) } => (true, text.clone()),
                    DialogResponse::Accept { text: None } => (true, open.as_ref().map(|d| d.default_prompt.clone()).unwrap_or_default()),
                    DialogResponse::Dismiss => (false, String::new()),
                };
                let page = self.attachment.page.clone();
                match self.call(&page, &HandleDialog { accept, prompt_text }).await {
                    Ok(_) => {
                        self.attachment.dialog = None;
                        Ok((None, Outcome::DialogHandled(open)))
                    }
                    Err(ActionError::Cdp(CallError::Protocol { .. })) => Err(ActionError::NoDialog),
                    Err(error) => Err(error),
                }
            }
        }
    }

    async fn focus_target(&mut self, into: Option<&Locator>, counter: &dyn RefCounter) -> Result<Option<Node>, ActionError> {
        let Some(locator) = into else { return Ok(None) };
        let node = self.locate(locator, counter).await?;
        self.actionable(&node, locator, true).await?;
        self.focus_end(&node, locator).await?;
        Ok(Some(node))
    }

    async fn read(&mut self, read: &Read, counter: &dyn RefCounter) -> Result<(Option<Node>, Outcome), ActionError> {
        match read {
            Read::Text(None) => Ok((None, self.read_text(None).await?)),
            Read::Text(Some(locator)) => {
                let node = self.locate(locator, counter).await?;
                let text = self.read_text(Some(&node)).await?;
                Ok((Some(node), text))
            }
            Read::Title => Ok((None, Outcome::Text { text: self.main_string("the title", TITLE).await?, truncated: false })),
            Read::Url => Ok((None, Outcome::Text { text: self.main_string("the URL", LOCATION).await?, truncated: false })),
            Read::Attribute { locator, name } => {
                let node = self.locate(locator, counter).await?;
                let value = self.inspect(&node.held, "the attribute", ATTRIBUTE, vec![string(name)]).await?;
                let attribute = match &value {
                    json::Value::Null => None,
                    json::Value::String(s) => Some(s.clone()),
                    _ => return Err(helper_reply("the attribute", &value)),
                };
                Ok((Some(node), Outcome::Attribute(attribute)))
            }
            Read::Value(locator) => {
                let node = self.locate(locator, counter).await?;
                let value = self.inspect(&node.held, "the value", FIELD_VALUE, Vec::new()).await?;
                let field = match &value {
                    json::Value::Array(parts) => match parts.as_slice() {
                        [json::Value::String(kind), json::Value::String(text)] if kind == "value" => FieldValue::Text(text.clone()),
                        [json::Value::String(kind)] if kind == "redacted" => FieldValue::Redacted,
                        [json::Value::String(kind), json::Value::String(name)] if kind == "element" => {
                            return Err(ActionError::NotActionable { locator: locator.to_string(), reason: format!("is a <{name}>, which has no value") });
                        }
                        _ => return Err(helper_reply("the value", &value)),
                    },
                    _ => return Err(helper_reply("the value", &value)),
                };
                Ok((Some(node), Outcome::Value(field)))
            }
        }
    }
}

impl Pages {
    /// Runs one action on `target`. A mutating action whose command was sent without an observed reply fails
    /// with [`CallError::UnknownOutcome`] and is never retried. A dialog that is open, or that the action opens,
    /// fails it with [`ActionError::DialogOpen`] until answered with [`Action::Dialog`]. A wait, or a navigation
    /// waiting to load, ends with [`ActionError::Interrupted`] soon after `interrupt` asks.
    pub async fn act(&mut self, target: &TargetScope, action: &Action, counter: &dyn RefCounter, interrupt: &dyn Interrupt) -> Result<Acted, ActionError> {
        let started = Instant::now();
        if target.generation != self.connection.generation() {
            return Err(ActionError::OtherGeneration);
        }
        let connection = self.connection.clone();
        let cdp = Counted::new(&connection);
        self.attach(&cdp, &target.target).await.map_err(ActionError::Attach)?;
        let attachment = self.attachments.get_mut(&target.target).expect("attached");
        let mut act = Act::new(&cdp, target, attachment, &mut self.refs);
        let open = act.attachment.current_dialog().await.map_err(subscription_end)?;
        let result = match open {
            Some(dialog) if !matches!(action, Action::Dialog(_)) => Err(ActionError::DialogOpen(dialog)),
            _ => act.run(action, counter, interrupt).await,
        };
        let open = act.attachment.current_dialog().await.map_err(subscription_end)?;
        let result = match (result, open) {
            (Ok(_), Some(dialog)) if action.mutates() => Err(ActionError::DialogOpen(dialog)),
            (result, open) => {
                if open.is_none() {
                    act.release().await;
                }
                result
            }
        };
        let (node, outcome) = result?;
        let on = node.map(|node| ActedOn { frame: node.scope.frame, element: node.element });
        Ok(Acted { target: target.clone(), on, outcome, cdp_calls: cdp.calls(), elapsed: started.elapsed() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_whole_urls() {
        assert!(glob("http://a/b", "http://a/b"));
        assert!(!glob("http://a/b", "http://a/bc"));
        assert!(glob("*/done", "http://a/done"));
        assert!(glob("http://*/x?*", "http://host:9/x?q=1"));
        assert!(!glob("http://*/x", "http://host/y"));
        assert!(glob("a*a", "aa"));
        assert!(!glob("a*a", "a"));
    }

    #[test]
    fn base64_decodes_with_and_without_padding_and_rejects_junk() {
        assert_eq!(decode_base64("iVBORw0KGgo=").unwrap(), b"\x89PNG\r\n\x1a\n");
        assert_eq!(decode_base64("YWJj").unwrap(), b"abc");
        assert_eq!(decode_base64("YQ==").unwrap(), b"a");
        assert_eq!(decode_base64("").unwrap(), b"");
        for bad in ["YQ=", "Y===", "YQ==YWJj", "Y!Jj"] {
            assert!(decode_base64(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_click_lands_in_the_centre_of_the_first_quad_with_area() {
        let empty = vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let box_ = vec![10.0, 20.0, 30.0, 20.0, 30.0, 40.0, 10.0, 40.0];
        assert_eq!(centre(&[empty, box_]), Some((20.0, 30.0)));
        assert_eq!(centre(&[]), None);
    }
}
