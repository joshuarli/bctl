mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use common::{HOST_FROM_GUEST, Home, assert_ok, up_with_fixtures};

const ROUNDS: usize = 60;
const MODULES_PER_ROUND: usize = 30;
const MODULE_BYTES: usize = 250 * 1024;
const ROUND_TIMEOUT_MS: u64 = 10_000;

static MODULES_SERVED: AtomicUsize = AtomicUsize::new(0);

/// A keep-alive HTTP/1.1 server like `bctl-bench serve`: Chromium reuses its few connections for every
/// request, so a response that stops halfway leaves the request pending with nothing on the wire.
fn serve_keep_alive() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let _ = serve(stream);
            });
        }
    });
    port
}

fn serve(stream: TcpStream) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        loop {
            let mut header = String::new();
            reader.read_line(&mut header)?;
            if header.trim().is_empty() {
                break;
            }
        }
        let path = line.split(' ').nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
        let (content_type, body) = route(&path);
        write!(writer, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n", body.len())?;
        writer.write_all(&body)?;
        if path.starts_with("/m/") {
            MODULES_SERVED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn route(path: &str) -> (&'static str, Vec<u8>) {
    if let Some(module) = path.strip_prefix("/m/").and_then(|m| m.strip_suffix(".js")) {
        let mut body = format!("export const v{module} = \"").into_bytes();
        body.resize(MODULE_BYTES, b'x');
        body.extend_from_slice(b"\";\n");
        return ("text/javascript", body);
    }
    if path == "/frame.html" {
        let imports: String = (0..MODULES_PER_ROUND).map(|i| format!("import \"/m/{i}.js\";")).collect();
        return ("text/html", format!("<!doctype html><script type=\"module\">{imports} parent.postMessage(+new URLSearchParams(location.search).get('r'), '*');</script>").into_bytes());
    }
    let page = format!(
        "<!doctype html><title>start</title><script>
let ok = 0, lost = [], round = 0;
function next() {{
  if (round >= {ROUNDS}) {{ document.title = `done ok=${{ok}} lost=${{lost.join(',')}}`; return; }}
  const r = round++, frame = document.createElement('iframe');
  let settled = false;
  const settle = (loaded) => {{ if (settled) return; settled = true; loaded ? ok++ : lost.push(r); frame.remove(); next(); }};
  const timer = setTimeout(() => settle(false), {ROUND_TIMEOUT_MS});
  window.onmessage = (e) => {{ if (e.data === r) {{ clearTimeout(timer); settle(true); }} }};
  frame.src = `/frame.html?r=${{r}}`;
  document.body.appendChild(frame);
}}
onload = next;
</script><body></body>"
    );
    ("text/html", page.into_bytes())
}

fn title(home: &Home, session: &str) -> String {
    let output = home.bctl(&["page", "eval", session, "document.title"]);
    assert_ok(&output);
    String::from_utf8_lossy(&output.stdout).trim().trim_matches('"').to_string()
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn the_guest_fetches_every_large_response_over_reused_connections_to_the_host() {
    let port = serve_keep_alive();
    let home = Home::new("netloss");
    let session = format!("{}-s", home.prefix());
    up_with_fixtures(&home, &session, "netloss");
    assert_ok(&home.bctl(&["tab", "new", &session, &format!("http://{HOST_FROM_GUEST}:{port}/page.html")]));

    let deadline = Instant::now() + Duration::from_secs(ROUNDS as u64 * ROUND_TIMEOUT_MS / 1000 + 60);
    let finished = loop {
        let title = title(&home, &session);
        if title.starts_with("done") {
            break title;
        }
        assert!(Instant::now() < deadline, "the page never finished its rounds; last title {title:?}");
        thread::sleep(Duration::from_secs(1));
    };
    assert_eq!(finished, format!("done ok={ROUNDS} lost="), "rounds whose module scripts never arrived");
    assert!(MODULES_SERVED.load(Ordering::Relaxed) >= ROUNDS * MODULES_PER_ROUND, "every round fetches its modules afresh");
}
