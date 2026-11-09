use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use bctl_core::{Endpoints, Generation, MachineName, SessionName};
use bctl_rfb::{Buttons, Client};
use bctl_runtime_smolvm::{InputSupport, LaunchSource, LaunchSpec, SmolVm, TemplateSpec};
use miniserde::{Deserialize, json};

const PAGE: &str = r#"<!doctype html><title>X1 template</title>
<style>
html, body { margin: 0; min-height: 100%; background: #cfe8ff }
#k { position: absolute; left: 100px; top: 150px; width: 900px; height: 400px; font-size: 32px; border: 4px solid #224466 }
</style>
<input id=k autofocus>
"#;

fn page_url() -> String {
    let mut encoded = String::new();
    for byte in PAGE.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("data:text/html,{encoded}")
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn cdp_dir(tag: &str) -> PathBuf {
    let dir = workspace().join(format!(".work/tmp/x1-cdp-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn template_spec(tag: &str) -> TemplateSpec {
    let root = workspace();
    let scratch = root.join(".work/tmp");
    TemplateSpec {
        machine: format!("x1-template-{}-{tag}", std::process::id()),
        image: root.join(".work/images/helium.tar"),
        checkpoint: scratch.join(format!("x1-template-{}-{tag}.checkpoint", std::process::id())),
        staging_dir: scratch.join(format!("x1-template-staging-{}-{tag}", std::process::id())),
        display: (1280, 800),
        memory_mib: 4096,
        cpus: 2,
        cdp_socket: cdp_dir("capture").join("s"),
    }
}

fn launch_spec(session: &SessionName, checkpoint: PathBuf, vnc_port: u16) -> LaunchSpec {
    LaunchSpec {
        machine: MachineName { session: session.clone(), generation: Generation(1) },
        source: LaunchSource::Checkpoint(checkpoint),
        profile_dir: None,
        endpoints: Endpoints { vnc_port },
        cdp_socket: cdp_dir("session").join("s"),
        display: (1280, 800),
        memory_mib: 4096,
        cpus: 2,
        allow_private: Vec::new(),
    }
}

fn cdp_http(socket: &Path, method: &str, path: &str) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("connect {}: {e}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n").map_err(|e| format!("send {method} {path}: {e}"))?;
    let mut response = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).map_err(|e| format!("read {method} {path}: {e}"))?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&response);
        let Some((head, body)) = text.split_once("\r\n\r\n") else { continue };
        let status: u16 = head.lines().next().and_then(|line| line.split(' ').nth(1)).and_then(|s| s.parse().ok()).ok_or_else(|| format!("bad status line in {head:?}"))?;
        let length: usize = head
            .lines()
            .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("{method} {path} answered without Content-Length: {head:?}"))?;
        if body.len() < length {
            continue;
        }
        if status != 200 {
            return Err(format!("{method} {path} answered {status}: {}", &body[..length]));
        }
        return Ok(body[..length].to_string());
    }
    Err(format!("{method} {path} closed without a complete response"))
}

#[derive(Deserialize, Debug)]
struct Target {
    #[serde(rename = "type")]
    kind: String,
    title: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    websocket: Option<String>,
}

fn targets(socket: &Path) -> Result<Vec<Target>, String> {
    let body = cdp_http(socket, "GET", "/json/list")?;
    json::from_str(&body).map_err(|_| format!("/json/list returned {body}"))
}

fn find_title(socket: &Path, wanted: &str) -> Result<Target, String> {
    targets(socket)?.into_iter().find(|target| target.kind == "page" && target.title == wanted).ok_or_else(|| format!("no page titled {wanted:?}"))
}

fn wait_for_title(socket: &Path, wanted: &str) -> Target {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match find_title(socket, wanted) {
            Ok(target) => return target,
            Err(_) if Instant::now() >= deadline => panic!("timed out waiting for the page title {wanted:?}: {:?}", targets(socket)),
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn field<'a>(value: &'a json::Value, key: &str) -> Option<&'a json::Value> {
    match value {
        json::Value::Object(fields) => fields.get(key),
        _ => None,
    }
}

struct Cdp {
    stream: UnixStream,
    next_id: u64,
}

impl Cdp {
    fn connect(socket: &Path, url: &str) -> Result<Cdp, String> {
        let resource = url.strip_prefix("ws://").and_then(|rest| rest.find('/').map(|at| &rest[at..])).ok_or_else(|| format!("unexpected websocket url {url}"))?;
        let mut stream = UnixStream::connect(socket).map_err(|e| format!("connect {}: {e}", socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
        write!(
            stream,
            "GET {resource} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .map_err(|e| format!("websocket upgrade: {e}"))?;
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).map_err(|e| format!("read websocket head: {e}"))?;
            head.push(byte[0]);
        }
        if !String::from_utf8_lossy(&head).starts_with("HTTP/1.1 101") {
            return Err(format!("websocket upgrade answered {}", String::from_utf8_lossy(&head)));
        }
        Ok(Cdp { stream, next_id: 0 })
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mask = [0x37u8, 0xfa, 0x21, 0x3d];
        let mut frame = vec![0x80 | opcode];
        let length = payload.len();
        if length < 126 {
            frame.push(0x80 | length as u8);
        } else if length <= usize::from(u16::MAX) {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(length as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&frame).map_err(|e| format!("websocket send: {e}"))
    }

    fn receive(&mut self) -> Result<Vec<u8>, String> {
        let mut message = Vec::new();
        loop {
            let mut head = [0u8; 2];
            self.stream.read_exact(&mut head).map_err(|e| format!("websocket read: {e}"))?;
            let (fin, opcode) = (head[0] & 0x80 != 0, head[0] & 0x0f);
            let mut length = u64::from(head[1] & 0x7f);
            if length == 126 {
                let mut extended = [0u8; 2];
                self.stream.read_exact(&mut extended).map_err(|e| format!("websocket read: {e}"))?;
                length = u64::from(u16::from_be_bytes(extended));
            } else if length == 127 {
                let mut extended = [0u8; 8];
                self.stream.read_exact(&mut extended).map_err(|e| format!("websocket read: {e}"))?;
                length = u64::from_be_bytes(extended);
            }
            let mut payload = vec![0u8; length as usize];
            self.stream.read_exact(&mut payload).map_err(|e| format!("websocket read: {e}"))?;
            match opcode {
                0x0..=0x2 => {
                    message.extend_from_slice(&payload);
                    if fin {
                        return Ok(message);
                    }
                }
                0x8 => return Err("the CDP websocket closed".into()),
                0x9 => self.send(0xa, &payload)?,
                _ => {}
            }
        }
    }

    fn call(&mut self, method: &str, params: &str) -> Result<json::Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(0x1, format!(r#"{{"id":{id},"method":"{method}","params":{params}}}"#).as_bytes())?;
        loop {
            let message = self.receive()?;
            let value: json::Value = json::from_str(&String::from_utf8_lossy(&message)).map_err(|_| format!("{method}: bad JSON reply"))?;
            let answers = matches!(field(&value, "id"), Some(json::Value::Number(json::Number::U64(n))) if *n == id);
            if !answers {
                continue;
            }
            if let Some(error) = field(&value, "error") {
                return Err(format!("{method} failed: {}", json::to_string(error)));
            }
            return Ok(field(&value, "result").cloned().unwrap_or(json::Value::Null));
        }
    }

    fn evaluate(&mut self, expression: &str) -> Result<String, String> {
        let params = format!(r#"{{"expression":{},"returnByValue":true}}"#, json::to_string(&json::Value::String(expression.into())));
        let result = self.call("Runtime.evaluate", &params)?;
        if let Some(exception) = field(&result, "exceptionDetails") {
            return Err(format!("evaluating {expression:?} threw {}", json::to_string(exception)));
        }
        let value = field(&result, "result").cloned().unwrap_or(json::Value::Null);
        match field(&value, "value") {
            Some(json::Value::String(text)) => Ok(text.clone()),
            Some(other) => Ok(json::to_string(other)),
            None => Ok(String::new()),
        }
    }
}

fn click_into_focus(vnc: &mut Client, cdp: &mut Cdp) -> Result<(), bctl_runtime_smolvm::RuntimeError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if cdp.evaluate("document.hasFocus()").is_ok_and(|focused| focused == "true") {
            return Ok(());
        }
        vnc.click(640, 500).map_err(|e| bctl_runtime_smolvm::RuntimeError::Spawn { message: e.to_string() })?;
        thread::sleep(Duration::from_millis(300));
        if Instant::now() >= deadline {
            return Err(bctl_runtime_smolvm::RuntimeError::Spawn { message: "the page never took focus after an RFB click".into() });
        }
    }
}

fn wait_for_value(cdp: &mut Cdp, wanted: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match cdp.evaluate("document.getElementById('k').value") {
            Ok(value) if value == wanted => return,
            Ok(value) if Instant::now() >= deadline => panic!("the page never received {wanted:?}; its input holds {value:?}"),
            Ok(_) => thread::sleep(Duration::from_millis(200)),
            Err(error) if Instant::now() >= deadline => panic!("timed out reading the page: {error}"),
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    }
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the helium image in .work/images"]
fn a_session_restored_from_a_pre_browser_template_reaches_ready_with_the_sandbox_intact() {
    let template = template_spec("restore");
    let runtime = SmolVm::new(workspace().join(".work/runtime/smolvm/smolvm"));
    runtime.create_template(&template).unwrap_or_else(|e| panic!("create template: {e}"));
    assert!(!runtime.list().unwrap().iter().any(|m| m.name == template.machine), "the template source is deleted after capture");

    let session = SessionName::new(&format!("restore-{}", std::process::id())).unwrap();
    let spec = launch_spec(&session, template.checkpoint.clone(), free_port());
    runtime.launch(&spec).unwrap_or_else(|e| panic!("launch the restored session: {e}"));

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ready = runtime.wait_ready(&spec.machine, spec.endpoints, &spec.cdp_socket, Duration::from_secs(180), |stage| {
            eprintln!("restore: waiting at {}", stage.as_str());
        })?;
        assert_eq!(ready.input, InputSupport::Interactive);
        assert!(ready.browser.starts_with("Chrome/"), "{}", ready.browser);

        let target = targets(&spec.cdp_socket).unwrap_or_else(|e| panic!("list targets: {e}")).into_iter().find(|target| target.kind == "page").expect("the appliance has a page");
        let websocket = target.websocket.expect("the page target has a websocket");
        let mut cdp = Cdp::connect(&spec.cdp_socket, &websocket).unwrap_or_else(|e| panic!("connect to the page: {e}"));
        let url = json::to_string(&json::Value::String(page_url().into()));
        cdp.call("Page.navigate", &format!(r#"{{"url":{url}}}"#)).unwrap_or_else(|e| panic!("navigate the page: {e}"));
        wait_for_title(&spec.cdp_socket, "X1 template");

        let mut vnc = Client::connect(("127.0.0.1", spec.endpoints.vnc_port), Duration::from_secs(10)).map_err(|e| bctl_runtime_smolvm::RuntimeError::Spawn { message: e.to_string() })?;
        let frame: bctl_rfb::Frame = vnc.refresh().map_err(|e| bctl_runtime_smolvm::RuntimeError::Spawn { message: e.to_string() })?.clone();
        assert_eq!((frame.width(), frame.height()), (1280, 800));
        let black = bctl_rfb::Rgb(0, 0, 0);
        let drawn = (0..frame.height()).step_by(8).flat_map(|y| (0..frame.width()).step_by(8).map(move |x| (x, y))).filter(|&(x, y)| frame.pixel(x, y) != black).count();
        assert!(drawn > 1000, "the restored session's frame is not drawn ({drawn} sampled pixels)");
        let page = frame.pixel(1200, 700);
        assert!(page.0.abs_diff(0xcf) < 24 && page.1.abs_diff(0xe8) < 24 && page.2.abs_diff(0xff) < 24, "the restored session does not show the data page: {page:?}");

        click_into_focus(&mut vnc, &mut cdp)?;
        vnc.type_text("x1").map_err(|e| bctl_runtime_smolvm::RuntimeError::Spawn { message: e.to_string() })?;
        wait_for_value(&mut cdp, "x1");
        vnc.pointer(640, 500, Buttons::NONE).map_err(|e| bctl_runtime_smolvm::RuntimeError::Spawn { message: e.to_string() })?;

        let report = runtime.guest_check(&spec.machine)?;
        assert_eq!(report.first_missing(), None);
        assert!(report.stages.iter().any(|(stage, ok)| *stage == bctl_runtime_smolvm::Stage::Browser && *ok), "browser ok");
        assert!(report.stages.iter().any(|(stage, ok)| *stage == bctl_runtime_smolvm::Stage::Sandbox && *ok), "sandbox ok");
        assert_eq!(report.input, InputSupport::Interactive, "input ok");
        Ok::<(), bctl_runtime_smolvm::RuntimeError>(())
    }));

    let removed = runtime.remove(&spec.machine);
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    outcome.unwrap();
    removed.unwrap();
}
