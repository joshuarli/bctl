mod common;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bctl_cdp::protocol::{CdpEvent, Command as CdpCommand, Effect, Empty, GetTargets, SessionId, SetDiscoverTargets, TargetDestroyed, TargetId};
use bctl_cdp::{Connection, EventFilter, EventScope, Scope};
use common::{Home, assert_ok, attach, outside_client, status_of};
use futures_lite::future::block_on;
use miniserde::{Deserialize, Serialize, json};

const ORIGIN: &str = "https://audio.test";

const WORKLET: &str = r#"
class PassThrough extends AudioWorkletProcessor {
  constructor() {
    super();
    this.quanta = 0;
  }
  process(inputs, outputs) {
    const input = inputs[0];
    const output = outputs[0];
    for (let channel = 0; channel < output.length; channel++) {
      const source = input[channel] || input[0];
      if (source) output[channel].set(source);
      else output[channel].fill(0);
    }
    if ((++this.quanta & 255) === 0) this.port.postMessage(this.quanta);
    return true;
  }
}
registerProcessor('passthrough', PassThrough);
"#;

const TONE: &str = r#"<!doctype html><title>Tone</title>
<script>
const workletSource = `__WORKLET__`;
let context = null;
let node = null;
let oscillator = null;
let quanta = 0;
window.startAudio = async () => {
  if (context) return JSON.stringify({ state: context.state, time: context.currentTime, quanta, worklet: !!node });
  context = new AudioContext({ sampleRate: 48000 });
  const source = URL.createObjectURL(new Blob([workletSource], { type: 'application/javascript' }));
  await context.audioWorklet.addModule(source);
  node = new AudioWorkletNode(context, 'passthrough', { numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [2] });
  node.port.onmessage = event => { quanta = event.data; };
  oscillator = new OscillatorNode(context, { type: 'sine', frequency: 220 });
  oscillator.connect(node).connect(context.destination);
  oscillator.start();
  await context.resume();
  return JSON.stringify({ state: context.state, time: context.currentTime, quanta, worklet: !!node });
};
window.probeAudio = () => JSON.stringify({
  state: context ? context.state : 'none',
  time: context ? context.currentTime : -1,
  quanta,
  worklet: !!node,
});
window.ready = () => typeof window.startAudio === 'function' ? 'ready' : 'waiting';
</script>"#;

const CONTENDED: &str = r#"<!doctype html><title>Contended</title>
<script>
const workerSource = `
  let spins = 0;
  function burn() {
    const end = performance.now() + 30;
    while (performance.now() < end) spins++;
    setTimeout(burn, 0);
  }
  burn();
  onmessage = () => postMessage(spins);
`;
window.workers = [];
for (let index = 0; index < 3; index++) {
  window.workers.push(new Worker(URL.createObjectURL(new Blob([workerSource], { type: 'application/javascript' }))));
}
window.probeContended = () => JSON.stringify({ workers: window.workers.length });
window.ready = () => typeof window.probeContended === 'function' ? 'ready' : 'waiting';
</script>"#;

fn body(path: &str) -> Option<String> {
    match path {
        "/tone" => Some(TONE.replace("__WORKLET__", WORKLET)),
        "/contended" => Some(CONTENDED.to_string()),
        _ => None,
    }
}

#[derive(Deserialize)]
struct Tabs {
    selected: Option<String>,
}

#[derive(Deserialize)]
struct TabsReply {
    error: Option<common::Error>,
    tabs: Option<Tabs>,
}

fn open_tab(home: &Home, session: &str) -> String {
    let output = home.bctl(&["tab", "new", session, "about:blank", "--json"]);
    assert_ok(&output);
    let reply: TabsReply = json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|_| panic!("tab new: {output:?}"));
    assert!(reply.error.is_none(), "tab new failed: {:?}", reply.error.map(|e| e.code));
    reply.tabs.and_then(|tabs| tabs.selected).unwrap_or_else(|| panic!("tab new selected nothing"))
}

#[derive(Serialize)]
struct Evaluate {
    expression: String,
    #[serde(rename = "returnByValue")]
    return_by_value: bool,
    #[serde(rename = "awaitPromise")]
    await_promise: bool,
    #[serde(rename = "userGesture")]
    user_gesture: bool,
}

impl CdpCommand for Evaluate {
    const METHOD: &'static str = "Runtime.evaluate";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Evaluated;
}

#[derive(Deserialize)]
struct Evaluated {
    result: Remote,
    #[serde(rename = "exceptionDetails")]
    exception_details: Option<ExceptionDetails>,
}

#[derive(Deserialize)]
struct Remote {
    value: Option<String>,
}

#[derive(Deserialize)]
struct ExceptionDetails {
    text: String,
    exception: Option<Exception>,
}

#[derive(Deserialize)]
struct Exception {
    description: Option<String>,
}

fn evaluate(outside: &Connection, session: &SessionId, expression: &str, user_gesture: bool, await_promise: bool) -> Result<String, String> {
    let command = Evaluate { expression: expression.to_string(), return_by_value: true, await_promise, user_gesture };
    let evaluated = block_on(outside.call(Scope::Session(session), &command, common::soon())).map_err(|error| format!("{error:?}"))?;
    if let Some(details) = evaluated.exception_details {
        return Err(format!("{expression} threw {}: {}", details.text, details.exception.and_then(|e| e.description).unwrap_or_default()));
    }
    evaluated.result.value.ok_or_else(|| format!("{expression} returned no value"))
}

fn must(outside: &Connection, session: &SessionId, expression: &str, user_gesture: bool, await_promise: bool) -> String {
    evaluate(outside, session, expression, user_gesture, await_promise).unwrap_or_else(|error| panic!("{expression}: {error:?}"))
}

#[derive(Deserialize, Debug)]
struct Audio {
    state: String,
    time: f64,
    quanta: f64,
    worklet: bool,
}

#[derive(Deserialize, Debug)]
struct Contended {
    workers: f64,
}

fn audio(outside: &Connection, session: &SessionId) -> Audio {
    json::from_str(&must(outside, session, "probeAudio()", false, false)).unwrap_or_else(|_| panic!("probeAudio did not return JSON"))
}

#[derive(Serialize)]
struct Pattern {
    #[serde(rename = "urlPattern")]
    url_pattern: String,
}

#[derive(Serialize)]
struct EnableFetch {
    patterns: Vec<Pattern>,
}

impl CdpCommand for EnableFetch {
    const METHOD: &'static str = "Fetch.enable";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Serialize)]
struct Navigate {
    url: String,
}

impl CdpCommand for Navigate {
    const METHOD: &'static str = "Page.navigate";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

#[derive(Deserialize)]
struct Paused {
    #[serde(rename = "requestId")]
    request_id: String,
    request: PausedRequest,
}

#[derive(Deserialize)]
struct PausedRequest {
    url: String,
}

impl CdpEvent for Paused {
    const METHOD: &'static str = "Fetch.requestPaused";
}

#[derive(Serialize)]
struct ResponseHeader {
    name: String,
    value: String,
}

#[derive(Serialize)]
struct Fulfill {
    #[serde(rename = "requestId")]
    request_id: String,
    #[serde(rename = "responseCode")]
    response_code: i32,
    #[serde(rename = "responseHeaders")]
    response_headers: Vec<ResponseHeader>,
    body: String,
}

impl CdpCommand for Fulfill {
    const METHOD: &'static str = "Fetch.fulfillRequest";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        encoded.push(TABLE[(bits >> 18) as usize] as char);
        encoded.push(TABLE[((bits >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 { TABLE[((bits >> 6) & 63) as usize] as char } else { '=' });
        encoded.push(if chunk.len() > 2 { TABLE[(bits & 63) as usize] as char } else { '=' });
    }
    encoded
}

fn serve(outside: &Connection, session: &SessionId, path: &'static str) {
    let mut events = outside.subscribe(EventFilter::new(&[Paused::METHOD], EventScope::Session(session.clone()))).expect("subscribe to fetch pauses");
    let outside = outside.clone();
    let session = session.clone();
    thread::spawn(move || {
        while let Ok(event) = block_on(events.next()) {
            let Ok(paused) = event.decode::<Paused>() else { continue };
            let requested = paused.request.url.strip_prefix(ORIGIN).unwrap_or(paused.request.url.as_str());
            let served = (requested == path).then(|| body(path)).flatten();
            let (code, mime, payload) = match served {
                Some(payload) => (200, "text/html; charset=utf-8", payload),
                None => (404, "text/plain", String::new()),
            };
            let command = Fulfill {
                request_id: paused.request_id,
                response_code: code,
                response_headers: vec![ResponseHeader { name: "Content-Type".into(), value: mime.into() }],
                body: base64(payload.as_bytes()),
            };
            if block_on(outside.call(Scope::Session(&session), &command, common::soon())).is_err() {
                break;
            }
        }
    });
}

fn navigate_to_page(outside: &Connection, session: &SessionId, path: &'static str) {
    serve(outside, session, path);
    let enable = EnableFetch { patterns: vec![Pattern { url_pattern: format!("{ORIGIN}/*") }] };
    block_on(outside.call(Scope::Session(session), &enable, common::soon())).unwrap();
    block_on(outside.call(Scope::Session(session), &Navigate { url: format!("{ORIGIN}{path}") }, common::soon())).unwrap();
}

#[derive(Deserialize)]
struct Crashed {
    #[serde(rename = "targetId")]
    target_id: String,
}

impl CdpEvent for Crashed {
    const METHOD: &'static str = "Target.targetCrashed";
}

#[derive(Clone, Debug)]
struct Observed {
    method: String,
    target: String,
    json: String,
}

fn crashed(witnessed: &Arc<Mutex<Vec<Observed>>>, targets: &[String]) -> Vec<Observed> {
    let events = witnessed.lock().unwrap();
    events.iter().filter(|event| targets.contains(&event.target)).cloned().collect()
}

fn describe(events: &[Observed]) -> String {
    events.iter().map(|event| format!("{} {} {}", event.method, event.target, event.json)).collect::<Vec<_>>().join("; ")
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn web_audio_worklet_threads_survive_a_minute_of_contended_playback() {
    let home = Home::new("m1-a");
    let session = format!("{}-s", home.prefix());
    let s = session.as_str();
    assert_ok(&home.up(s, "m1-a"));
    assert_eq!(status_of(&home.json(&["status"]), s).state.as_str(), "ready");

    let outside = outside_client(&home, s);
    block_on(outside.call(Scope::Browser, &SetDiscoverTargets { discover: true }, common::soon())).unwrap();
    let witnessed = Arc::new(Mutex::new(Vec::new()));
    {
        let witnessed = Arc::clone(&witnessed);
        let mut events = outside.subscribe(EventFilter::new(&[Crashed::METHOD, TargetDestroyed::METHOD], EventScope::Browser)).expect("subscribe to crashes");
        thread::spawn(move || {
            while let Ok(event) = block_on(events.next()) {
                let target = if event.method() == Crashed::METHOD {
                    event.decode::<Crashed>().ok().map(|crash| crash.target_id)
                } else {
                    event.decode::<TargetDestroyed>().ok().map(|destroyed| destroyed.target_id.to_string())
                };
                witnessed.lock().unwrap().push(Observed { method: event.method().to_string(), target: target.unwrap_or_default(), json: event.json().to_string() });
            }
        });
    }

    let toned = [open_tab(&home, s), open_tab(&home, s), open_tab(&home, s)];
    let contended = open_tab(&home, s);
    let targets: Vec<String> = toned.iter().chain(std::iter::once(&contended)).cloned().collect();
    let sessions: Vec<SessionId> = toned.iter().chain(std::iter::once(&contended)).map(|target| attach(&outside, target)).collect();
    let (tone_sessions, contended_session) = sessions.split_at(toned.len());

    for session in tone_sessions {
        navigate_to_page(&outside, session, "/tone");
    }
    navigate_to_page(&outside, &contended_session[0], "/contended");

    for (index, (target, session)) in toned.iter().zip(tone_sessions).enumerate() {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match evaluate(&outside, session, "ready()", false, false) {
                Ok(value) if value == "ready" => break,
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
                other => panic!("tone {index} on target {target} never loaded: {other:?}"),
            }
        }
        let started = must(&outside, session, "startAudio()", true, true);
        let started: Audio = json::from_str(&started).unwrap_or_else(|_| panic!("startAudio returned {started}"));
        assert_eq!(started.state, "running", "tone {index} on target {target} did not start: {started:?}");
        assert!(started.worklet, "tone {index} on target {target} created no AudioWorkletNode");
    }

    let contended_deadline = Instant::now() + Duration::from_secs(30);
    let workers: Contended = loop {
        match evaluate(&outside, &contended_session[0], "probeContended()", false, false) {
            Ok(value) => break json::from_str(&value).unwrap_or_else(|_| panic!("probeContended returned {value}")),
            _ if Instant::now() < contended_deadline => thread::sleep(Duration::from_millis(100)),
            other => panic!("the contended page never loaded: {other:?}"),
        }
    };
    assert_eq!(workers.workers, 3.0, "the contended page runs its workers");

    let mut first = [0.0; 3];
    let mut last = [0.0; 3];
    let mut quanta = [0.0; 3];
    for (index, session) in tone_sessions.iter().enumerate() {
        let probe = audio(&outside, session);
        assert_eq!(probe.state, "running");
        first[index] = probe.time;
        last[index] = probe.time;
        quanta[index] = probe.quanta;
    }

    let began = Instant::now();
    let mut rounds = 0;
    while began.elapsed() < Duration::from_secs(65) {
        thread::sleep(Duration::from_secs(5));
        rounds += 1;
        let crashed_now = crashed(&witnessed, &targets);
        assert!(crashed_now.is_empty(), "round {rounds} saw a target crash: {}", describe(&crashed_now));
        for (index, (session, target)) in tone_sessions.iter().zip(&toned).enumerate() {
            let probe = audio(&outside, session);
            assert_eq!(probe.state, "running", "tone {index} on target {target} left the running state: {probe:?}");
            assert!(probe.worklet, "tone {index} on target {target} lost its AudioWorkletNode: {probe:?}");
            assert!(probe.time > last[index], "tone {index} on target {target} audio time stalled at {}: {probe:?}", last[index]);
            assert!(probe.quanta > quanta[index], "tone {index} on target {target} rendered no AudioWorklet quanta: {probe:?}");
            last[index] = probe.time;
            quanta[index] = probe.quanta;
        }
        let text = must(&outside, &contended_session[0], "probeContended()", false, false);
        let contended: Contended = json::from_str(&text).unwrap_or_else(|_| panic!("probeContended returned {text}"));
        assert_eq!(contended.workers, 3.0, "round {rounds}: the contended page lost a worker");
    }
    let elapsed = began.elapsed();
    assert!(elapsed >= Duration::from_secs(60), "the load ran for {elapsed:?}");

    let crashed_all = crashed(&witnessed, &targets);
    assert!(crashed_all.is_empty(), "targets crashed during the run: {}", describe(&crashed_all));
    for (index, (session, target)) in tone_sessions.iter().zip(&toned).enumerate() {
        let probe = audio(&outside, session);
        assert!(probe.time > first[index], "tone {index} on target {target} advanced no audio time: first {} last {}", first[index], probe.time);
        assert!(probe.time - first[index] >= 20.0, "tone {index} on target {target} advanced only {}s of audio in {elapsed:?}", probe.time - first[index]);
    }
    let listed = block_on(outside.call(Scope::Browser, &GetTargets {}, common::soon())).unwrap();
    for target in &targets {
        assert!(
            listed.target_infos.iter().any(|info| info.target_id == TargetId::new(target.as_str())),
            "target {target} left the browser: {} remain",
            listed.target_infos.len()
        );
    }

    println!(
        "web audio: {rounds} rounds in {elapsed:?}, {:.1}s / {:.1}s / {:.1}s of audio time, {} quanta / {} quanta / {} quanta",
        last[0] - first[0],
        last[1] - first[1],
        last[2] - first[2],
        quanta[0],
        quanta[1],
        quanta[2]
    );
    assert_ok(&home.bctl(&["down", s]));
}
