mod mock;

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bctl_cdp::protocol::{
    AttachToTarget, FrameNavigated, GetTargets, GetVersion, SessionId, SetDiscoverTargets, TargetCreated, TargetId,
};
use bctl_cdp::{
    CallError, Connection, EventFilter, EventScope, Generation, MAX_MESSAGE_BYTES, MAX_PENDING_COMMANDS, Scope, SubscriptionEnd,
    Termination, Unobserved,
};
use futures_lite::future::{block_on, poll_once};
use mock::{Action, Inbound, Mock};

const VERSION: &str = r#"{"protocolVersion":"1.3","product":"HeadlessChrome/140","revision":"@r","userAgent":"ua","jsVersion":"13"}"#;

fn targets(id: &str) -> String {
    format!(r#"{{"targetInfos":[{{"targetId":"{id}","type":"page","title":"t","url":"about:blank","attached":false}}]}}"#)
}

fn target_info(id: &str) -> String {
    format!(r#"{{"targetId":"{id}","type":"page","title":"","url":"about:blank","attached":false}}"#)
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn connected() -> (Mock, Connection, JoinHandle<Termination>) {
    let mock = Mock::start();
    let (connection, driver) = block_on(bctl_cdp::connect(&mock.endpoint, Generation::new(7))).unwrap();
    assert_eq!(connection.generation(), Generation::new(7));
    (mock, connection, thread::spawn(move || block_on(driver)))
}

fn spawn_call<C, R>(connection: &Connection, scope: Option<SessionId>, command: C, deadline: Instant) -> JoinHandle<Result<R, CallError>>
where
    C: bctl_cdp::protocol::Command<Returns = R> + Send + 'static,
    R: Send + 'static,
{
    let connection = connection.clone();
    thread::spawn(move || {
        let scope = match &scope {
            Some(session) => Scope::Session(session),
            None => Scope::Browser,
        };
        block_on(connection.call(scope, &command, deadline))
    })
}

fn first_target(result: Result<bctl_cdp::protocol::Targets, CallError>) -> String {
    result.unwrap().target_infos[0].target_id.as_str().to_string()
}

async fn join_all<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut futures: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        for (future, output) in futures.iter_mut().zip(outputs.iter_mut()) {
            if output.is_none()
                && let Poll::Ready(value) = future.as_mut().poll(cx)
            {
                *output = Some(value);
            }
        }
        if outputs.iter().all(Option::is_some) { Poll::Ready(()) } else { Poll::Pending }
    })
    .await;
    outputs.into_iter().map(Option::unwrap).collect()
}

#[test]
fn reordered_replies_reach_their_own_callers() {
    let (mock, connection, _driver) = connected();
    let version = spawn_call(&connection, None, GetVersion {}, soon());
    let first = mock.request();
    let listing = spawn_call(&connection, None, GetTargets {}, soon());
    let second = mock.request();
    assert_eq!((first.method.as_str(), second.method.as_str()), ("Browser.getVersion", "Target.getTargets"));
    assert_ne!(first.id, second.id);

    mock.reply(second.id, &targets("second"));
    mock.reply(first.id, VERSION);
    assert_eq!(first_target(listing.join().unwrap()), "second");
    assert_eq!(version.join().unwrap().unwrap().product, "HeadlessChrome/140");
}

#[test]
fn interleaved_events_reach_only_matching_subscriptions_in_order() {
    let (mock, connection, _driver) = connected();
    let mut created = connection.subscribe(EventFilter::new(&["Target.targetCreated"], EventScope::Any)).unwrap();
    let mut navigated = connection.subscribe(EventFilter::new(&["Page.frameNavigated"], EventScope::Session(SessionId::new("S1")))).unwrap();
    let navigation = |session: &str, url: &str| {
        format!(r#"{{"method":"Page.frameNavigated","sessionId":"{session}","params":{{"frame":{{"id":"F","loaderId":"L","url":"{url}"}}}}}}"#)
    };

    let listing = spawn_call(&connection, None, GetTargets {}, soon());
    let request = mock.request();
    mock.send(format!(r#"{{"method":"Target.targetCreated","params":{{"targetInfo":{}}}}}"#, target_info("one")));
    mock.send(navigation("S2", "https://other.test/"));
    mock.send(navigation("S1", "https://a.test/"));
    mock.reply(request.id, &targets("listed"));
    mock.send(format!(r#"{{"method":"Target.targetCreated","params":{{"targetInfo":{}}}}}"#, target_info("two")));
    mock.send(navigation("S1", "https://b.test/"));

    assert_eq!(first_target(listing.join().unwrap()), "listed");
    let ids: Vec<String> = (0..2)
        .map(|_| block_on(created.next()).unwrap().decode::<TargetCreated>().unwrap().target_info.target_id.as_str().to_string())
        .collect();
    assert_eq!(ids, ["one", "two"]);
    let urls: Vec<String> = (0..2)
        .map(|_| {
            let event = block_on(navigated.next()).unwrap();
            assert_eq!(event.session(), Some(&SessionId::new("S1")));
            event.decode::<FrameNavigated>().unwrap().frame.url
        })
        .collect();
    assert_eq!(urls, ["https://a.test/", "https://b.test/"]);
}

#[test]
fn a_dropped_socket_fails_a_sent_mutation_as_unknown_and_a_read_as_disconnected() {
    let (mock, connection, driver) = connected();
    let mutation = spawn_call(&connection, None, SetDiscoverTargets { discover: true }, soon());
    assert_eq!(mock.request().method, "Target.setDiscoverTargets");
    let read = spawn_call(&connection, None, GetTargets {}, soon());
    mock.request();
    mock.act(Action::Drop);

    let termination = driver.join().unwrap();
    assert!(matches!(termination, Termination::Socket(_)), "{termination:?}");
    assert_eq!(
        mutation.join().unwrap().unwrap_err(),
        CallError::UnknownOutcome { method: "Target.setDiscoverTargets", cause: Unobserved::Disconnected(termination.clone()) }
    );
    assert_eq!(read.join().unwrap().unwrap_err(), CallError::Disconnected { method: "Target.getTargets", termination: termination.clone() });
    assert_eq!(block_on(connection.call(Scope::Browser, &SetDiscoverTargets { discover: true }, soon())), Err(CallError::Closed(termination)));
}

#[test]
fn a_reply_after_the_deadline_is_ignored_and_the_entry_is_gone() {
    let (mock, connection, _driver) = connected();
    let brief = || Instant::now() + Duration::from_millis(150);
    assert_eq!(block_on(connection.call(Scope::Browser, &GetTargets {}, brief())).unwrap_err(), CallError::Timeout { method: "Target.getTargets" });
    let read = mock.request();
    assert_eq!(
        block_on(connection.call(Scope::Browser, &SetDiscoverTargets { discover: true }, brief())).unwrap_err(),
        CallError::UnknownOutcome { method: "Target.setDiscoverTargets", cause: Unobserved::Deadline }
    );
    let mutation = mock.request();

    mock.reply(read.id, &targets("late"));
    mock.reply(mutation.id, "{}");
    let fresh = spawn_call(&connection, None, GetTargets {}, soon());
    let request = mock.request();
    mock.reply(request.id, &targets("fresh"));
    assert_eq!(first_target(fresh.join().unwrap()), "fresh");
}

#[test]
fn timed_out_and_cancelled_commands_release_their_pending_slots() {
    let (mock, connection, _driver) = connected();
    let deadline = Instant::now() + Duration::from_millis(300);
    let calls = (0..=MAX_PENDING_COMMANDS).map(|_| connection.call(Scope::Browser, &GetTargets {}, deadline)).collect();
    let results = block_on(join_all(calls));
    let (last, rest) = results.split_last().unwrap();
    assert_eq!(last.as_ref().unwrap_err(), &CallError::TooManyPending);
    assert!(rest.iter().all(|r| r.as_ref().unwrap_err() == &CallError::Timeout { method: "Target.getTargets" }));

    for _ in 0..MAX_PENDING_COMMANDS {
        let mut call = Box::pin(connection.call(Scope::Browser, &GetTargets {}, soon()));
        assert!(block_on(poll_once(&mut call)).is_none());
    }
    let fresh = spawn_call(&connection, None, GetVersion {}, soon());
    let id = loop {
        let request = mock.request();
        if request.method == "Browser.getVersion" {
            break request.id;
        }
    };
    mock.reply(id, VERSION);
    assert!(fresh.join().unwrap().is_ok());
}

#[test]
fn malformed_messages_fail_only_the_command_they_name() {
    let (mock, connection, driver) = connected();
    mock.send("not json at all");
    mock.send(r#"{"neither":"id nor method"}"#);

    let shape = spawn_call(&connection, None, GetVersion {}, soon());
    let request = mock.request();
    mock.send(format!(r#"{{"id":{},"result":{{"product":5}}}}"#, request.id));
    assert!(matches!(shape.join().unwrap(), Err(CallError::MalformedReply { method: "Browser.getVersion", .. })));

    let broken = spawn_call(&connection, None, GetVersion {}, soon());
    let request = mock.request();
    mock.send(format!(r#"{{"id":{},"result":{{]}}"#, request.id));
    assert!(matches!(broken.join().unwrap(), Err(CallError::MalformedReply { method: "Browser.getVersion", .. })));

    let refused = spawn_call(&connection, None, GetVersion {}, soon());
    let request = mock.request();
    mock.send(format!(r#"{{"id":{},"error":{{"code":-32601,"message":"'Browser.getVersion' wasn't found"}}}}"#, request.id));
    assert_eq!(
        refused.join().unwrap().unwrap_err(),
        CallError::Protocol { method: "Browser.getVersion", code: -32601, message: "'Browser.getVersion' wasn't found".into() }
    );

    let healthy = spawn_call(&connection, None, GetVersion {}, soon());
    let request = mock.request();
    mock.reply(request.id, VERSION);
    assert!(healthy.join().unwrap().is_ok());

    let pending = spawn_call(&connection, None, GetTargets {}, soon());
    mock.request();
    mock.act(Action::InvalidUtf8);
    let termination = driver.join().unwrap();
    assert!(matches!(termination, Termination::Protocol(_)), "{termination:?}");
    assert_eq!(pending.join().unwrap().unwrap_err(), CallError::Disconnected { method: "Target.getTargets", termination });
}

#[test]
fn concurrent_callers_each_receive_their_own_reply() {
    let (mock, connection, _driver) = connected();
    let callers: Vec<_> = (0..32).map(|i| (i, spawn_call(&connection, Some(SessionId::new(format!("s{i}"))), GetTargets {}, soon()))).collect();
    let mut requests: Vec<_> = (0..32).map(|_| mock.request()).collect();
    requests.reverse();
    for request in requests {
        let session = request.session.expect("every caller is session-scoped");
        mock.reply(request.id, &targets(&format!("for-{session}")));
    }
    for (i, caller) in callers {
        assert_eq!(first_target(caller.join().unwrap()), format!("for-s{i}"));
    }
}

#[test]
fn oversized_messages_are_refused_in_both_directions() {
    let (mock, connection, driver) = connected();
    let huge = AttachToTarget::new(TargetId::new("x".repeat(MAX_MESSAGE_BYTES)));
    assert!(matches!(
        block_on(connection.call(Scope::Browser, &huge, soon())),
        Err(CallError::RequestTooLarge { method: "Target.attachToTarget", .. })
    ));

    let read = spawn_call(&connection, None, GetTargets {}, soon());
    let request = mock.request();
    assert_eq!(request.method, "Target.getTargets", "the oversized command must never be sent");
    mock.send(format!(r#"{{"id":{},"result":{{"pad":"{}"}}}}"#, request.id, "x".repeat(MAX_MESSAGE_BYTES)));
    assert_eq!(driver.join().unwrap(), Termination::MessageTooLarge);
    assert_eq!(read.join().unwrap().unwrap_err(), CallError::Disconnected { method: "Target.getTargets", termination: Termination::MessageTooLarge });
}

#[test]
fn shutdown_closes_the_socket_and_resolves_everything_pending() {
    let (mock, connection, driver) = connected();
    let mut events = connection.subscribe(EventFilter::new(&["Target.targetCreated"], EventScope::Any)).unwrap();
    let mutation = spawn_call(&connection, None, SetDiscoverTargets { discover: true }, soon());
    mock.request();
    let read = spawn_call(&connection, None, GetTargets {}, soon());
    mock.request();

    assert_eq!(block_on(connection.shutdown()), Termination::Shutdown);
    assert!(matches!(mock.next(), Inbound::Close(Some(1000))));
    assert_eq!(driver.join().unwrap(), Termination::Shutdown);
    assert_eq!(
        mutation.join().unwrap().unwrap_err(),
        CallError::UnknownOutcome { method: "Target.setDiscoverTargets", cause: Unobserved::Disconnected(Termination::Shutdown) }
    );
    assert_eq!(read.join().unwrap().unwrap_err(), CallError::Disconnected { method: "Target.getTargets", termination: Termination::Shutdown });
    assert_eq!(block_on(events.next()).unwrap_err(), SubscriptionEnd::Closed(Termination::Shutdown));
    assert_eq!(block_on(connection.call(Scope::Browser, &GetTargets {}, soon())).unwrap_err(), CallError::Closed(Termination::Shutdown));
}

#[test]
fn flattened_sessions_route_commands_and_fail_with_their_detachment() {
    let (mock, connection, _driver) = connected();
    let attach = spawn_call(&connection, None, AttachToTarget::new(TargetId::new("T1")), soon());
    let request = mock.request();
    assert_eq!((request.method.as_str(), request.session.as_deref()), ("Target.attachToTarget", None));
    assert!(request.text.contains(r#""flatten":true"#), "{}", request.text);
    mock.reply(request.id, r#"{"sessionId":"S1"}"#);
    let session = attach.join().unwrap().unwrap().session_id;

    let read = spawn_call(&connection, Some(session.clone()), GetTargets {}, soon());
    assert_eq!(mock.request().session.as_deref(), Some("S1"));
    let mutation = spawn_call(&connection, Some(session.clone()), SetDiscoverTargets { discover: true }, soon());
    mock.request();
    let browser = spawn_call(&connection, None, GetVersion {}, soon());
    let browser_request = mock.request();

    mock.send(r#"{"method":"Target.detachedFromTarget","params":{"sessionId":"S1","targetId":"T1"}}"#);
    assert_eq!(read.join().unwrap().unwrap_err(), CallError::SessionDetached { method: "Target.getTargets", session });
    assert_eq!(
        mutation.join().unwrap().unwrap_err(),
        CallError::UnknownOutcome { method: "Target.setDiscoverTargets", cause: Unobserved::SessionDetached }
    );
    mock.reply(browser_request.id, VERSION);
    assert!(browser.join().unwrap().is_ok());
}

#[test]
fn a_full_subscription_ends_as_overflowed_after_its_queued_events() {
    let (mock, connection, _driver) = connected();
    let mut events = connection.subscribe(EventFilter::new(&["Target.targetCreated"], EventScope::Browser)).unwrap();
    let total = bctl_cdp::EVENT_QUEUE_CAPACITY + 5;
    for i in 0..total {
        mock.send(format!(r#"{{"method":"Target.targetCreated","params":{{"targetInfo":{}}}}}"#, target_info(&format!("t{i}"))));
    }
    let probe = spawn_call(&connection, None, GetVersion {}, soon());
    let request = mock.request();
    mock.reply(request.id, VERSION);
    probe.join().unwrap().unwrap();

    for i in 0..bctl_cdp::EVENT_QUEUE_CAPACITY {
        let created = block_on(events.next()).unwrap().decode::<TargetCreated>().unwrap();
        assert_eq!(created.target_info.target_id.as_str(), format!("t{i}"));
    }
    assert_eq!(block_on(events.next()).unwrap_err(), SubscriptionEnd::Overflowed);
}

#[test]
fn browser_pings_are_answered_and_a_browser_close_ends_the_connection() {
    let (mock, connection, driver) = connected();
    mock.act(Action::Ping);
    assert!(matches!(mock.next(), Inbound::Pong));
    let read = spawn_call(&connection, None, GetTargets {}, soon());
    mock.request();
    mock.act(Action::Close(1001));
    let termination = driver.join().unwrap();
    assert!(matches!(termination, Termination::PeerClosed { code: Some(1001), .. }), "{termination:?}");
    assert!(matches!(mock.next(), Inbound::Close(Some(1001))));
    assert_eq!(read.join().unwrap().unwrap_err(), CallError::Disconnected { method: "Target.getTargets", termination });
}

#[test]
fn a_browser_that_stops_answering_pings_is_declared_unresponsive() {
    let (mock, connection, driver) = connected();
    mock.ignore_pings();
    let started = Instant::now();
    let read = spawn_call(&connection, None, GetTargets {}, Instant::now() + Duration::from_secs(60));
    mock.request();
    assert_eq!(driver.join().unwrap(), Termination::Unresponsive);
    let silence = bctl_cdp::PING_INTERVAL + bctl_cdp::PONG_DEADLINE;
    assert!((silence..silence + Duration::from_secs(2)).contains(&started.elapsed()), "{:?}", started.elapsed());
    assert_eq!(read.join().unwrap().unwrap_err(), CallError::Disconnected { method: "Target.getTargets", termination: Termination::Unresponsive });
}
