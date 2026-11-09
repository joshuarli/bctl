mod common;

use std::time::Instant;

use bctl_cdp::Scope;
use bctl_cdp::protocol::{Command as CdpCommand, Effect, Empty, TargetId};
use common::{Fixture, Home, OTHER_SITE_FROM_GUEST, assert_ok, dashboard_viewer, eventually, http, outside_client, soon, up_with_fixtures};
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

fn site(path: &str, port: u16) -> String {
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
    assert_eq!(refused(&home, "click", s, &["--role", "button", "--name", "Same", "--nth", "3"]).code, "NOT_FOUND");
    assert_eq!(refused(&home, "click", s, &["--css", "button:nth-of-type(2)", "--label", "x"]).code, "BAD_REQUEST");
    assert_eq!(refused(&home, "click", s, &["--css", "[["]).code, "BAD_REQUEST");
    let css_ambiguous = refused(&home, "click", s, &["--css", "button"]);
    assert_eq!(css_ambiguous.code, "AMBIGUOUS");
    assert!(css_ambiguous.candidates.unwrap().iter().any(|c| c.summary == "button \"Go\""));

    for (css, code) in [("#hidden", "NOT_VISIBLE"), ("#invisible", "NOT_VISIBLE"), ("#off", "DISABLED"), ("#aria-off", "DISABLED"), ("#under", "COVERED")] {
        let error = refused(&home, "click", s, &["--css", css]);
        assert_eq!((error.code.as_str(), error.target.as_deref()), (code, Some(buttons.as_str())), "{css}: {error:?}");
    }
    assert!(refused(&home, "click", s, &["--css", "#under"]).message.contains("div#overlay \"Overlay\""));
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
    assert_eq!(refused(&home, "select", s, &["--label", "Plan", "enterprise"]).code, "NOT_FOUND");
    assert_eq!(refused(&home, "select", s, &["--label", "Name", "x"]).code, "NOT_ACTIONABLE");

    assert_eq!(done(&home, "check", s, &["--label", "I agree"]).changed, Some(true));
    assert_eq!(done(&home, "check", s, &["--label", "I agree"]).changed, Some(false), "checking a checked box changes nothing");
    assert_eq!(evaluate(&home, s, "document.getElementById('agree').checked"), "true");
    assert!(events(&home, s).contains("agree input|agree change"), "a click checks the box with its events");
    assert_eq!(done(&home, "uncheck", s, &["--label", "I agree"]).changed, Some(true));
    assert_eq!(evaluate(&home, s, "document.getElementById('agree').checked"), "false");
    assert_eq!(refused(&home, "uncheck", s, &["--label", "Small"]).code, "NOT_ACTIONABLE");
    assert_eq!(refused(&home, "check", s, &["--label", "Locked"]).code, "DISABLED");
    assert_eq!(refused(&home, "fill", s, &["--label", "Fixed", "x"]).code, "NOT_ACTIONABLE");
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
    let (status, response) = http(port, &address, &format!("POST /api/sessions/{s}/control/release"), &as_viewer);
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
