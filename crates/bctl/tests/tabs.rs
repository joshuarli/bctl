mod common;

use std::process::Command;

use bctl_cdp::protocol::{AttachToTarget, Command as CdpCommand, Effect, Empty, TargetId};
use bctl_cdp::Scope;
use common::{Home, assert_ok, dashboard_viewer, eventually, http, outside_client, soon};
use futures_lite::future::block_on;
use miniserde::{Deserialize, Serialize, json};

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
struct Error {
    code: String,
    target: Option<String>,
    targets: Option<Vec<Tab>>,
}

#[derive(Deserialize, Debug)]
struct Reply {
    error: Option<Error>,
    tabs: Option<Tabs>,
}

fn tab(home: &Home, args: &[&str]) -> Reply {
    let mut argv = vec!["tab"];
    argv.extend_from_slice(args);
    argv.push("--json");
    let output = home.bctl(&argv);
    json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl {argv:?}: {output:?}"))
}

fn listed(home: &Home, args: &[&str]) -> Tabs {
    let reply = tab(home, args);
    match (reply.error, reply.tabs) {
        (None, Some(tabs)) => tabs,
        (error, _) => panic!("bctl tab {args:?} failed: {error:?}"),
    }
}

fn refused(home: &Home, args: &[&str]) -> Error {
    tab(home, args).error.unwrap_or_else(|| panic!("bctl tab {args:?} succeeded"))
}

fn ids(tabs: &[Tab]) -> Vec<&str> {
    let mut ids: Vec<&str> = tabs.iter().map(|t| t.id.as_str()).collect();
    ids.sort_unstable();
    ids
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

#[derive(Serialize)]
struct Evaluate {
    expression: &'static str,
    #[serde(rename = "userGesture")]
    user_gesture: bool,
}

impl CdpCommand for Evaluate {
    const METHOD: &'static str = "Runtime.evaluate";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n2_targets_are_listed_opened_selected_and_closed_without_the_binding_ever_moving_on_its_own() {
    let home = Home::new("n2-t");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    assert_ok(&home.up(s, "n2-t"));

    let initial = listed(&home, &["list", s]);
    assert_eq!(initial.selected, None, "nothing is selected until asked");
    let first = initial.targets.first().expect("a ready session shows a page").id.clone();

    let opened = listed(&home, &["new", s, "about:blank"]);
    let second = opened.selected.clone().expect("a new target is selected");
    assert_ne!(second, first);
    assert_eq!(ids(&opened.targets), ids(&listed(&home, &["list", s]).targets));
    assert!(opened.targets.iter().any(|t| t.id == first));

    assert_eq!(listed(&home, &["use", s, &first]).selected.as_deref(), Some(first.as_str()));
    assert_eq!(refused(&home, &["use", s, "0000"]).code, "NO_SUCH_TARGET");

    let outside = outside_client(&home, s);
    let attached = block_on(outside.call(Scope::Browser, &AttachToTarget::new(TargetId::new(&first)), soon())).unwrap();
    let popup_open = Evaluate { expression: "window.open('about:blank', '_blank') !== null", user_gesture: true };
    block_on(outside.call(Scope::Session(&attached.session_id), &popup_open, soon())).unwrap();
    let with_popup = eventually("the popup to appear", || Some(listed(&home, &["list", s])).filter(|t| t.targets.len() == 3));
    assert_eq!(with_popup.selected.as_deref(), Some(first.as_str()), "a popup must not move the binding");
    let popup = with_popup.targets.iter().map(|t| t.id.clone()).find(|id| *id != first && *id != second).unwrap();
    let closed = listed(&home, &["close", s, &popup]);
    assert_eq!((closed.selected.as_deref(), ids(&closed.targets)), (Some(first.as_str()), ids(&opened.targets)));

    block_on(outside.call(Scope::Browser, &CloseTarget { target_id: TargetId::new(&first) }, soon())).unwrap();
    let gone = eventually("the selected target to be gone", || tab(&home, &["list", s]).error);
    for gone in [gone, refused(&home, &["list", s])] {
        assert_eq!((gone.code.as_str(), gone.target.as_deref()), ("TARGET_GONE", Some(first.as_str())));
        assert_eq!(gone.targets.as_deref().map(ids), Some(vec![second.as_str()]), "recovery lists what remains and switches to none of it");
    }
    assert_eq!(refused(&home, &["close", s, &second]).code, "LAST_TARGET");
    assert_eq!(listed(&home, &["use", s, &second]).selected.as_deref(), Some(second.as_str()));
    assert_ok(&home.bctl(&["down", s]));
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn n2_the_binding_survives_a_daemon_kill_but_not_a_new_generation_and_waits_for_a_person_to_release_control() {
    let home = Home::new("n2-r");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    assert_ok(&home.up(s, "n2-r"));
    let selected = listed(&home, &["new", s]).selected.expect("a new target is selected");

    let pid = home.daemon_pid();
    assert!(Command::new("kill").args(["-9", &pid]).status().unwrap().success());
    eventually("the daemon to die", || home.daemon_pid().is_empty().then_some(()));
    let reattached = listed(&home, &["list", s]);
    assert_eq!(reattached.selected.as_deref(), Some(selected.as_str()), "a restarted daemon reattaches to the same target");

    let (port, address, as_viewer) = dashboard_viewer(&home);
    eventually("the restarted daemon to offer input control", || {
        (http(port, &address, &format!("POST /api/sessions/{s}/control"), &as_viewer).0 == 200).then_some(())
    });
    assert_eq!(refused(&home, &["new", s]).code, "HUMAN_IN_CONTROL");
    assert_eq!(refused(&home, &["close", s, &selected]).code, "HUMAN_IN_CONTROL");
    let during = listed(&home, &["list", s]);
    assert_eq!(ids(&during.targets), ids(&reattached.targets), "a refused command changes no page");
    let (status, response) = http(port, &address, &format!("POST /api/sessions/{s}/control/release"), &as_viewer);
    assert_eq!(status, 200, "{response}");
    let after = listed(&home, &["new", s]);
    assert_eq!(after.targets.len(), reattached.targets.len() + 1);

    assert_ok(&home.bctl(&["down", s]));
    assert_ok(&home.up(s, "n2-r"));
    let fresh = listed(&home, &["list", s]);
    assert_eq!(fresh.selected, None, "a new generation starts with nothing selected");
    let stale = after.selected.unwrap();
    assert!(!fresh.targets.iter().any(|t| t.id == stale));
    assert_eq!(refused(&home, &["use", s, &stale]).code, "NO_SUCH_TARGET");
    assert_ok(&home.bctl(&["down", s]));
}
