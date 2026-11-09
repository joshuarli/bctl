mod common;

use std::process::Command;

use bctl_cdp::protocol::{AttachToTarget, Command as CdpCommand, Effect, Empty, GetTargets, TargetId};
use bctl_cdp::{Connection, Scope};
use common::{Fixture, Home, OTHER_SITE_FROM_GUEST, assert_ok, eventually, outside_client, soon, up_with_fixtures};
use futures_lite::future::block_on;
use miniserde::{Deserialize, Serialize, json};

const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024;

const FORM: &str = "<!doctype html><title>Form</title>
<h1>Sign in</h1>
<nav><a href=\"/roles\">Roles</a></nav>
<form>
<label>Email <input type=email name=email value=\"a@b.c\"></label>
<label>Password <input type=password name=password value=\"hunter2\"></label>
<label><input type=checkbox checked> Remember me</label>
<select aria-label=Plan><option>Free<option selected>Pro</select>
<button type=submit>Sign in</button>
<button type=button disabled>Later</button>
</form>
<p id=note></p>
<script>document.getElementById('note').textContent = 'odd \\uD800 text'</script>";

const ROLES: &str = "<!doctype html><title>Roles</title><main><h2>Controls</h2>
<input type=search aria-label=Find> <input type=range aria-label=Volume> <input type=number aria-label=Count value=3>
<input type=radio name=side aria-label=Left checked> <textarea aria-label=Notes></textarea>
<select multiple aria-label=Colors><option>Red<option>Blue</select>
<ul><li>One</li><li>Two</li></ul></main>";

const DYNAMIC: &str = "<!doctype html><title>Dynamic</title><button id=keep>Keep</button><button id=swap>Swap</button>";

fn site(path: &str, port: u16) -> String {
    match path {
        "/form" => FORM.to_string(),
        "/roles" => ROLES.to_string(),
        "/dynamic" => DYNAMIC.to_string(),
        "/frames" => format!(
            "<!doctype html><title>Frames</title><button>Outside</button>
             <iframe title=Same src=\"/inner?same\"></iframe>
             <iframe title=Cross src=\"http://{OTHER_SITE_FROM_GUEST}:{port}/inner?cross\"></iframe>"
        ),
        "/many" => format!("<!doctype html><title>Many</title>{}", (0..3000).map(|n| format!("<button>Button {n}</button>")).collect::<String>()),
        inner if inner.starts_with("/inner?") => format!("<!doctype html><title>Inner</title><button>Inside {}</button>", &inner[7..]),
        _ => "<!doctype html><title>Missing</title>".to_string(),
    }
}

#[derive(Deserialize, Debug)]
struct Node {
    #[serde(rename = "ref")]
    element: Option<String>,
    role: String,
    name: Option<String>,
    value: Option<String>,
    redacted: Option<bool>,
    disabled: Option<bool>,
    checked: Option<String>,
    level: Option<u32>,
    children: Option<Vec<Node>>,
}

#[derive(Deserialize, Debug)]
struct SnapshotView {
    target: String,
    text: Option<String>,
    nodes: Option<Vec<Node>>,
    omitted: u64,
    documents: u32,
    cdp_calls: u32,
    text_bytes: u64,
}

#[derive(Deserialize, Debug)]
struct ElementView {
    #[serde(rename = "ref")]
    element: String,
    target: String,
    frame: String,
    role: String,
    name: String,
}

#[derive(Deserialize, Debug)]
struct PageView {
    snapshot: Option<SnapshotView>,
    element: Option<ElementView>,
}

#[derive(Deserialize, Debug)]
struct Error {
    code: String,
    message: String,
    target: Option<String>,
}

#[derive(Deserialize, Debug)]
struct Tab {
    id: String,
    title: String,
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

fn snapshot(home: &Home, session: &str, flags: &[&str]) -> SnapshotView {
    let mut args = vec!["page", "snapshot", session];
    args.extend_from_slice(flags);
    match reply(home, &args) {
        Reply { error: None, page: Some(PageView { snapshot: Some(snapshot), .. }), .. } => snapshot,
        other => panic!("bctl {args:?} failed: {other:?}"),
    }
}

fn text_snapshot(home: &Home, session: &str, flags: &[&str]) -> String {
    let mut args = vec!["page", "snapshot", session];
    args.extend_from_slice(flags);
    let output = home.bctl(&args);
    assert_ok(&output);
    String::from_utf8(output.stdout).unwrap()
}

fn resolve(home: &Home, session: &str, element: &str) -> Result<ElementView, Error> {
    match reply(home, &["page", "resolve", session, element]) {
        Reply { error: None, page: Some(PageView { element: Some(element), .. }), .. } => Ok(element),
        Reply { error: Some(error), .. } => Err(error),
        other => panic!("bctl page resolve {element}: {other:?}"),
    }
}

fn stale(home: &Home, session: &str, element: &str) -> Error {
    let error = resolve(home, session, element).expect_err(&format!("{element} still resolves"));
    assert_eq!(error.code, "STALE_REF", "{error:?}");
    error
}

fn refused(home: &Home, args: &[&str]) -> Error {
    reply(home, args).error.unwrap_or_else(|| panic!("bctl {args:?} succeeded"))
}

fn open_tab(home: &Home, session: &str, url: &str) -> String {
    reply(home, &["tab", "new", session, url]).tabs.and_then(|t| t.selected).expect("a new tab is selected")
}

/// Waits until a tab shows its page's title, so its document has been parsed.
fn titled(home: &Home, session: &str, target: &str, title: &str) {
    eventually(&format!("{target} to show {title:?}"), || {
        let tabs = reply(home, &["tab", "list", session]).tabs?;
        tabs.targets.iter().any(|t| t.id == target && t.title == title).then_some(())
    })
}

fn walk<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for node in nodes {
        out.push(node);
        walk(node.children.as_deref().unwrap_or_default(), out);
    }
}

fn all(snapshot: &SnapshotView) -> Vec<&Node> {
    let mut out = Vec::new();
    walk(snapshot.nodes.as_deref().expect("asked for nodes"), &mut out);
    out
}

fn find<'a>(snapshot: &'a SnapshotView, role: &str, name: &str) -> Option<&'a Node> {
    all(snapshot).into_iter().find(|n| n.role == role && n.name.as_deref() == Some(name))
}

fn ref_of(snapshot: &SnapshotView, role: &str, name: &str) -> String {
    let node = find(snapshot, role, name).unwrap_or_else(|| panic!("no {role} {name:?} in {snapshot:#?}"));
    node.element.clone().unwrap_or_else(|| panic!("{role} {name:?} has no ref"))
}

fn ref_number(element: &str) -> u64 {
    element.strip_prefix('e').and_then(|n| n.parse().ok()).unwrap_or_else(|| panic!("{element} is not a ref"))
}

/// Waits until the selected tab has loaded far enough to show `role` `name`.
fn loaded(home: &Home, session: &str, flags: &[&str], role: &str, name: &str) -> SnapshotView {
    eventually(&format!("{role} {name:?} to appear"), || {
        let snapshot = snapshot(home, session, flags);
        find(&snapshot, role, name).is_some().then_some(snapshot)
    })
}

#[derive(Serialize)]
struct Evaluate {
    expression: String,
}

impl CdpCommand for Evaluate {
    const METHOD: &'static str = "Runtime.evaluate";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

/// Runs a script in a target's main frame from outside bctl, as the page itself might.
fn run_script(outside: &Connection, target: &str, expression: &str) {
    let attached = block_on(outside.call(Scope::Browser, &AttachToTarget::new(TargetId::new(target)), soon())).unwrap();
    block_on(outside.call(Scope::Session(&attached.session_id), &Evaluate { expression: expression.to_string() }, soon())).unwrap();
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn n3_snapshots_show_roles_names_states_and_refs_across_frames_and_never_a_password() {
    let fixture = Fixture::start(site);
    let home = Home::new("n3-s");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n3-s");

    let form = open_tab(&home, s, &fixture.url("/form"));
    titled(&home, s, &form, "Form");
    let cold = snapshot(&home, s, &["--json"]);
    assert!(find(&cold, "button", "Sign in").is_some(), "{cold:#?}");
    let warm = snapshot(&home, s, &["--json"]);
    let text = text_snapshot(&home, s, &[]);
    let refs = |names: &[(&str, &str)]| names.iter().map(|(role, name)| ref_of(&warm, role, name)).collect::<Vec<_>>();
    let [link, email, password, remember, plan, sign_in, later] =
        <[String; 7]>::try_from(refs(&[("link", "Roles"), ("textbox", "Email"), ("textbox", "Password"), ("checkbox", "Remember me"), ("combobox", "Plan"), ("button", "Sign in"), ("button", "Later")]))
            .unwrap();
    assert_eq!(
        text,
        format!(
            "- link \"Roles\" [ref={link}]\n- textbox \"Email\" [ref={email}]: \"a@b.c\"\n- textbox \"Password\" [ref={password}]: <redacted>\n\
             - checkbox \"Remember me\" [ref={remember}, checked]\n- combobox \"Plan\" [ref={plan}, collapsed]: \"Pro\"\n  \
             - option \"Free\" [ref={}]\n  - option \"Pro\" [ref={}, selected]\n\
             - button \"Sign in\" [ref={sign_in}]\n- button \"Later\" [ref={later}, disabled]\n",
            ref_of(&warm, "option", "Free"),
            ref_of(&warm, "option", "Pro"),
        ),
        "the default snapshot is the interactive one"
    );
    let password_node = find(&warm, "textbox", "Password").unwrap();
    assert_eq!((password_node.value.as_deref(), password_node.redacted), (None, Some(true)));
    assert_eq!(find(&warm, "button", "Later").unwrap().disabled, Some(true));
    assert_eq!(find(&warm, "checkbox", "Remember me").unwrap().checked.as_deref(), Some("true"));
    assert_eq!((warm.target.as_str(), warm.text.as_deref()), (form.as_str(), None), "--json returns nodes, not text");

    let json_bytes = home.bctl(&["page", "snapshot", s, "--json"]).stdout.len();
    let full_text = text_snapshot(&home, s, &["--full"]);
    let full = snapshot(&home, s, &["--full", "--json"]);
    for secret in ["hunter2", "•"] {
        assert!(!full_text.contains(secret) && !text.contains(secret), "a password leaked: {full_text}");
    }
    assert_eq!(find(&full, "heading", "Sign in").and_then(|h| h.level), Some(1));
    let navigation = all(&full).into_iter().find(|n| n.role == "navigation").expect("<nav> is a navigation");
    assert_eq!(navigation.children.as_deref().and_then(|c| c.first()).and_then(|l| l.element.clone()), Some(link.clone()));
    assert!(find(&full, "paragraph", "odd \u{fffd} text").is_some(), "an unpaired surrogate in page text is replaced, not fatal: {full_text}");
    assert_eq!(ref_of(&full, "button", "Sign in"), sign_in, "a node keeps its ref across snapshots of one document");

    let scoped = snapshot(&home, s, &["--scope", "form", "--json"]);
    assert!(find(&scoped, "link", "Roles").is_none(), "the link is outside the form");
    assert_eq!(ref_of(&scoped, "button", "Sign in"), sign_in);
    assert_eq!(refused(&home, &["page", "snapshot", s, "--scope", "#nowhere"]).code, "SCOPE_NOT_FOUND");
    assert_eq!(refused(&home, &["page", "snapshot", s, "--scope", "[["]).code, "BAD_REQUEST");
    let shallow = snapshot(&home, s, &["--full", "--depth", "1", "--json"]);
    assert!(shallow.nodes.as_ref().unwrap().iter().all(|n| n.children.is_none()), "{shallow:#?}");
    assert!(find(&shallow, "link", "Roles").is_none(), "the link is below the first level");

    open_tab(&home, s, &fixture.url("/roles"));
    let roles = loaded(&home, s, &["--full", "--json"], "listbox", "Colors");
    for (role, name) in [("searchbox", "Find"), ("slider", "Volume"), ("spinbutton", "Count"), ("radio", "Left"), ("textbox", "Notes"), ("option", "Red")] {
        assert!(find(&roles, role, name).is_some(), "no implicit {role} {name:?}");
    }
    assert_eq!(find(&roles, "heading", "Controls").and_then(|h| h.level), Some(2));
    assert_eq!(find(&roles, "spinbutton", "Count").and_then(|n| n.value.as_deref()), Some("3"));
    assert_eq!(find(&roles, "radio", "Left").and_then(|n| n.checked.as_deref()), Some("true"));
    for role in ["main", "list", "listitem"] {
        assert!(all(&roles).iter().any(|n| n.role == role), "no implicit {role}");
    }

    let frames = open_tab(&home, s, &fixture.url("/frames"));
    let framed = eventually("both iframes to load", || {
        let framed = snapshot(&home, s, &["--json"]);
        (find(&framed, "button", "Inside same").is_some() && find(&framed, "button", "Inside cross").is_some()).then_some(framed)
    });
    assert_eq!(framed.documents, 3, "the page and both iframes are read");
    let same_iframe = find(&framed, "iframe", "Same").expect("the same-origin iframe is shown");
    let cross_iframe = find(&framed, "iframe", "Cross").expect("the cross-site iframe is shown");
    let child = |iframe: &Node| iframe.children.as_ref().and_then(|c| c.first()).map(|n| (n.role.clone(), n.name.clone()));
    assert_eq!(child(same_iframe), Some(("button".into(), Some("Inside same".into()))));
    assert_eq!(child(cross_iframe), Some(("button".into(), Some("Inside cross".into()))));
    let framed_warm = snapshot(&home, s, &["--json"]);
    let outside_ref = ref_of(&framed, "button", "Outside");
    let resolved: Vec<ElementView> =
        [&outside_ref, &ref_of(&framed, "button", "Inside same"), &ref_of(&framed, "button", "Inside cross")].iter().map(|r| resolve(&home, s, r).unwrap()).collect();
    assert_eq!(resolved.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Outside", "Inside same", "Inside cross"]);
    assert!(resolved.iter().all(|e| e.role == "button" && e.target == frames));
    assert_eq!(resolved[0].element, outside_ref);
    assert_eq!(resolved[0].frame, frames, "the main frame's id is the target's");
    assert!(resolved[1].frame != frames && resolved[2].frame != frames && resolved[1].frame != resolved[2].frame);
    let outside = outside_client(&home, s);
    let targets = block_on(outside.call(Scope::Browser, &GetTargets {}, soon())).unwrap().target_infos;
    let oopif = targets.iter().find(|t| t.kind == "iframe").expect("the cross-site iframe has its own target");
    assert_eq!(oopif.target_id.as_str(), resolved[2].frame, "the cross-site iframe runs out of process");
    assert!(!targets.iter().any(|t| t.target_id.as_str() == resolved[1].frame), "the same-origin iframe runs in the page's process");

    open_tab(&home, s, &fixture.url("/inner?solo"));
    loaded(&home, s, &[], "button", "Inside solo");
    let one = snapshot(&home, s, &["--json"]);
    open_tab(&home, s, &fixture.url("/many"));
    loaded(&home, s, &[], "button", "Button 0");
    let many = snapshot(&home, s, &["--json"]);
    let many_text = text_snapshot(&home, s, &[]);
    assert!(many.omitted > 0 && many.text_bytes <= MAX_SNAPSHOT_BYTES + 64, "{} bytes, {} omitted", many.text_bytes, many.omitted);
    assert!(many_text.ends_with(&format!("- … {} more nodes not shown\n", many.omitted)), "{}", &many_text[many_text.len() - 200..]);
    assert_eq!(many.cdp_calls, one.cdp_calls, "a snapshot of 3000 buttons costs the same CDP calls as one of a single button");

    eprintln!(
        "n3 measurement: form fixture cold snapshot {} CDP calls, warm {} CDP calls, interactive text {} bytes, --json {} bytes, full text {} bytes; frames page warm {} calls over {} documents; one button {} calls, 3000 buttons {} calls",
        cold.cdp_calls,
        warm.cdp_calls,
        warm.text_bytes,
        json_bytes,
        full.text_bytes,
        framed_warm.cdp_calls,
        framed_warm.documents,
        one.cdp_calls,
        many.cdp_calls,
    );
    assert_ok(&home.bctl(&["down", s]));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn n3_a_ref_resolves_only_while_its_node_document_target_and_daemon_last() {
    let fixture = Fixture::start(site);
    let home = Home::new("n3-r");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    up_with_fixtures(&home, s, "n3-r");
    let outside = outside_client(&home, s);

    let page = open_tab(&home, s, &fixture.url("/dynamic"));
    let before = loaded(&home, s, &["--json"], "button", "Swap");
    let (keep, swap) = (ref_of(&before, "button", "Keep"), ref_of(&before, "button", "Swap"));
    assert_eq!(resolve(&home, s, &format!("@{keep}")).unwrap().name, "Keep");
    assert_eq!(refused(&home, &["page", "resolve", s, "button"]).code, "BAD_REQUEST");
    assert_eq!(stale(&home, s, "e999999999").target.as_deref(), Some(page.as_str()), "a ref never issued is stale");

    run_script(
        &outside,
        &page,
        "document.getElementById('swap').replaceWith(Object.assign(document.createElement('button'), {id: 'swap', textContent: 'Swap'}));
         document.getElementById('keep').textContent = 'Kept';",
    );
    let replaced = stale(&home, s, &swap);
    assert!(replaced.message.contains("no longer in its document"), "{replaced:?}");
    eventually("the surviving node to show its new text", || (resolve(&home, s, &keep).unwrap().name == "Kept").then_some(()));
    let after = snapshot(&home, s, &["--json"]);
    assert_eq!(ref_of(&after, "button", "Kept"), keep);
    let new_swap = ref_of(&after, "button", "Swap");
    assert!(ref_number(&new_swap) > ref_number(&swap), "the replacement with the same text gets a new ref, never the old one");
    stale(&home, s, &swap);

    let other = open_tab(&home, s, &fixture.url("/dynamic"));
    let other_snapshot = loaded(&home, s, &["--json"], "button", "Swap");
    let foreign = stale(&home, s, &keep);
    assert_eq!(foreign.target.as_deref(), Some(other.as_str()));
    assert!(foreign.message.contains(&page), "{foreign:?}");
    let other_keep = ref_of(&other_snapshot, "button", "Keep");
    assert_ne!(other_keep, keep, "the same node in another tab has its own ref");
    assert_eq!(reply(&home, &["tab", "use", s, &page]).tabs.and_then(|t| t.selected).as_deref(), Some(page.as_str()));
    assert_eq!(resolve(&home, s, &keep).unwrap().name, "Kept", "a ref works again once its own tab is selected");
    stale(&home, s, &other_keep);

    run_script(&outside, &page, "location.reload()");
    let reloaded = eventually("the reload to commit", || resolve(&home, s, &keep).err());
    assert_eq!(reloaded.code, "STALE_REF");
    let navigated = loaded(&home, s, &["--json"], "button", "Keep");
    for old in [&keep, &new_swap] {
        stale(&home, s, old);
    }
    let fresh = ref_of(&navigated, "button", "Keep");
    assert!(ref_number(&fresh) > ref_number(&other_keep), "a new document's refs are new");

    let pid = home.daemon_pid();
    assert!(Command::new("kill").args(["-9", &pid]).status().unwrap().success());
    eventually("the daemon to die", || home.daemon_pid().is_empty().then_some(()));
    let gone = stale(&home, s, &fresh);
    assert!(gone.message.contains("not issued"), "{gone:?}");
    let restarted = snapshot(&home, s, &["--json"]);
    let renewed = ref_of(&restarted, "button", "Keep");
    let highest_before = [&keep, &swap, &new_swap, &other_keep, &fresh].iter().map(|r| ref_number(r)).max().unwrap();
    let issued_after: Vec<u64> = all(&restarted).iter().filter_map(|n| n.element.as_deref()).map(ref_number).collect();
    assert!(issued_after.iter().all(|&n| n > highest_before), "a restarted daemon reuses no ref: {issued_after:?} vs {highest_before}");
    assert_eq!(resolve(&home, s, &renewed).unwrap().name, "Keep");
    assert_ok(&home.bctl(&["down", s]));
}
