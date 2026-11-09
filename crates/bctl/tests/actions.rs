mod common;

use std::thread;
use std::time::{Duration, Instant};

use bctl_cdp::Scope;
use bctl_cdp::protocol::{Command as CdpCommand, Effect, Empty, TargetId};
use common::{
    Fixture, Home, OTHER_SITE_FROM_GUEST, assert_ok, attach, dashboard_viewer, evaluate_outside, eventually, http, outside_client, soon, up_with_fixtures,
};
use futures_lite::future::block_on;
use miniserde::{Deserialize, Serialize, json};

const BUTTONS: &str = r#"<!doctype html><title>Buttons</title>
<style>body { margin: 0 } button, div[role] { display: block; margin: 4px }</style>
<div role=button tabindex=0 onclick="log('fancy')">Fancy</div>
<button onclick="log('go')">Go</button>
<button onclick="log('same 1')">Same</button><button onclick="log('same 2')">Same</button>
<button id=hidden style="display:none" onclick="log('hidden')">Hidden</button>
<button id=invisible style="visibility:hidden" onclick="log('invisible')">Invisible</button>
<button id=off disabled onclick="log('off')">Off</button>
<span aria-disabled=true><button id=aria-off onclick="log('aria-off')">Aria off</button></span>
<div style="position:relative"><button id=under onclick="log('under')">Under</button>
<div id=overlay style="position:absolute; inset:0; background:rgba(0,0,0,.1)">Overlay</div></div>
<button id=twice ondblclick="log('dbl')">Twice</button>
<div id=hover onmouseenter="log('hover')" style="width:80px; height:20px">Hover me</div>
<button id=alert onclick="alert('Hello from the page')">Alert</button>
<button id=confirm onclick="log('confirm ' + confirm('Sure?'))">Confirm</button>
<button id=popup onclick="window.open('/inner?popup', '_blank')">Popup</button>
<button id=far style="margin-top:3000px" onclick="log('far')">Far</button>
<a id=away href="/inner?away">Away</a>
<p id=log></p>
<script>
window.events = [];
function log(e) { events.push(e); document.getElementById('log').textContent = events.join(','); }
document.addEventListener('click', e => { if (!e.isTrusted) events.push('untrusted click'); }, true);
</script>"#;

const FORM: &str = r#"<!doctype html><title>Form</title>
<form onsubmit="event.preventDefault(); events.push('submit')">
<label>Name <input id=name></label>
<label for=bio>Bio</label> <textarea id=bio></textarea>
<input id=search placeholder="Search here">
<select id=plan aria-label=Plan><option value=free>Free</option><option value=pro>Pro plan</option></select>
<label><input type=checkbox id=agree> I agree</label>
<label><input type=radio name=size id=small checked> Small</label>
<input type=checkbox id=locked disabled aria-label=Locked>
<input id=fixed readonly value=fixed aria-label=Fixed>
<input type=password id=secret aria-label=Secret value=hunter2>
<div contenteditable=true data-testid=editor aria-label=Editor></div>
<button type=submit>Send</button>
</form>
<p id=late></p>
<script>
window.events = [];
for (const id of ['name', 'bio', 'search', 'plan', 'agree']) {
  for (const type of ['keydown', 'input', 'change']) {
    document.getElementById(id).addEventListener(type, e => events.push(id + ' ' + type + (e.isTrusted ? '' : ' untrusted')));
  }
}
setTimeout(() => { document.getElementById('late').textContent = 'Arrived late'; location.hash = 'done'; }, 400);
</script>"#;

fn playwright(page: &str) -> Option<&'static str> {
    Some(match page {
        "late-layout" => include_str!("fixtures/playwright/late-layout.html"),
        "overlay" => include_str!("fixtures/playwright/overlay.html"),
        "fractional" => include_str!("fixtures/playwright/fractional.html"),
        "detach" => include_str!("fixtures/playwright/detach.html"),
        "frame" => include_str!("fixtures/playwright/frame.html"),
        "navigation" => include_str!("fixtures/playwright/navigation.html"),
        "landing" => include_str!("fixtures/playwright/landing.html"),
        "hang" => include_str!("fixtures/playwright/hang.html"),
        _ => return None,
    })
}

fn site(path: &str, port: u16) -> String {
    if let Some(page) = path.strip_prefix("/playwright/").and_then(|rest| playwright(rest.split('?').next().unwrap_or_default())) {
        return page.to_string();
    }
    match path {
        "/buttons" => BUTTONS.to_string(),
        "/form" => FORM.to_string(),
        "/frames" => format!(
            "<!doctype html><title>Frames</title><style>iframe {{ border: 6px solid; padding: 5px; width: 260px; height: 90px }}</style>
             <div style=\"padding: 120px 0 0 150px\"><iframe title=Same src=\"/inner?same\"></iframe></div>
             <div style=\"padding: 40px 0 0 300px\"><iframe title=Cross src=\"http://{OTHER_SITE_FROM_GUEST}:{port}/inner?cross\"></iframe></div>"
        ),
        inner if inner.starts_with("/inner?") => {
            let which = &inner[7..];
            format!(
                "<!doctype html><title>Inner</title><body style=\"margin: 10px 0 0 40px\"><button onclick=\"document.getElementById('out-{which}').textContent = 'clicked ' + Date.now()\">Inside {which}</button><p id=out-{which}></p>"
            )
        }
        _ => "<!doctype html><title>Missing</title>".to_string(),
    }
}

/// One thing a fixture page saw: a pointer event, or a change it made.
#[derive(Deserialize, Debug, Clone)]
struct Seen {
    what: String,
    on: String,
    trusted: bool,
    page: String,
    at: f64,
}

#[derive(Deserialize, Debug)]
struct Candidate {
    #[serde(rename = "ref")]
    element: String,
    summary: String,
}

#[derive(Deserialize, Debug)]
struct DialogView {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

#[derive(Deserialize, Debug)]
struct Error {
    code: String,
    message: String,
    target: Option<String>,
    candidates: Option<Vec<Candidate>>,
    dialog: Option<Box<DialogView>>,
}

#[derive(Deserialize, Debug)]
struct ActionView {
    target: String,
    frame: Option<String>,
    #[serde(rename = "ref")]
    element: Option<String>,
    changed: Option<bool>,
    selected: Option<Vec<String>>,
    text: Option<String>,
    redacted: Option<bool>,
    value: Option<json::Value>,
    matched: Option<u64>,
    bytes: Option<u64>,
    dialog: Option<DialogView>,
    cdp_calls: u32,
    elapsed_ms: u64,
}

#[derive(Deserialize, Debug)]
struct Node {
    #[serde(rename = "ref")]
    element: Option<String>,
    role: String,
    name: Option<String>,
    children: Option<Vec<Node>>,
}

#[derive(Deserialize, Debug)]
struct SnapshotView {
    nodes: Option<Vec<Node>>,
}

#[derive(Deserialize, Debug)]
struct PageView {
    snapshot: Option<SnapshotView>,
    action: Option<ActionView>,
}

#[derive(Deserialize, Debug)]
struct Tab {
    id: String,
}

#[derive(Deserialize, Debug)]
struct Tabs {
    selected: Option<String>,
    targets: Vec<Tab>,
}

#[derive(Deserialize, Debug)]
struct Reply {
    error: Option<Error>,
    page: Option<PageView>,
    tabs: Option<Tabs>,
}

fn reply(home: &Home, args: &[&str]) -> Reply {
    let mut argv = args.to_vec();
    argv.push("--json");
    let output = home.bctl(&argv);
    json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {argv:?}: {output:?}"))
}

fn page(home: &Home, verb: &str, session: &str, args: &[&str]) -> Result<ActionView, Error> {
    let mut argv = vec!["page", verb, session];
    argv.extend_from_slice(args);
    match reply(home, &argv) {
        Reply { error: None, page: Some(PageView { action: Some(action), .. }), .. } => Ok(action),
        Reply { error: Some(error), .. } => Err(error),
        other => panic!("bctl {argv:?}: {other:?}"),
    }
}

fn done(home: &Home, verb: &str, session: &str, args: &[&str]) -> ActionView {
    page(home, verb, session, args).unwrap_or_else(|error| panic!("bctl page {verb} {args:?} failed: {error:?}"))
}

fn refused(home: &Home, verb: &str, session: &str, args: &[&str]) -> Error {
    page(home, verb, session, args).err().unwrap_or_else(|| panic!("bctl page {verb} {args:?} succeeded"))
}

fn evaluate(home: &Home, session: &str, expression: &str) -> String {
    json::to_string(done(home, "eval", session, &[expression]).value.as_ref().expect("eval returns a value"))
}

fn events(home: &Home, session: &str) -> String {
    evaluate(home, session, "events.join('|')")
}

fn open_tab(home: &Home, session: &str, url: &str) -> String {
    reply(home, &["tab", "new", session, url]).tabs.and_then(|t| t.selected).expect("a new tab is selected")
}

fn walk<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for node in nodes {
        out.push(node);
        walk(node.children.as_deref().unwrap_or_default(), out);
    }
}

fn snapshot_ref(home: &Home, session: &str, role: &str, name: &str) -> String {
    let snapshot = match reply(home, &["page", "snapshot", session]) {
        Reply { page: Some(PageView { snapshot: Some(snapshot), .. }), .. } => snapshot,
        other => panic!("snapshot failed: {other:?}"),
    };
    let mut all = Vec::new();
    walk(snapshot.nodes.as_deref().unwrap_or_default(), &mut all);
    let node = all.into_iter().find(|n| n.role == role && n.name.as_deref() == Some(name)).unwrap_or_else(|| panic!("no {role} {name:?}"));
    node.element.clone().expect("an interactive node has a ref")
}

#[derive(Serialize)]
struct CloseTarget {
    #[serde(rename = "targetId")]
    target_id: TargetId,
}

impl CdpCommand for CloseTarget {
    const METHOD: &'static str = "Target.closeTarget";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

fn percentile(samples: &mut [u64], p: usize) -> u64 {
    samples.sort_unstable();
    samples[(samples.len() * p / 100).min(samples.len() - 1)]
}

/// Runs `args` `runs` times and returns the CLI's wall-clock p50 and p95 and the daemon's, in ms.
fn timed(home: &Home, verb: &str, session: &str, args: &[&str], runs: usize) -> (u64, u64, u64, u64) {
    let (mut wall, mut daemon) = (Vec::new(), Vec::new());
    for _ in 0..runs {
        let started = Instant::now();
        let action = done(home, verb, session, args);
        wall.push(started.elapsed().as_millis() as u64);
        daemon.push(action.elapsed_ms);
    }
    (percentile(&mut wall, 50), percentile(&mut wall, 95), percentile(&mut daemon, 50), percentile(&mut daemon, 95))
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n4_clicks_land_only_on_the_named_visible_enabled_uncovered_element_in_any_frame() {
    let fixture = Fixture::start(site);
    let home = Home::new("n4-c");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n4-c");

    let buttons = open_tab(&home, s, "about:blank");
    let went = done(&home, "goto", s, &[&fixture.url("/buttons")]);
    assert_eq!(went.target, buttons);
    assert_eq!(done(&home, "get", s, &["title"]).text.as_deref(), Some("Buttons"), "goto waits for the load event");

    let go = snapshot_ref(&home, s, "button", "Go");
    let by_ref = done(&home, "click", s, &[&format!("@{go}")]);
    assert_eq!((by_ref.element.as_deref(), by_ref.frame.as_deref()), (Some(go.as_str()), Some(buttons.as_str())));
    done(&home, "click", s, &["--role", "button", "--name", "Go"]);
    done(&home, "click", s, &["--role", "button", "--name", "Fancy"]);
    assert_eq!(events(&home, s), "\"go|go|fancy\"", "a ref and an implicit role reach the same button, and every click is trusted");

    let ambiguous = refused(&home, "click", s, &["--role", "button", "--name", "Same"]);
    assert_eq!(ambiguous.code, "AMBIGUOUS", "{ambiguous:?}");
    let candidates = ambiguous.candidates.expect("an ambiguous locator lists its matches");
    assert_eq!(candidates.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(), ["button \"Same\"", "button \"Same\""]);
    assert_eq!(events(&home, s), "\"go|go|fancy\"", "an ambiguous click clicks nothing");
    done(&home, "click", s, &[&format!("@{}", candidates[1].element)]);
    done(&home, "click", s, &["--role", "button", "--name", "Same", "--nth", "1"]);
    assert_eq!(refused(&home, "click", s, &["--role", "button", "--name", "Same", "--nth", "3", "--timeout", "300"]).code, "NOT_FOUND");
    assert_eq!(refused(&home, "click", s, &["--css", "button:nth-of-type(2)", "--label", "x"]).code, "BAD_REQUEST");
    assert_eq!(refused(&home, "click", s, &["--css", "[["]).code, "BAD_REQUEST");
    let css_ambiguous = refused(&home, "click", s, &["--css", "button"]);
    assert_eq!(css_ambiguous.code, "AMBIGUOUS");
    assert!(css_ambiguous.candidates.unwrap().iter().any(|c| c.summary == "button \"Go\""));

    for (css, code) in [("#hidden", "NOT_VISIBLE"), ("#invisible", "NOT_VISIBLE"), ("#off", "DISABLED"), ("#aria-off", "DISABLED"), ("#under", "COVERED")] {
        let error = refused(&home, "click", s, &["--css", css, "--timeout", "300"]);
        assert_eq!((error.code.as_str(), error.target.as_deref()), (code, Some(buttons.as_str())), "{css}: {error:?}");
    }
    assert!(refused(&home, "click", s, &["--css", "#under", "--timeout", "300"]).message.contains("div#overlay \"Overlay\""));
    done(&home, "dblclick", s, &["--css", "#twice"]);
    done(&home, "hover", s, &["--css", "#hover"]);
    done(&home, "click", s, &["--css", "#far"]);
    assert_eq!(events(&home, s), "\"go|go|fancy|same 2|same 1|dbl|hover|far\"", "refused clicks clicked nothing");
    assert!(evaluate(&home, s, "scrollY").parse::<f64>().unwrap() > 1000.0, "a click scrolls its element into view");

    let alert = refused(&home, "click", s, &["--css", "#alert"]);
    assert_eq!(alert.code, "DIALOG_OPEN", "{alert:?}");
    let dialog = alert.dialog.expect("the dialog is described");
    assert_eq!((dialog.kind.as_str(), dialog.message.as_str()), ("alert", "Hello from the page"));
    assert_eq!(refused(&home, "click", s, &["--role", "button", "--name", "Go"]).code, "DIALOG_OPEN", "nothing acts behind a dialog");
    let dismissed = done(&home, "dialog", s, &["dismiss"]);
    assert_eq!(dismissed.dialog.map(|d| d.message), Some("Hello from the page".into()));
    assert_eq!(refused(&home, "dialog", s, &["dismiss"]).code, "NO_DIALOG");
    assert_eq!(refused(&home, "click", s, &["--css", "#confirm"]).code, "DIALOG_OPEN");
    done(&home, "dialog", s, &["accept"]);
    assert!(events(&home, s).ends_with("far|confirm true\""), "accepting a confirm answers it");

    let before_popup = reply(&home, &["tab", "list", s]).tabs.expect("tabs").targets.len();
    done(&home, "click", s, &["--css", "#popup"]);
    let with_popup = eventually("the popup to open", || reply(&home, &["tab", "list", s]).tabs.filter(|t| t.targets.len() == before_popup + 1));
    assert_eq!(with_popup.selected.as_deref(), Some(buttons.as_str()));
    assert!(with_popup.targets.iter().any(|t| t.id == buttons));
    assert_eq!(done(&home, "get", s, &["title"]).text.as_deref(), Some("Buttons"), "a popup leaves the selected target alone");

    let shot = home.path.join("buttons.png");
    let taken = done(&home, "screenshot", s, &[shot.to_str().unwrap()]);
    let png = std::fs::read(&shot).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n") && taken.bytes == Some(png.len() as u64), "{} bytes", png.len());

    let frames = open_tab(&home, s, &fixture.url("/frames"));
    eventually("both iframes to load", || {
        page(&home, "wait", s, &["--role", "button", "--name", "Inside cross", "--timeout", "1000"]).ok()?;
        page(&home, "wait", s, &["--role", "button", "--name", "Inside same", "--timeout", "1000"]).ok()
    });
    for which in ["cross", "same"] {
        let clicked = done(&home, "click", s, &["--role", "button", "--name", &format!("Inside {which}")]);
        assert_eq!(clicked.target, frames);
        assert_ne!(clicked.frame.as_deref(), Some(frames.as_str()), "the button is in its iframe's frame");
        let out = done(&home, "get", s, &["text", "--css", &format!("#out-{which}")]);
        assert!(out.text.as_deref().is_some_and(|t| t.starts_with("clicked ")), "the {which}-origin click landed: {out:?}");
    }
    let inner = refused(&home, "click", s, &["--role", "button"]);
    assert_eq!(inner.candidates.map(|c| c.len()), Some(2), "a query searches the page and both iframes");

    let outside = outside_client(&home, s);
    block_on(outside.call(Scope::Browser, &CloseTarget { target_id: TargetId::new(&frames) }, soon())).unwrap();
    let gone = eventually("the closed target to be noticed", || page(&home, "click", s, &["--role", "button", "--name", "Inside same"]).err());
    assert_eq!((gone.code.as_str(), gone.target.as_deref()), ("TARGET_GONE", Some(frames.as_str())), "{gone:?}");

    assert_eq!(reply(&home, &["tab", "use", s, &buttons]).tabs.and_then(|t| t.selected).as_deref(), Some(buttons.as_str()));
    let go = snapshot_ref(&home, s, "button", "Go");
    let warm_ref = done(&home, "click", s, &[&format!("@{go}")]);
    let warm_role = done(&home, "click", s, &["--role", "button", "--name", "Go"]);
    let warm_css = done(&home, "click", s, &["--css", "#twice"]);
    let (ref_p50, ref_p95, ref_daemon_p50, ref_daemon_p95) = timed(&home, "click", s, &[&format!("@{go}")], 40);
    let (role_p50, role_p95, role_daemon_p50, role_daemon_p95) = timed(&home, "click", s, &["--role", "button", "--name", "Go"], 40);
    let (get_p50, get_p95, get_daemon_p50, get_daemon_p95) = timed(&home, "get", s, &["title"], 40);
    eprintln!(
        "n4 measurement: click CDP calls by ref {}, by role+name {}, by css {}; warm click by ref p50/p95 {ref_p50}/{ref_p95} ms CLI, {ref_daemon_p50}/{ref_daemon_p95} ms daemon; \
         by role p50/p95 {role_p50}/{role_p95} ms CLI, {role_daemon_p50}/{role_daemon_p95} ms daemon; get title p50/p95 {get_p50}/{get_p95} ms CLI, {get_daemon_p50}/{get_daemon_p95} ms daemon",
        warm_ref.cdp_calls, warm_role.cdp_calls, warm_css.cdp_calls,
    );

    done(&home, "click", s, &["--role", "link", "--name", "Away"]);
    done(&home, "wait", s, &["--url", "*/inner?away"]);
    assert_eq!(done(&home, "get", s, &["title"]).text.as_deref(), Some("Inner"), "a click that navigates its own document away has landed");
    assert_ok(&home.bctl(&["down", s]));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n4_forms_take_unicode_text_selections_and_checks_like_a_person_and_refuse_while_a_person_has_control() {
    let fixture = Fixture::start(site);
    let home = Home::new("n4-f");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n4-f");
    open_tab(&home, s, "about:blank");
    done(&home, "goto", s, &[&fixture.url("/form"), "--wait", "domcontentloaded"]);
    done(&home, "wait", s, &["--load", "load"]);

    let name = "Zoë 日本語 🚀 naïve";
    let filled = done(&home, "fill", s, &["--label", "Name", name]);
    let value = |label: &str| done(&home, "get", s, &["value", "--label", label]).text;
    assert_eq!(value("Name").as_deref(), Some(name));
    done(&home, "fill", s, &["--label", "Name", "Replaced"]);
    assert_eq!(value("Name").as_deref(), Some("Replaced"), "fill replaces the content");
    done(&home, "fill", s, &["--label", "Bio", "Ünïcödé bio"]);
    assert_eq!(value("Bio").as_deref(), Some("Ünïcödé bio"));
    let typed = done(&home, "type", s, &["--placeholder", "Search here", "Ab1 ü?"]);
    assert_eq!(done(&home, "get", s, &["value", "--placeholder", "Search here"]).text.as_deref(), Some("Ab1 ü?"));
    done(&home, "press", s, &["--placeholder", "Search here", "Backspace"]);
    assert_eq!(done(&home, "get", s, &["value", "--placeholder", "Search here"]).text.as_deref(), Some("Ab1 ü"));
    done(&home, "press", s, &["Enter"]);
    let log = events(&home, s);
    for expected in ["name input", "name change", "bio input", "bio change", "search keydown", "search input", "submit"] {
        assert!(log.contains(expected), "no {expected:?} in {log}");
    }
    assert!(!log.contains("name keydown"), "fill inserts text without key presses: {log}");
    assert!(!log.contains("untrusted"), "typed input is trusted: {log}");

    let selected = done(&home, "select", s, &["--label", "Plan", "Pro plan"]);
    assert_eq!(selected.selected, Some(vec!["pro".to_string()]), "an option is chosen by its label");
    assert_eq!(value("Plan").as_deref(), Some("pro"));
    assert!(events(&home, s).contains("plan input untrusted|plan change untrusted"), "select fires input then change");
    assert_eq!(refused(&home, "select", s, &["--label", "Plan", "enterprise", "--timeout", "300"]).code, "NOT_FOUND");
    assert_eq!(refused(&home, "select", s, &["--label", "Name", "x"]).code, "NOT_ACTIONABLE");

    assert_eq!(done(&home, "check", s, &["--label", "I agree"]).changed, Some(true));
    assert_eq!(done(&home, "check", s, &["--label", "I agree"]).changed, Some(false), "checking a checked box changes nothing");
    assert_eq!(evaluate(&home, s, "document.getElementById('agree').checked"), "true");
    assert!(events(&home, s).contains("agree input|agree change"), "a click checks the box with its events");
    assert_eq!(done(&home, "uncheck", s, &["--label", "I agree"]).changed, Some(true));
    assert_eq!(evaluate(&home, s, "document.getElementById('agree').checked"), "false");
    assert_eq!(refused(&home, "uncheck", s, &["--label", "Small"]).code, "NOT_ACTIONABLE");
    assert_eq!(refused(&home, "check", s, &["--label", "Locked", "--timeout", "300"]).code, "DISABLED");
    assert_eq!(refused(&home, "fill", s, &["--label", "Fixed", "x", "--timeout", "300"]).code, "NOT_ACTIONABLE");
    assert_eq!(refused(&home, "fill", s, &["--label", "I agree", "x"]).code, "NOT_ACTIONABLE");
    let secret = done(&home, "get", s, &["value", "--label", "Secret"]);
    assert_eq!((secret.text, secret.redacted), (None, Some(true)), "a password is never read back");

    done(&home, "type", s, &["--testid", "editor", "Héllo wörld"]);
    done(&home, "type", s, &["--testid", "editor", " ✓"]);
    let edited = done(&home, "get", s, &["text", "--testid", "editor"]).text.map(|t| t.replace('\u{a0}', " "));
    assert_eq!(edited.as_deref(), Some("Héllo wörld ✓"), "typing appends to a contenteditable");
    done(&home, "fill", s, &["--label", "Editor", "Fresh ⚡"]);
    assert_eq!(done(&home, "get", s, &["text", "--label", "Editor"]).text.as_deref(), Some("Fresh ⚡"));
    assert_eq!(done(&home, "get", s, &["attribute", "contenteditable", "--testid", "editor"]).value.map(|v| json::to_string(&v)).as_deref(), Some("\"true\""));
    assert!(done(&home, "get", s, &["attribute", "missing", "--testid", "editor"]).value.is_none(), "an absent attribute is null");

    assert_eq!(done(&home, "wait", s, &["--css", "#late:not(:empty)"]).matched, Some(1));
    assert_eq!(done(&home, "wait", s, &["--text", "Arrived late"]).matched, None);
    done(&home, "wait", s, &["--url", "*/form#done"]);
    let late = refused(&home, "wait", s, &["--css", "#never", "--timeout", "300"]);
    assert_eq!(late.code, "TIMEOUT");
    assert!(done(&home, "get", s, &["url"]).text.unwrap().ends_with("/form#done"));
    assert_eq!(refused(&home, "eval", s, &["throw new Error('nope')"]).code, "EVAL_FAILED");

    let before = events(&home, s);
    let (port, address, as_viewer) = dashboard_viewer(&home);
    eventually("the daemon to offer input control", || (http(port, &address, &format!("POST /api/sessions/{s}/control"), &as_viewer).0 == 200).then_some(()));
    for (verb, args) in [
        ("click", vec!["--label", "I agree"]),
        ("fill", vec!["--label", "Name", "x"]),
        ("type", vec!["x"]),
        ("press", vec!["Enter"]),
        ("select", vec!["--label", "Plan", "free"]),
        ("check", vec!["--label", "I agree"]),
        ("goto", vec!["about:blank"]),
        ("eval", vec!["events.push('evaluated')"]),
        ("scroll", vec!["--dy", "100"]),
        ("dialog", vec!["dismiss"]),
    ] {
        assert_eq!(refused(&home, verb, s, &args).code, "HUMAN_IN_CONTROL", "{verb}");
    }
    assert_eq!(done(&home, "get", s, &["value", "--label", "Plan"]).text.as_deref(), Some("pro"), "reads go on while a person has control");
    done(&home, "wait", s, &["--text", "Arrived late"]);
    let (status, response) = http(port, &address, &format!("POST /api/sessions/{s}/control/resume"), &as_viewer);
    assert_eq!(status, 200, "{response}");
    assert_eq!(events(&home, s), before, "refused actions changed nothing");

    let warm_fill = done(&home, "fill", s, &["--label", "Name", "Warm"]);
    let (fill_p50, fill_p95, fill_daemon_p50, fill_daemon_p95) = timed(&home, "fill", s, &["--label", "Name", "Zoë 日本"], 40);
    eprintln!(
        "n4 measurement: fill CDP calls by label {} (first fill {}, type of 6 chars {}); warm fill p50/p95 {fill_p50}/{fill_p95} ms CLI, {fill_daemon_p50}/{fill_daemon_p95} ms daemon",
        warm_fill.cdp_calls, filled.cdp_calls, typed.cdp_calls,
    );
    assert_eq!(refused(&home, "goto", s, &["http://100.96.0.1:1/"]).code, "NAVIGATION_FAILED");
    assert_ok(&home.bctl(&["down", s]));
}

fn seen_in(home: &Home, session: &str, expression: &str) -> Vec<Seen> {
    let value = done(home, "eval", session, &[expression]).value.expect("eval returns a value");
    json::from_str(&json::to_string(&value)).unwrap_or_else(|_| panic!("{expression} is not a record: {}", json::to_string(&value)))
}

fn seen(home: &Home, session: &str) -> Vec<Seen> {
    seen_in(home, session, "seen")
}

fn input(seen: &[Seen]) -> Vec<&Seen> {
    seen.iter().filter(|s| matches!(s.what.as_str(), "mousedown" | "mouseup" | "click")).collect()
}

fn noted<'a>(seen: &'a [Seen], what: &str, on: &str) -> &'a Seen {
    seen.iter().rev().find(|s| s.what == what && s.on == on).unwrap_or_else(|| panic!("the page never noted {what} {on}: {seen:?}"))
}

/// Asserts the page's one trusted click went to `on`, after it noted `ready` and within `bound` of arming.
fn clicked_after<'a>(seen: &'a [Seen], armed: &str, on: &str, bound: Duration) -> &'a Seen {
    let arming = noted(seen, "armed", armed);
    let armed_at = arming.at;
    let ready = noted(seen, "ready", armed).at;
    let since: Vec<&Seen> = input(seen).into_iter().filter(|s| s.page == arming.page && s.at >= armed_at).collect();
    assert_eq!(since.iter().map(|s| (s.what.as_str(), s.on.as_str(), s.trusted)).collect::<Vec<_>>(), [("mousedown", on, true), ("mouseup", on, true), ("click", on, true)], "{seen:?}");
    let click = since[2];
    assert!(click.at >= ready, "{on} was clicked at {} ms, before it was ready at {ready} ms", click.at);
    assert!(click.at - armed_at < bound.as_millis() as f64, "{on} was clicked {} ms after arming", click.at - armed_at);
    click
}

#[derive(Serialize)]
struct DeviceMetrics {
    width: u32,
    height: u32,
    #[serde(rename = "deviceScaleFactor")]
    device_scale_factor: f64,
    mobile: bool,
}

impl CdpCommand for DeviceMetrics {
    const METHOD: &'static str = "Emulation.setDeviceMetricsOverride";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n7_actions_wait_for_late_layout_and_overlays_and_hit_fractional_boxes_at_any_device_scale() {
    let fixture = Fixture::start(site);
    let home = Home::new("n7-l");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n7-l");
    let tab = open_tab(&home, s, "about:blank");
    let bound = Duration::from_secs(5);

    done(&home, "goto", s, &[&fixture.url("/playwright/late-layout")]);
    let started = Instant::now();
    let late = refused(&home, "click", s, &["--css", "#shown", "--timeout", "800"]);
    let waited = started.elapsed();
    assert_eq!(late.code, "NOT_VISIBLE", "{late:?}");
    assert!(late.message.contains("is not visible: its style hides it") && late.message.contains("still after waiting"), "{}", late.message);
    assert!(waited >= Duration::from_millis(800) && waited < bound, "the click gave up after {waited:?}");
    assert!(input(&seen(&home, s)).is_empty(), "an element that never became visible got no input");
    for (id, after, on) in [("moving", 0, "moving"), ("shown", 500, "shown"), ("revealed", 500, "revealed"), ("nested", 500, "nested"), ("enabled", 500, "label")] {
        done(&home, "eval", s, &[&format!("arm('{id}', {after})")]);
        done(&home, "click", s, &["--css", &format!("#{id}")]);
        clicked_after(&seen(&home, s), id, on, bound);
    }
    assert_eq!(noted(&seen(&home, s), "left", "200").on, "200", "the moving button was clicked where it came to rest");

    done(&home, "goto", s, &[&fixture.url("/playwright/overlay")]);
    let covered = refused(&home, "click", s, &["--css", "#target", "--timeout", "600"]);
    assert_eq!(covered.code, "COVERED", "{covered:?}");
    assert!(covered.message.contains("div#flyover") && covered.message.contains("still after waiting"), "{}", covered.message);
    for (overlay, target) in [("flyover", "target"), ("veil", "below")] {
        done(&home, "eval", s, &[&format!("arm('{overlay}', 600)")]);
        done(&home, "click", s, &["--css", &format!("#{target}")]);
        clicked_after(&seen(&home, s), overlay, target, bound);
    }
    assert!(input(&seen(&home, s)).iter().all(|s| s.on != "flyover" && s.on != "veil"), "no overlay ever got the input");

    done(&home, "goto", s, &[&fixture.url("/playwright/fractional")]);
    let outside = outside_client(&home, s);
    let emulated = attach(&outside, &tab);
    let dots = ["aligned", "half", "v1", "v2", "v3", "v4", "zoomed-v1", "zoomed-v4"];
    for scale in [1.0, 1.5, 2.75] {
        block_on(outside.call(Scope::Session(&emulated), &DeviceMetrics { width: 0, height: 0, device_scale_factor: scale, mobile: false }, soon())).unwrap();
        let ratio = match evaluate_outside(&outside, &emulated, "devicePixelRatio") {
            json::Value::Number(json::Number::F64(ratio)) => ratio,
            json::Value::Number(json::Number::U64(ratio)) => ratio as f64,
            other => panic!("devicePixelRatio is {}", json::to_string(&other)),
        };
        assert_eq!(ratio, scale, "the page renders at the emulated scale");
        for dot in dots {
            let before = input(&seen(&home, s)).len();
            done(&home, "click", s, &["--css", &format!("#{dot}")]);
            let all = seen(&home, s);
            let after: Vec<_> = input(&all).into_iter().skip(before).map(|s| (s.what.clone(), s.on.clone(), s.trusted)).collect();
            assert_eq!(after, [("mousedown".to_string(), dot.to_string(), true), ("mouseup".into(), dot.into(), true), ("click".into(), dot.into(), true)], "the 1x1 {dot} at scale {scale}");
        }
    }
    assert_ok(&home.bctl(&["down", s]));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n7_a_ref_stays_stale_when_its_node_leaves_while_a_query_follows_the_page_to_its_new_element() {
    let fixture = Fixture::start(site);
    let home = Home::new("n7-d");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n7-d");
    let tab = open_tab(&home, s, "about:blank");
    let quickly = Duration::from_secs(3);
    let stale = |args: &[&str]| {
        let started = Instant::now();
        let error = refused(&home, "click", s, args);
        assert_eq!(error.code, "STALE_REF", "{args:?}: {error:?}");
        assert!(started.elapsed() < quickly, "a stale ref fails when its node goes, not at the deadline: {:?}", started.elapsed());
        error
    };

    done(&home, "goto", s, &[&fixture.url("/playwright/detach")]);
    done(&home, "wait", s, &["--css", "#inner"]);
    done(&home, "eval", s, &["addButton()"]);
    let first = snapshot_ref(&home, s, "button", "Click me");
    done(&home, "eval", s, &["removeLater(600)"]);
    let removed = stale(&[&format!("@{first}")]);
    assert!(removed.message.contains("no longer in its document"), "{}", removed.message);

    done(&home, "eval", s, &["addButton()"]);
    done(&home, "eval", s, &["churnLater(300)"]);
    let started = Instant::now();
    done(&home, "click", s, &["--css", "#stage button"]);
    assert!(started.elapsed() < Duration::from_secs(5));
    let record = seen(&home, s);
    let ready = noted(&record, "ready", "b4").at;
    let clicks: Vec<_> = input(&record).into_iter().map(|s| (s.what.as_str(), s.on.as_str(), s.trusted)).collect();
    assert_eq!(clicks, [("mousedown", "b4", true), ("mouseup", "b4", true), ("click", "b4", true)], "only the button that stayed got the input: {record:?}");
    assert!(input(&record)[0].at >= ready, "{record:?}");

    done(&home, "eval", s, &["stopButton(true)"]);
    done(&home, "eval", s, &["addButton()"]);
    let moving = snapshot_ref(&home, s, "button", "Click me");
    done(&home, "eval", s, &["adoptLater(500)"]);
    stale(&[&format!("@{moving}")]);
    let framed = seen_in(&home, s, "document.getElementById('frame').contentWindow.seen");
    assert!(input(&framed).is_empty() && input(&seen(&home, s)).len() == 3, "the button that moved into the frame got no input: {framed:?}");
    noted(&seen(&home, s), "moved", "b5");

    let inner = snapshot_ref(&home, s, "button", "Inner");
    done(&home, "eval", s, &["dropFrameLater(500)"]);
    stale(&[&format!("@{inner}")]);
    let gone = refused(&home, "click", s, &["--css", "#inner", "--timeout", "800"]);
    assert_eq!(gone.code, "NOT_FOUND", "a query for an element whose frame went away waits and then fails: {gone:?}");

    done(&home, "goto", s, &[&fixture.url("/playwright/navigation")]);
    done(&home, "eval", s, &["sessionStorage.clear(); seen.length = 0"]);
    let moving = snapshot_ref(&home, s, "button", "Moving");
    done(&home, "eval", s, &["reloadLater(500)"]);
    let navigated = stale(&[&format!("@{moving}")]);
    assert!(navigated.message.contains("navigated away"), "{}", navigated.message);
    done(&home, "goto", s, &[&fixture.url("/playwright/navigation")]);
    done(&home, "eval", s, &["reloadLater(500)"]);
    done(&home, "click", s, &["--css", "#moving"]);
    let record = seen(&home, s);
    let landed: Vec<_> = input(&record).into_iter().map(|s| (s.what.as_str(), s.on.as_str(), s.page.as_str())).collect();
    assert_eq!(
        landed,
        [("mousedown", "moving", "/playwright/navigation?stable"), ("mouseup", "moving", "/playwright/navigation?stable"), ("click", "moving", "/playwright/navigation?stable")],
        "the query clicked the button of the page the navigation brought: {record:?}"
    );

    for (id, from) in [("away", "link"), ("leave", "press")] {
        done(&home, "goto", s, &[&fixture.url("/playwright/navigation?stable")]);
        done(&home, "eval", s, &["sessionStorage.clear(); seen.length = 0"]);
        done(&home, "click", s, &["--css", &format!("#{id}")]);
        done(&home, "wait", s, &["--url", &format!("*from={from}")]);
        let record = seen(&home, s);
        let pressed = input(&record).into_iter().find(|s| s.what == "mousedown").unwrap_or_else(|| panic!("{record:?}"));
        assert!(pressed.on == id && pressed.trusted && pressed.page == "/playwright/navigation?stable", "{record:?}");
        noted(&record, "landed", &format!("?from={from}"));
    }

    done(&home, "goto", s, &[&fixture.url("/playwright/navigation?stable")]);
    let before = reply(&home, &["tab", "list", s]).tabs.expect("tabs").targets.len();
    done(&home, "click", s, &["--css", "#popup"]);
    let with_popup = eventually("the popup to open", || reply(&home, &["tab", "list", s]).tabs.filter(|t| t.targets.len() == before + 1));
    assert_eq!(with_popup.selected.as_deref(), Some(tab.as_str()), "the popup left the selected target alone");
    let record = seen(&home, s);
    assert!(input(&record).iter().any(|s| s.what == "click" && s.on == "popup" && s.trusted), "{record:?}");
    assert_ok(&home.bctl(&["down", s]));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n7_input_whose_reply_or_connection_is_lost_is_unknown_and_never_sent_again() {
    let fixture = Fixture::start(site);
    let home = Home::new("n7-u");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n7-u");
    open_tab(&home, s, "about:blank");
    let beacons = || fixture.requests().into_iter().filter(|r| r.starts_with("POST /playwright/beacon")).collect::<Vec<_>>();

    done(&home, "goto", s, &[&fixture.url("/playwright/hang?ms=13000")]);
    let lost = refused(&home, "click", s, &["--css", "#hang"]);
    assert_eq!(lost.code, "UNKNOWN_OUTCOME", "{lost:?}");
    assert!(lost.message.contains("Input.dispatchMouseEvent was sent but its outcome is unknown"), "{}", lost.message);
    assert_eq!(beacons(), ["POST /playwright/beacon?trusted-hang"], "the press reached the button once");
    let record = eventually("the page to answer again", || page(&home, "eval", s, &["seen"]).ok().map(|_| seen(&home, s)));
    let sent: Vec<_> = input(&record).into_iter().map(|s| (s.what.as_str(), s.on.as_str(), s.trusted)).collect();
    assert_eq!(sent, [("mousedown", "hang", true)], "after the lost reply bctl sent nothing more, not even the release: {record:?}");

    done(&home, "goto", s, &[&fixture.url("/playwright/hang?ms=120000")]);
    let clicking = {
        let path = home.path.clone();
        let session = session.clone();
        thread::spawn(move || {
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_bctl")).args(["page", "click", &session, "--css", "#hang", "--json"]).env("BCTL_HOME", path).output().unwrap();
            json::from_str::<Reply>(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("{output:?}"))
        })
    };
    eventually("the second press to reach the button", || (beacons().len() == 2).then_some(()));
    assert_ok(&home.bctl(&["down", s]));
    let cut = clicking.join().unwrap().error.expect("the click does not succeed");
    assert_eq!(cut.code, "UNKNOWN_OUTCOME", "{cut:?}");
    assert_eq!(beacons().len(), 2, "the press was sent once and never again");
}
