//! Drives one page of a bctl session through a CDP capability URL:
//!
//! ```sh
//! cargo run -p bctl-automation --example drive -- "$(bctl cdp-url NAME)" https://example.com/
//! ```
//!
//! It opens a tab at the URL, waits for it to load, prints its snapshot, clicks the first link the snapshot
//! shows, and prints the title it lands on.

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::ops::Range;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use bctl_automation::cdp::{self, Endpoint, Generation};
use bctl_automation::{
    Action, Binding, BindingStore, BindingStoreError, Condition, Coverage, LoadState, Locator, Outcome, Pages, Read as ReadWhat, RefCounter, RefCounterError,
    SnapshotNode, SnapshotOptions, Tabs, Uninterrupted,
};
use futures_lite::future::block_on;
use miniserde::{Deserialize, json};

#[derive(Default)]
struct Bindings(RefCell<Option<(Generation, Binding)>>);

impl BindingStore for Bindings {
    fn load(&self, generation: Generation) -> Result<Option<Binding>, BindingStoreError> {
        Ok(self.0.borrow().as_ref().filter(|(g, _)| *g == generation).map(|(_, b)| b.clone()))
    }

    fn save(&self, generation: Generation, binding: &Binding) -> Result<(), BindingStoreError> {
        *self.0.borrow_mut() = Some((generation, binding.clone()));
        Ok(())
    }

    fn release(&self, generation: Generation, target: &cdp::protocol::TargetId) -> Result<(), BindingStoreError> {
        let mut held = self.0.borrow_mut();
        if held.as_ref().is_some_and(|(g, b)| *g == generation && b.target == *target) {
            *held = None;
        }
        Ok(())
    }
}

struct Counter(Cell<u64>);

impl RefCounter for Counter {
    fn reserve(&self, count: u64) -> Result<Range<u64>, RefCounterError> {
        let start = self.0.get();
        self.0.set(start + count);
        Ok(start..start + count)
    }
}

#[derive(Deserialize)]
struct Version {
    #[serde(rename = "webSocketDebuggerUrl")]
    url: String,
}

/// The browser websocket behind a capability URL. The capability is the URL's host, so the request carries it.
fn browser_endpoint(control_url: &str) -> Result<Endpoint, String> {
    let host = control_url.strip_prefix("http://").ok_or("a bctl CDP URL starts with http://")?.trim_end_matches('/');
    let port = host.rsplit_once(':').and_then(|(_, port)| port.parse::<u16>().ok()).ok_or("the CDP URL has no port")?;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("cannot reach the gateway: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|e| e.to_string())?;
    let (head, body) = response.split_once("\r\n\r\n").ok_or("the gateway sent no HTTP response")?;
    if head.split_whitespace().nth(1) != Some("200") {
        return Err(format!("the gateway refused: {}", body.trim()));
    }
    let version: Version = json::from_str(body).map_err(|_| "unexpected /json/version")?;
    Endpoint::parse(&version.url).map_err(|e| e.to_string())
}

fn first_link(nodes: &[SnapshotNode]) -> Option<&SnapshotNode> {
    nodes.iter().find_map(|node| if node.role == "link" && node.element.is_some() { Some(node) } else { first_link(&node.children) })
}

fn drive(control_url: &str, url: &str) -> Result<(), String> {
    let (connection, driver) = block_on(cdp::connect(&browser_endpoint(control_url)?, Generation::new(1))).map_err(|e| e.to_string())?;
    let driven = thread::spawn(move || block_on(driver));
    let bindings = Bindings::default();
    let counter = Counter(Cell::new(1));
    let tabs = Tabs::new(&connection, &bindings);
    let mut pages = Pages::new(connection.clone());

    let result = block_on(async {
        tabs.open(url).await.map_err(|e| e.to_string())?;
        let target = tabs.selected().await.map_err(|e| e.to_string())?;
        let loaded = Action::Wait { condition: Condition::Loaded(LoadState::Load), timeout: Duration::from_secs(30) };
        pages.act(&target, &loaded, &counter, &Uninterrupted).await.map_err(|e| e.to_string())?;
        let options = SnapshotOptions { coverage: Coverage::Interactive, depth: None, scope: None };
        let snapshot = pages.snapshot(&target, &options, &counter).await.map_err(|e| e.to_string())?;
        print!("{}", snapshot.text());
        let Some(link) = first_link(&snapshot.nodes) else { return Ok(()) };
        let element = link.element.expect("first_link only returns nodes with refs");
        println!("clicking @{element} {:?}", link.name);
        pages.act(&target, &Action::Click { locator: Locator::Ref(element), count: bctl_automation::ClickCount::Single }, &counter, &Uninterrupted).await.map_err(|e| e.to_string())?;
        pages.act(&target, &loaded, &counter, &Uninterrupted).await.map_err(|e| e.to_string())?;
        match pages.act(&target, &Action::Read(ReadWhat::Title), &counter, &Uninterrupted).await.map_err(|e| e.to_string())?.outcome {
            Outcome::Text { text, .. } => println!("landed on {text:?}"),
            other => return Err(format!("reading the title returned {other:?}")),
        }
        Ok(())
    });
    let termination = block_on(connection.shutdown());
    driven.join().map_err(|_| "the CDP driver panicked")?;
    eprintln!("connection closed: {termination}");
    result
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [control_url, url] = args.as_slice() else {
        eprintln!("usage: drive CDP_URL URL   (CDP_URL from `bctl cdp-url NAME`)");
        return ExitCode::from(2);
    };
    match drive(control_url, url) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("drive: {error}");
            ExitCode::FAILURE
        }
    }
}
