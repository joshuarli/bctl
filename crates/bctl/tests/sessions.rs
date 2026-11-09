mod common;

use miniserde::{Deserialize, json};

use common::{Home, assert_ok, status_of};

#[derive(Deserialize)]
struct Action {
    value: Option<json::Value>,
}

#[derive(Deserialize)]
struct Page {
    action: Option<Action>,
}

#[derive(Deserialize)]
struct Reply {
    error: Option<common::Error>,
    page: Option<Page>,
}

fn eval(home: &Home, session: &str, expression: &str) -> Reply {
    let output = home.bctl(&["page", "eval", session, expression, "--json"]);
    json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("bctl page eval {session} {expression}: {output:?}"))
}

fn evaluated(reply: Reply) -> f64 {
    let value = match (reply.error, reply.page.and_then(|page| page.action).and_then(|action| action.value)) {
        (None, Some(value)) => value,
        (error, value) => panic!(
            "expected an evaluated value, got error {:?} and value {}",
            error.map(|error| error.code),
            json::to_string(&value.unwrap_or(json::Value::Null))
        ),
    };
    match value {
        json::Value::Number(json::Number::F64(number)) => number,
        json::Value::Number(json::Number::U64(number)) => number as f64,
        other => panic!("not a number: {}", json::to_string(&other)),
    }
}

fn state(home: &Home, session: &str) -> String {
    let response = home.json(&["inspect", session]);
    status_of(&response, session).state.clone()
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn a_paused_session_keeps_its_browser_state_and_page_binding_across_resume() {
    let home = Home::new("sp");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    assert_ok(&home.up(s, "sp"));
    assert_ok(&home.bctl(&["tab", "new", s, "about:blank"]));

    assert_eq!(evaluated(eval(&home, s, "window.__mark = 42")), 42.0);

    assert_ok(&home.bctl(&["pause", s]));
    assert_eq!(state(&home, s), "paused");
    let Reply { error, page } = eval(&home, s, "window.__mark");
    assert_eq!(error.map(|error| error.code), Some("SESSION_NOT_READY".to_string()));
    assert!(page.is_none());

    assert_ok(&home.bctl(&["resume", s]));
    assert_eq!(state(&home, s), "ready");
    assert_eq!(evaluated(eval(&home, s, "window.__mark")), 42.0, "the selected target and its JS globals survive resume");

    assert_ok(&home.bctl(&["pause", s]));
    assert_eq!(home.bctl_machines().len(), 1);
    assert_ok(&home.bctl(&["down", s]));
    assert!(home.bctl_machines().is_empty());
}
