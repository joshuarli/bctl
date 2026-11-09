mod common;

use std::io::Write;
use std::process::{Command, Stdio};

use common::{Fixture, Home, assert_ok, up_with_fixtures};
use miniserde::{Deserialize, json};

const STEPS: &str = r#"<!doctype html><title>Steps</title>
<label>Name <input id=name></label>
<button onclick="log('go:' + document.getElementById('name').value)">Go</button>
<button onclick="setTimeout(() => { document.getElementById('late').textContent = 'Shown later' }, 300)">Later</button>
<p id=late></p>
<script>window.events = []; function log(e) { events.push(e) }</script>"#;

fn site(path: &str, _port: u16) -> String {
    match path {
        "/steps" => STEPS.to_string(),
        _ => "<!doctype html><title>Missing</title>".to_string(),
    }
}

#[derive(Deserialize, Debug)]
struct Error {
    code: String,
    message: String,
}

#[derive(Deserialize, Debug)]
struct ActionView {
    action: String,
    target: String,
    text: Option<String>,
    value: Option<json::Value>,
}

#[derive(Deserialize, Debug)]
struct StepView {
    action: String,
    result: Option<ActionView>,
    error: Option<Error>,
    elapsed_ms: u64,
}

#[derive(Deserialize, Debug)]
struct BatchView {
    target: String,
    steps: u32,
    ran: Vec<StepView>,
    elapsed_ms: u64,
}

#[derive(Deserialize, Debug)]
struct PageView {
    batch: Option<BatchView>,
}

#[derive(Deserialize, Debug)]
struct Tabs {
    selected: Option<String>,
}

#[derive(Deserialize, Debug)]
struct Reply {
    error: Option<Error>,
    page: Option<PageView>,
    tabs: Option<Tabs>,
}

fn reply(home: &Home, args: &[&str], stdin: &str) -> (bool, Reply) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bctl"))
        .args(args)
        .arg("--json")
        .env("BCTL_HOME", &home.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    let parsed = json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {args:?}: {output:?}"));
    (output.status.success(), parsed)
}

fn batch(home: &Home, session: &str, steps: &str) -> (bool, Reply) {
    let file = home.path.join("batch.json");
    std::fs::write(&file, steps).unwrap();
    reply(home, &["page", "batch", session, file.to_str().unwrap()], "")
}

fn events(home: &Home, session: &str) -> String {
    let (ok, reply) = reply(home, &["page", "batch", session, "-"], r#"[{"action": "eval", "expression": "events.join('|')"}]"#);
    assert!(ok, "{reply:?}");
    let ran = reply.page.and_then(|p| p.batch).expect("a batch view").ran;
    json::to_string(ran[0].result.as_ref().and_then(|r| r.value.as_ref()).expect("eval returns a value"))
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn n5_a_batch_runs_its_steps_in_order_and_stops_at_the_first_failure_without_undoing_any() {
    let fixture = Fixture::start(site);
    let home = Home::new("n5-b");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n5-b");
    let (ok, opened) = reply(&home, &["tab", "new", s, "about:blank"], "");
    assert!(ok, "{opened:?}");
    let tab = opened.tabs.and_then(|t| t.selected).expect("a new tab is selected");

    let steps = format!(
        r##"[{{"action": "goto", "url": "{}"}},
            {{"action": "fill", "label": "Name", "text": "Zoë 日本"}},
            {{"action": "click", "role": "button", "name": "Go"}},
            {{"action": "click", "role": "button", "name": "Later"}},
            {{"action": "wait", "text": "Shown later", "timeout_ms": 5000}},
            {{"action": "get", "read": "text", "css": "#late"}},
            {{"action": "eval", "expression": "events.join('|')"}}]"##,
        fixture.url("/steps")
    );
    let (ok, done) = batch(&home, s, &steps);
    assert!(ok && done.error.is_none(), "{done:?}");
    let view = done.page.and_then(|p| p.batch).expect("a batch view");
    assert_eq!((view.target.as_str(), view.steps), (tab.as_str(), 7));
    assert_eq!(view.ran.iter().map(|step| step.action.as_str()).collect::<Vec<_>>(), ["goto", "fill", "click", "click", "wait", "get", "eval"]);
    assert!(view.ran.iter().all(|step| step.error.is_none() && step.result.as_ref().is_some_and(|r| r.target == tab && r.action == step.action)), "{view:?}");
    assert_eq!(view.ran[5].result.as_ref().and_then(|r| r.text.as_deref()), Some("Shown later"));
    assert_eq!(view.ran[6].result.as_ref().and_then(|r| r.value.as_ref()).map(json::to_string).as_deref(), Some("\"go:Zoë 日本\""), "steps ran in order");
    assert!(view.ran[4].elapsed_ms >= 200, "the wait step waited for the late text: {view:?}");
    assert!(view.ran.iter().map(|step| step.elapsed_ms).sum::<u64>() <= view.elapsed_ms, "{view:?}");

    let (ok, stopped) = batch(
        &home,
        s,
        r##"[{"action": "click", "role": "button", "name": "Go"},
            {"action": "click", "css": "#missing", "timeout_ms": 300},
            {"action": "click", "role": "button", "name": "Go"}]"##,
    );
    assert!(!ok, "a failed step fails the batch");
    let error = stopped.error.expect("the batch reports the step that stopped it");
    assert_eq!(error.code, "NOT_FOUND", "{error:?}");
    assert!(error.message.starts_with("batch step 2 of 3 (click) failed: "), "{}", error.message);
    let view = stopped.page.and_then(|p| p.batch).expect("a failed batch still shows its steps");
    assert_eq!(view.steps, 3);
    assert_eq!(view.ran.len(), 2, "nothing after the failing step runs");
    assert!(view.ran[0].result.is_some() && view.ran[1].result.is_none());
    assert_eq!(view.ran[1].error.as_ref().map(|e| e.code.as_str()), Some("NOT_FOUND"));
    assert_eq!(events(&home, s), "\"go:Zoë 日本|go:Zoë 日本\"", "the step before the failure stays done and the one after never ran");

    for (steps, why) in [
        (r#"[{"action": "click", "role": "button", "name": "Go"}, {"action": "click", "rol": "button"}]"#, "batch step 2: click takes no field \"rol\""),
        (r#"[{"action": "click", "role": "button", "name": "Go"}, {"action": "snapshot"}]"#, "batch step 2: \"snapshot\" is not a page action"),
        (r#"[{"action": "click", "role": "button", "name": "Go"}, {"action": "fill", "label": "Name"}]"#, "batch step 2: fill: the text to fill is required"),
        (r#"[{"action": "click", "role": "button", "name": "Go"}, {"action": "click", "nth": "two", "css": "button"}]"#, "batch step 2: a click field has the wrong type"),
        (r#"[]"#, "a batch holds 1 to 1000 steps; this one has 0"),
    ] {
        let (ok, refused) = batch(&home, s, steps);
        let error = refused.error.unwrap_or_else(|| panic!("{steps} ran"));
        assert!(!ok && error.code == "BAD_REQUEST" && error.message == why, "{steps}: {error:?}");
        assert!(refused.page.is_none(), "a malformed batch runs nothing");
    }
    assert_eq!(events(&home, s), "\"go:Zoë 日本|go:Zoë 日本\"", "a batch with a malformed step runs none of its steps");

    assert_ok(&home.bctl(&["down", s]));
}
