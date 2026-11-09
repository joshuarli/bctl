use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use async_io::Timer;
use async_lock::Mutex as AsyncMutex;
use async_net::TcpListener;
use bctl_cdp::Endpoint;
use futures_lite::FutureExt;
use futures_lite::future::block_on;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture, FnExecutor};
use h12tiny::server::conn::auto;
use h12tiny::web::{Router, WebSocketFrame, WebSocketOpCode, WebSocketPayload, WebSocketUpgrade, get};
use miniserde::{Deserialize, json};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct Wire {
    id: u64,
    method: String,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

#[derive(Debug)]
pub struct Request {
    pub id: u64,
    pub method: String,
    pub session: Option<String>,
    pub text: String,
}

#[derive(Debug)]
pub enum Inbound {
    Command(Request),
    Pong,
    Close(Option<u16>),
    Other,
}

pub enum Action {
    Text(String),
    InvalidUtf8,
    Ping,
    Close(u16),
    Drop,
}

pub struct Mock {
    pub endpoint: Endpoint,
    inbound: Receiver<Inbound>,
    actions: Sender<Action>,
    silent: Arc<AtomicBool>,
}

impl Mock {
    pub fn start() -> Mock {
        let listener = block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (inbound_tx, inbound) = async_channel::unbounded();
        let (actions, actions_rx) = async_channel::unbounded();
        let silent = Arc::new(AtomicBool::new(false));
        let socket_silent = Arc::clone(&silent);
        thread::spawn(move || {
            block_on(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let router = Router::<()>::new().route(
                    "/devtools/browser/mock",
                    get(move |upgrade: WebSocketUpgrade| {
                        let (inbound_tx, actions_rx, silent) = (inbound_tx.clone(), actions_rx.clone(), Arc::clone(&socket_silent));
                        async move {
                            let response = upgrade.response();
                            thread::spawn(move || block_on(serve_socket(upgrade, inbound_tx, actions_rx, silent)));
                            response
                        }
                    }),
                );
                let executor = BoxExecutor::new(FnExecutor::new(|future: BoxSendFuture| {
                    thread::spawn(move || block_on(future));
                }));
                let _ = auto::Builder::new(executor).http1_only().serve_connection(FuturesIo::new(stream), router).await;
            });
        });
        let endpoint = Endpoint::parse(&format!("ws://127.0.0.1:{port}/devtools/browser/mock")).unwrap();
        Mock { endpoint, inbound, actions, silent }
    }

    pub fn next(&self) -> Inbound {
        let received = async { self.inbound.recv().await.ok() }.or(async {
            Timer::after(WAIT).await;
            None
        });
        block_on(received).expect("the client sent nothing")
    }

    pub fn request(&self) -> Request {
        match self.next() {
            Inbound::Command(request) => request,
            other => panic!("expected a command, got {other:?}"),
        }
    }

    pub fn send(&self, text: impl Into<String>) {
        self.act(Action::Text(text.into()));
    }

    pub fn reply(&self, id: u64, result: &str) {
        self.send(format!(r#"{{"id":{id},"result":{result}}}"#));
    }

    pub fn act(&self, action: Action) {
        self.actions.try_send(action).unwrap();
    }

    pub fn ignore_pings(&self) {
        self.silent.store(true, Ordering::Release);
    }
}

async fn serve_socket(upgrade: WebSocketUpgrade, inbound: Sender<Inbound>, actions: Receiver<Action>, silent: Arc<AtomicBool>) {
    let (mut reader, writer) = upgrade.into_connection().await.unwrap().split();
    let writer = AsyncMutex::new(writer);
    let reading = async {
        loop {
            let frame = reader
                .read_frame(&mut |reply| {
                    let writer = &writer;
                    let mute = silent.load(Ordering::Acquire) && reply.opcode == WebSocketOpCode::Pong;
                    async move {
                        if !mute {
                            let mut writer = writer.lock().await;
                            writer.write_frame(reply).await?;
                            writer.flush().await?;
                        }
                        Ok::<(), h12tiny::web::WebSocketError>(())
                    }
                })
                .await;
            let Ok(frame) = frame else { return };
            let message = match frame.opcode {
                WebSocketOpCode::Text => {
                    let text = String::from_utf8(frame.payload.to_vec()).unwrap();
                    match json::from_str::<Wire>(&text) {
                        Ok(wire) => Inbound::Command(Request { id: wire.id, method: wire.method, session: wire.session_id, text }),
                        Err(_) => Inbound::Other,
                    }
                }
                WebSocketOpCode::Pong => Inbound::Pong,
                WebSocketOpCode::Close => Inbound::Close(frame.payload.get(..2).map(|c| u16::from_be_bytes([c[0], c[1]]))),
                _ => Inbound::Other,
            };
            let _ = inbound.send(message).await;
        }
    };
    let acting = async {
        while let Ok(action) = actions.recv().await {
            let frame = match action {
                Action::Text(text) => WebSocketFrame::text(WebSocketPayload::Owned(text.into_bytes())),
                Action::InvalidUtf8 => WebSocketFrame::text(WebSocketPayload::Owned(vec![0xff, 0xfe])),
                Action::Ping => WebSocketFrame::ping(WebSocketPayload::Borrowed(b"mock")),
                Action::Close(code) => WebSocketFrame::close(code, b"mock"),
                Action::Drop => return,
            };
            let mut writer = writer.lock().await;
            if writer.write_frame(frame).await.is_err() || writer.flush().await.is_err() {
                return;
            }
        }
    };
    reading.or(acting).await;
}
