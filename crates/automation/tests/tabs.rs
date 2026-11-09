#[path = "../../cdp/tests/mock/mod.rs"]
mod mock;

use std::collections::HashMap;
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bctl_automation::{Binding, BindingStore, BindingStoreError, Listing, PageTarget, SafeUrl, Selection, TabError, Tabs, TargetScope};
use bctl_cdp::protocol::TargetId;
use bctl_cdp::{CallError, Connection, Generation, Termination};
use futures_lite::future::block_on;
use miniserde::{Deserialize, json};
use mock::{Action, Inbound, Mock, Request};

fn generation() -> Generation {
    Generation::new(7)
}

#[derive(Default)]
struct MemoryStore(Mutex<HashMap<Generation, Binding>>);

impl MemoryStore {
    fn get(&self, generation: Generation) -> Option<Binding> {
        self.0.lock().unwrap().get(&generation).cloned()
    }
}

impl BindingStore for MemoryStore {
    fn load(&self, generation: Generation) -> Result<Option<Binding>, BindingStoreError> {
        Ok(self.get(generation))
    }

    fn save(&self, generation: Generation, binding: &Binding) -> Result<(), BindingStoreError> {
        self.0.lock().unwrap().insert(generation, binding.clone());
        Ok(())
    }

    fn release(&self, generation: Generation, target: &TargetId) -> Result<(), BindingStoreError> {
        let mut bindings = self.0.lock().unwrap();
        if bindings.get(&generation).is_some_and(|b| b.target == *target) {
            bindings.remove(&generation);
        }
        Ok(())
    }
}

struct Target {
    id: String,
    kind: &'static str,
    url: String,
    opener: Option<String>,
}

#[derive(Deserialize)]
struct Params<T> {
    params: T,
}

#[derive(Deserialize)]
struct CreateParams {
    url: String,
}

#[derive(Deserialize)]
struct CloseParams {
    #[serde(rename = "targetId")]
    target_id: String,
}

/// A browser behind the mock socket: it answers target commands from its own target list, which a test
/// changes to stand in for a person or a page acting on the browser.
struct Browser {
    mock: Mock,
    targets: Vec<Target>,
    created: usize,
    methods: Vec<String>,
    drop_on: Option<&'static str>,
}

impl Browser {
    fn start(pages: &[(&str, &str)]) -> (Browser, Connection, JoinHandle<Termination>) {
        let mock = Mock::start();
        let (connection, driver) = block_on(bctl_cdp::connect(&mock.endpoint, generation())).unwrap();
        let mut targets: Vec<Target> = pages.iter().map(|(id, url)| Target { id: id.to_string(), kind: "page", url: url.to_string(), opener: None }).collect();
        targets.push(Target { id: "SW".into(), kind: "service_worker", url: "https://example.com/sw.js".into(), opener: None });
        let browser = Browser { mock, targets, created: 0, methods: Vec::new(), drop_on: None };
        (browser, connection, thread::spawn(move || block_on(driver)))
    }

    fn popup(&mut self, id: &str, opener: &str) {
        self.targets.push(Target { id: id.into(), kind: "page", url: "https://example.com/popup".into(), opener: Some(opener.into()) });
    }

    fn vanish(&mut self, id: &str) {
        self.targets.retain(|t| t.id != id);
    }

    fn answer(&mut self, request: Request) {
        self.methods.push(request.method.clone());
        if self.drop_on == Some(request.method.as_str()) {
            self.mock.act(Action::Drop);
            return;
        }
        let result = match request.method.as_str() {
            "Target.getTargets" => {
                let infos: Vec<String> = self
                    .targets
                    .iter()
                    .map(|t| {
                        let opener = t.opener.as_ref().map(|o| format!(r#","openerId":"{o}""#)).unwrap_or_default();
                        format!(r#"{{"targetId":"{}","type":"{}","title":"title {}","url":{},"attached":false{opener}}}"#, t.id, t.kind, t.id, json::to_string(&t.url))
                    })
                    .collect();
                format!(r#"{{"targetInfos":[{}]}}"#, infos.join(","))
            }
            "Target.createTarget" => {
                let url = json::from_str::<Params<CreateParams>>(&request.text).unwrap().params.url;
                self.created += 1;
                let id = format!("NEW{}", self.created);
                self.targets.push(Target { id: id.clone(), kind: "page", url, opener: None });
                format!(r#"{{"targetId":"{id}"}}"#)
            }
            "Target.closeTarget" => {
                let id = json::from_str::<Params<CloseParams>>(&request.text).unwrap().params.target_id;
                self.vanish(&id);
                r#"{"success":true}"#.to_string()
            }
            other => panic!("the mock browser does not answer {other}"),
        };
        self.mock.reply(request.id, &result);
    }

    /// Runs `op` on its own thread and answers its commands until it returns.
    fn serve<T: Send>(&mut self, op: impl FnOnce() -> T + Send) -> T {
        thread::scope(|scope| {
            let running = scope.spawn(op);
            while !running.is_finished() {
                if let Some(Inbound::Command(request)) = self.mock.next_within(Duration::from_millis(10)) {
                    self.answer(request);
                }
            }
            running.join().unwrap()
        })
    }
}

fn id(s: &str) -> TargetId {
    TargetId::new(s)
}

fn ids(targets: &[PageTarget]) -> Vec<&str> {
    targets.iter().map(|t| t.id.as_str()).collect()
}

#[test]
fn only_page_targets_are_listed_and_nothing_is_selected_until_asked() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "https://user:pw@example.com/a?token=s#f"), ("B", "about:blank")]);
    let store = MemoryStore::default();
    let tabs = Tabs::new(&connection, &store);
    let listing = browser.serve(|| block_on(tabs.list())).unwrap();
    assert_eq!(ids(&listing.targets), ["A", "B"]);
    assert_eq!(listing.targets[0].url.as_str(), "https://example.com/a");
    assert_eq!(listing.selection, Selection::Unselected);
    assert!(matches!(browser.serve(|| block_on(tabs.selected())), Err(TabError::NotSelected { targets }) if targets.len() == 2));
}

#[test]
fn selecting_a_target_binds_this_generation_and_keeps_only_a_safe_url() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "https://user:pw@example.com/a?token=s"), ("B", "about:blank")]);
    let store = MemoryStore::default();
    let tabs = Tabs::new(&connection, &store);
    let listing = browser.serve(|| block_on(tabs.select(&id("A")))).unwrap();
    assert_eq!(listing.selection, Selection::Selected(id("A")));
    assert_eq!(store.get(generation()), Some(Binding { target: id("A"), url: SafeUrl::new("https://example.com/a") }));
    let scope = browser.serve(|| block_on(tabs.selected())).unwrap();
    assert_eq!(scope, TargetScope { generation: generation(), target: id("A") });
}

#[test]
fn an_unknown_or_non_page_target_cannot_be_selected_and_the_binding_stays() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "about:blank"), ("B", "about:blank")]);
    let store = MemoryStore::default();
    let tabs = Tabs::new(&connection, &store);
    browser.serve(|| block_on(tabs.select(&id("A")))).unwrap();
    for missing in ["Z", "SW"] {
        match browser.serve(|| block_on(tabs.select(&id(missing)))) {
            Err(TabError::NoSuchTarget { target, targets }) => assert_eq!((target.as_str(), ids(&targets)), (missing, vec!["A", "B"])),
            other => panic!("expected NoSuchTarget, got {other:?}"),
        }
    }
    assert_eq!(store.get(generation()).map(|b| b.target), Some(id("A")));
}

#[test]
fn a_vanished_selected_target_is_gone_with_the_survivors_and_never_replaced() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "https://example.com/a"), ("B", "https://example.com/b?q=1")]);
    let store = MemoryStore::default();
    let tabs = Tabs::new(&connection, &store);
    browser.serve(|| block_on(tabs.select(&id("A")))).unwrap();
    browser.vanish("A");
    for _ in 0..2 {
        let listing = browser.serve(|| block_on(tabs.list())).unwrap();
        assert_eq!(ids(&listing.targets), ["B"]);
        assert_eq!(listing.selection, Selection::Gone(Binding { target: id("A"), url: SafeUrl::new("https://example.com/a") }));
        match browser.serve(|| block_on(tabs.selected())) {
            Err(TabError::TargetGone { binding, remaining }) => {
                assert_eq!(binding.target, id("A"));
                assert_eq!(remaining.iter().map(|t| t.url.as_str()).collect::<Vec<_>>(), ["https://example.com/b"]);
            }
            other => panic!("expected TargetGone, got {other:?}"),
        }
    }
    assert_eq!(store.get(generation()).map(|b| b.target), Some(id("A")));
    let listing = browser.serve(|| block_on(tabs.select(&id("B")))).unwrap();
    assert_eq!(listing.selection, Selection::Selected(id("B")));
}

#[test]
fn a_popup_from_the_selected_target_leaves_the_binding_alone() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "https://example.com/a")]);
    let store = MemoryStore::default();
    let tabs = Tabs::new(&connection, &store);
    browser.serve(|| block_on(tabs.select(&id("A")))).unwrap();
    browser.popup("P", "A");
    let listing = browser.serve(|| block_on(tabs.list())).unwrap();
    assert_eq!(ids(&listing.targets), ["A", "P"]);
    assert_eq!(listing.selection, Selection::Selected(id("A")));
}

#[test]
fn opening_a_target_selects_it() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "about:blank")]);
    let store = MemoryStore::default();
    let tabs = Tabs::new(&connection, &store);
    browser.serve(|| block_on(tabs.select(&id("A")))).unwrap();
    let listing = browser.serve(|| block_on(tabs.open("https://example.com/new?secret=1"))).unwrap();
    assert_eq!(ids(&listing.targets), ["A", "NEW1"]);
    assert_eq!(listing.selection, Selection::Selected(id("NEW1")));
    assert_eq!(store.get(generation()), Some(Binding { target: id("NEW1"), url: SafeUrl::new("https://example.com/new") }));
}

#[test]
fn closing_the_selected_target_unselects_and_closing_another_keeps_the_selection() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "about:blank"), ("B", "about:blank"), ("C", "about:blank")]);
    let store = MemoryStore::default();
    let tabs = Tabs::new(&connection, &store);
    browser.serve(|| block_on(tabs.select(&id("A")))).unwrap();
    let Listing { targets, selection } = browser.serve(|| block_on(tabs.close(&id("B")))).unwrap();
    assert_eq!((ids(&targets), selection), (vec!["A", "C"], Selection::Selected(id("A"))));
    let Listing { targets, selection } = browser.serve(|| block_on(tabs.close(&id("A")))).unwrap();
    assert_eq!((ids(&targets), selection), (vec!["C"], Selection::Unselected));
    assert_eq!(store.get(generation()), None);
}

#[test]
fn the_last_page_target_is_never_closed() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "about:blank")]);
    let store = MemoryStore::default();
    let tabs = Tabs::new(&connection, &store);
    assert!(matches!(browser.serve(|| block_on(tabs.close(&id("A")))), Err(TabError::LastTarget { target }) if target == id("A")));
    assert!(!browser.methods.iter().any(|m| m == "Target.closeTarget"), "{:?}", browser.methods);
}

#[test]
fn a_binding_from_another_generation_is_not_this_generations_selection() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "about:blank")]);
    let store = MemoryStore::default();
    store.save(Generation::new(6), &Binding { target: id("A"), url: SafeUrl::new("about:blank") }).unwrap();
    let tabs = Tabs::new(&connection, &store);
    assert_eq!(browser.serve(|| block_on(tabs.list())).unwrap().selection, Selection::Unselected);
}

#[test]
fn a_close_whose_reply_is_lost_reports_an_unknown_outcome_and_keeps_the_binding() {
    let (mut browser, connection, _driver) = Browser::start(&[("A", "about:blank"), ("B", "about:blank")]);
    let store = MemoryStore::default();
    let tabs = Tabs::new(&connection, &store);
    browser.serve(|| block_on(tabs.select(&id("A")))).unwrap();
    browser.drop_on = Some("Target.closeTarget");
    match browser.serve(|| block_on(tabs.close(&id("A")))) {
        Err(TabError::Cdp(CallError::UnknownOutcome { method: "Target.closeTarget", .. })) => {}
        other => panic!("expected an unknown outcome, got {other:?}"),
    }
    assert_eq!(store.get(generation()).map(|b| b.target), Some(id("A")));
}
