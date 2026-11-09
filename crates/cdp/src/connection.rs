use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Instant;

use async_channel::{Receiver, Sender, TrySendError};
use async_io::Timer;
use async_lock::Mutex as AsyncMutex;
use async_net::TcpStream;
use futures_lite::FutureExt;
use futures_lite::io::{AsyncRead, AsyncWrite};
use h12tiny::web::{
    WebSocketConnection, WebSocketError, WebSocketFrame, WebSocketOpCode, WebSocketPayload, WebSocketReader, WebSocketWriter,
    connect_websocket,
};
use miniserde::{Deserialize, json};

use crate::protocol::{CdpEvent, Command, DetachedFromTarget, Effect, SessionId};
use crate::{
    CLOSE_DEADLINE, CONNECT_DEADLINE, CallError, ConnectError, DecodeError, EVENT_QUEUE_CAPACITY, Endpoint, MAX_MESSAGE_BYTES,
    MAX_PENDING_COMMANDS, PING_INTERVAL, PONG_DEADLINE, SubscriptionEnd, Termination, Unobserved,
};

/// The owner's tag for the browser generation a connection belongs to. The
/// connection carries it and never interprets it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Generation(u64);

impl Generation {
    pub fn new(value: u64) -> Generation {
        Generation(value)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// Where a command runs: on the browser target or on a flattened session.
#[derive(Clone, Copy, Debug)]
pub enum Scope<'a> {
    Browser,
    Session(&'a SessionId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventScope {
    Any,
    Browser,
    Session(SessionId),
}

#[derive(Clone, Debug)]
pub struct EventFilter {
    methods: &'static [&'static str],
    scope: EventScope,
}

impl EventFilter {
    pub fn new(methods: &'static [&'static str], scope: EventScope) -> EventFilter {
        EventFilter { methods, scope }
    }

    fn matches(&self, method: &str, session: Option<&SessionId>) -> bool {
        let scoped = match &self.scope {
            EventScope::Any => true,
            EventScope::Browser => session.is_none(),
            EventScope::Session(wanted) => session == Some(wanted),
        };
        scoped && self.methods.contains(&method)
    }
}

#[derive(Debug)]
struct EventMessage {
    method: String,
    session: Option<SessionId>,
    text: Box<str>,
}

/// One CDP event, shared by every subscription it matched.
#[derive(Clone, Debug)]
pub struct Event(Arc<EventMessage>);

impl Event {
    pub fn method(&self) -> &str {
        &self.0.method
    }

    pub fn session(&self) -> Option<&SessionId> {
        self.0.session.as_ref()
    }

    pub fn json(&self) -> &str {
        &self.0.text
    }

    pub fn decode<E: CdpEvent>(&self) -> Result<E, DecodeError> {
        if self.0.method != E::METHOD {
            return Err(DecodeError::Method { expected: E::METHOD, actual: self.0.method.clone() });
        }
        decode_params::<E>(&self.0.text).ok_or_else(|| DecodeError::Malformed(self.0.method.clone()))
    }
}

pub struct Subscription {
    queue: Receiver<Event>,
    overflowed: Arc<AtomicBool>,
    shared: Arc<Shared>,
}

impl Subscription {
    /// The next matching event, in arrival order. Events queued before an
    /// overflow or termination are still delivered first.
    pub async fn next(&mut self) -> Result<Event, SubscriptionEnd> {
        match self.queue.recv().await {
            Ok(event) => Ok(event),
            Err(_) if self.overflowed.load(Ordering::Acquire) => Err(SubscriptionEnd::Overflowed),
            Err(_) => Err(SubscriptionEnd::Closed(
                self.shared.lock().termination.clone().expect("a subscription closes only on overflow or termination"),
            )),
        }
    }
}

struct Subscriber {
    filter: EventFilter,
    queue: Sender<Event>,
    overflowed: Arc<AtomicBool>,
}

struct Pending {
    method: &'static str,
    effect: Effect,
    session: Option<SessionId>,
    dispatched: bool,
    reply: Sender<Result<String, CallError>>,
}

impl Pending {
    fn unobserved(&self, cause: Unobserved) -> Option<CallError> {
        (self.dispatched && self.effect == Effect::Mutating).then_some(CallError::UnknownOutcome { method: self.method, cause })
    }

    fn fail(self, error: CallError) {
        let _ = self.reply.try_send(Err(error));
    }
}

struct State {
    pending: HashMap<u64, Pending>,
    subscribers: Vec<Subscriber>,
    stopping: bool,
    termination: Option<Termination>,
    last_inbound: Instant,
}

struct Shared {
    generation: Generation,
    next_id: AtomicU64,
    state: Mutex<State>,
    ended: Receiver<()>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

struct Outgoing {
    id: u64,
    text: String,
}

/// A handle to one browser websocket. Clones share the connection; when the
/// last clone is dropped the connection shuts down.
#[derive(Clone)]
pub struct Connection {
    shared: Arc<Shared>,
    outgoing: Sender<Outgoing>,
}

/// The connection's actor. Poll it to completion on any executor; it resolves
/// once the socket is gone and every pending command has been answered.
pub struct Driver(Pin<Box<dyn Future<Output = Termination> + Send>>);

impl Future for Driver {
    type Output = Termination;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Termination> {
        self.0.as_mut().poll(cx)
    }
}

/// Dials the endpoint over TCP and opens the browser websocket.
pub async fn connect(endpoint: &Endpoint, generation: Generation) -> Result<(Connection, Driver), ConnectError> {
    let deadline = Instant::now() + CONNECT_DEADLINE;
    let (host, port) = endpoint.dial_target();
    let stream = async { TcpStream::connect((host.as_str(), port)).await.map_err(ConnectError::Dial) }.or(expire(deadline)).await?;
    stream.set_nodelay(true).map_err(ConnectError::Dial)?;
    establish(stream, endpoint, generation, deadline).await
}

/// Opens the browser websocket over a stream the caller dialed, such as a
/// Unix socket forwarded to the browser's DevTools port.
pub async fn open<S>(stream: S, endpoint: &Endpoint, generation: Generation) -> Result<(Connection, Driver), ConnectError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    establish(stream, endpoint, generation, Instant::now() + CONNECT_DEADLINE).await
}

async fn expire<T>(deadline: Instant) -> Result<T, ConnectError> {
    Timer::at(deadline).await;
    Err(ConnectError::Deadline)
}

async fn establish<S>(stream: S, endpoint: &Endpoint, generation: Generation, deadline: Instant) -> Result<(Connection, Driver), ConnectError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let websocket = async { connect_websocket(stream, endpoint.uri()).await.map_err(ConnectError::Handshake) }.or(expire(deadline)).await?;
    Ok(start(websocket, generation))
}

fn start(websocket: WebSocketConnection, generation: Generation) -> (Connection, Driver) {
    let (mut reader, writer) = websocket.split();
    reader.set_max_message_size(MAX_MESSAGE_BYTES);
    let writer = AsyncMutex::new(writer);
    let (outgoing, queued) = async_channel::bounded(MAX_PENDING_COMMANDS);
    let (ended_tx, ended) = async_channel::bounded(1);
    let shared = Arc::new(Shared {
        generation,
        next_id: AtomicU64::new(1),
        state: Mutex::new(State {
            pending: HashMap::new(),
            subscribers: Vec::new(),
            stopping: false,
            termination: None,
            last_inbound: Instant::now(),
        }),
        ended,
    });
    let mut finisher = Finisher { shared: Arc::clone(&shared), ended: Some(ended_tx) };
    let driven = Arc::clone(&shared);
    let driver = Driver(Box::pin(async move {
        let termination = read_loop(&driven, reader, &writer).or(write_loop(&driven, queued, &writer)).or(keepalive(&driven, &writer)).await;
        finisher.finish(termination)
    }));
    (Connection { shared, outgoing }, driver)
}

impl Connection {
    pub fn generation(&self) -> Generation {
        self.shared.generation
    }

    pub fn termination(&self) -> Option<Termination> {
        self.shared.lock().termination.clone()
    }

    /// Sends one command and waits for its reply until `deadline`. The command
    /// is written at most once; dropping this future before it is written
    /// keeps it off the socket.
    pub async fn call<C: Command>(&self, scope: Scope<'_>, command: &C, deadline: Instant) -> Result<C::Returns, CallError> {
        let method = C::METHOD;
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let text = request_text(id, method, scope, &json::to_string(command));
        if text.len() >= MAX_MESSAGE_BYTES {
            return Err(CallError::RequestTooLarge { method, bytes: text.len() });
        }
        let (reply, replied) = async_channel::bounded(1);
        {
            let mut state = self.shared.lock();
            if let Some(termination) = &state.termination {
                return Err(CallError::Closed(termination.clone()));
            }
            if state.stopping {
                return Err(CallError::Closed(Termination::Shutdown));
            }
            if state.pending.len() >= MAX_PENDING_COMMANDS {
                return Err(CallError::TooManyPending);
            }
            let session = match scope {
                Scope::Browser => None,
                Scope::Session(session) => Some(session.clone()),
            };
            state.pending.insert(id, Pending { method, effect: C::EFFECT, session, dispatched: false, reply });
        }
        let guard = PendingGuard { shared: &self.shared, id };
        let answered = async {
            let _ = self.outgoing.send(Outgoing { id, text }).await;
            replied.recv().await.ok()
        }
        .or(async {
            Timer::at(deadline).await;
            None
        })
        .await;
        let answer = match answered {
            Some(answer) => answer,
            None => match guard.take() {
                Some(pending) => return Err(pending.unobserved(Unobserved::Deadline).unwrap_or(CallError::Timeout { method })),
                None => replied.try_recv().unwrap_or(Err(CallError::Timeout { method })),
            },
        };
        let text = answer?;
        decode_result::<C::Returns>(&text).ok_or_else(|| CallError::MalformedReply { method, reason: "the result does not match its type".into() })
    }

    pub fn subscribe(&self, filter: EventFilter) -> Result<Subscription, CallError> {
        let mut state = self.shared.lock();
        if let Some(termination) = &state.termination {
            return Err(CallError::Closed(termination.clone()));
        }
        let (queue, events) = async_channel::bounded(EVENT_QUEUE_CAPACITY);
        let overflowed = Arc::new(AtomicBool::new(false));
        state.subscribers.push(Subscriber { filter, queue, overflowed: Arc::clone(&overflowed) });
        Ok(Subscription { queue: events, overflowed, shared: Arc::clone(&self.shared) })
    }

    /// Refuses new commands, sends nothing still queued, closes the websocket,
    /// and waits for the driver to resolve every pending command.
    pub async fn shutdown(&self) -> Termination {
        self.shared.lock().stopping = true;
        self.outgoing.close();
        self.closed().await
    }

    /// Waits until the driver has finished.
    pub async fn closed(&self) -> Termination {
        let _ = self.shared.ended.recv().await;
        self.shared.lock().termination.clone().expect("the driver records its termination before it finishes")
    }
}

struct PendingGuard<'a> {
    shared: &'a Shared,
    id: u64,
}

impl PendingGuard<'_> {
    fn take(&self) -> Option<Pending> {
        self.shared.lock().pending.remove(&self.id)
    }
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.take();
    }
}

struct Finisher {
    shared: Arc<Shared>,
    ended: Option<Sender<()>>,
}

impl Finisher {
    fn finish(&mut self, termination: Termination) -> Termination {
        let (termination, pending, subscribers) = {
            let mut state = self.shared.lock();
            let termination = if state.stopping { Termination::Shutdown } else { termination };
            state.termination = Some(termination.clone());
            (termination, std::mem::take(&mut state.pending), std::mem::take(&mut state.subscribers))
        };
        for pending in pending.into_values() {
            let error = pending
                .unobserved(Unobserved::Disconnected(termination.clone()))
                .unwrap_or(CallError::Disconnected { method: pending.method, termination: termination.clone() });
            pending.fail(error);
        }
        drop(subscribers);
        self.ended.take();
        termination
    }
}

impl Drop for Finisher {
    fn drop(&mut self) {
        if self.ended.is_some() {
            self.finish(Termination::DriverDropped);
        }
    }
}

fn request_text(id: u64, method: &str, scope: Scope<'_>, params: &str) -> String {
    let mut text = format!("{{\"id\":{id},\"method\":{}", json::to_string(method));
    if let Scope::Session(session) = scope {
        text.push_str(",\"sessionId\":");
        text.push_str(&json::to_string(session.as_str()));
    }
    text.push_str(",\"params\":");
    text.push_str(params);
    text.push('}');
    text
}

async fn send(writer: &AsyncMutex<WebSocketWriter>, frame: WebSocketFrame<'_>) -> Result<(), WebSocketError> {
    let mut writer = writer.lock().await;
    writer.write_frame(frame).await?;
    writer.flush().await
}

fn termination_of(error: WebSocketError) -> Termination {
    match error {
        WebSocketError::FrameTooLarge => Termination::MessageTooLarge,
        WebSocketError::IoError(_) | WebSocketError::UnexpectedEOF | WebSocketError::ConnectionClosed | WebSocketError::SendError(_) => {
            Termination::Socket(error.to_string())
        }
        error => Termination::Protocol(error.to_string()),
    }
}

async fn read_loop(shared: &Shared, mut reader: WebSocketReader, writer: &AsyncMutex<WebSocketWriter>) -> Termination {
    loop {
        let frame = match reader.read_frame(&mut |reply| send(writer, reply)).await {
            Ok(frame) => frame,
            Err(error) => return termination_of(error),
        };
        shared.lock().last_inbound = Instant::now();
        match frame.opcode {
            WebSocketOpCode::Text => match std::str::from_utf8(&frame.payload) {
                Ok(text) => dispatch(shared, text),
                Err(_) => return Termination::Protocol("a text message is not UTF-8".into()),
            },
            WebSocketOpCode::Close => {
                let code = frame.payload.get(..2).map(|code| u16::from_be_bytes([code[0], code[1]]));
                let reason = frame.payload.get(2..).map(|reason| String::from_utf8_lossy(reason).into_owned()).unwrap_or_default();
                return Termination::PeerClosed { code, reason };
            }
            WebSocketOpCode::Binary | WebSocketOpCode::Ping | WebSocketOpCode::Pong | WebSocketOpCode::Continuation => {}
        }
    }
}

async fn write_loop(shared: &Shared, queued: Receiver<Outgoing>, writer: &AsyncMutex<WebSocketWriter>) -> Termination {
    while let Ok(Outgoing { id, text }) = queued.recv().await {
        {
            let mut state = shared.lock();
            if state.stopping {
                continue;
            }
            match state.pending.get_mut(&id) {
                Some(pending) => pending.dispatched = true,
                None => continue,
            }
        }
        if let Err(error) = send(writer, WebSocketFrame::text(WebSocketPayload::Owned(text.into_bytes()))).await {
            return termination_of(error);
        }
    }
    shared.lock().stopping = true;
    if let Err(error) = send(writer, WebSocketFrame::close(1000, b"")).await {
        return termination_of(error);
    }
    Timer::after(CLOSE_DEADLINE).await;
    Termination::Shutdown
}

async fn keepalive(shared: &Shared, writer: &AsyncMutex<WebSocketWriter>) -> Termination {
    loop {
        let idle_since = shared.lock().last_inbound;
        Timer::at(idle_since + PING_INTERVAL).await;
        if shared.lock().last_inbound != idle_since {
            continue;
        }
        let pinged = Instant::now();
        if let Err(error) = send(writer, WebSocketFrame::ping(WebSocketPayload::Borrowed(b""))).await {
            return termination_of(error);
        }
        Timer::after(PONG_DEADLINE).await;
        if shared.lock().last_inbound < pinged {
            return Termination::Unresponsive;
        }
    }
}

#[derive(Deserialize)]
struct Inbound {
    id: Option<u64>,
    method: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<SessionId>,
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: i64,
    message: String,
}

enum Answer<'a> {
    Result(&'a str),
    Error(ErrorBody),
    Malformed,
}

fn dispatch(shared: &Shared, text: &str) {
    match json::from_str::<Inbound>(text) {
        Ok(Inbound { id: Some(id), error, .. }) => answer(shared, id, error.map_or(Answer::Result(text), Answer::Error)),
        Ok(Inbound { id: None, method: Some(method), session_id, .. }) => deliver(shared, method, session_id, text),
        Ok(Inbound { id: None, method: None, .. }) => {}
        Err(_) => {
            if let Some(id) = leading_id(text) {
                answer(shared, id, Answer::Malformed);
            }
        }
    }
}

/// Chrome serializes every reply with `id` as its first member, so a reply
/// whose body fails to parse (an unpaired surrogate escape, say) still names
/// the command it answers.
fn leading_id(text: &str) -> Option<u64> {
    let rest = text.strip_prefix("{\"id\":")?;
    let end = rest.find([',', '}'])?;
    rest[..end].parse().ok()
}

fn answer(shared: &Shared, id: u64, answer: Answer<'_>) {
    let Some(pending) = shared.lock().pending.remove(&id) else { return };
    let method = pending.method;
    let reply = match answer {
        Answer::Result(text) => Ok(text.to_owned()),
        Answer::Error(ErrorBody { code, message }) => Err(CallError::Protocol { method, code, message }),
        Answer::Malformed => Err(CallError::MalformedReply { method, reason: "the reply is not valid JSON".into() }),
    };
    let _ = pending.reply.try_send(reply);
}

fn deliver(shared: &Shared, method: String, session: Option<SessionId>, text: &str) {
    let detached = (method == DetachedFromTarget::METHOD).then(|| decode_params::<DetachedFromTarget>(text)).flatten();
    let event = Event(Arc::new(EventMessage { method, session, text: text.into() }));
    let mut state = shared.lock();
    if let Some(detached) = detached {
        let ids: Vec<u64> = state.pending.iter().filter(|(_, p)| p.session.as_ref() == Some(&detached.session_id)).map(|(id, _)| *id).collect();
        for id in ids {
            let pending = state.pending.remove(&id).expect("the id was just listed");
            let error = pending
                .unobserved(Unobserved::SessionDetached)
                .unwrap_or(CallError::SessionDetached { method: pending.method, session: detached.session_id.clone() });
            pending.fail(error);
        }
    }
    state.subscribers.retain(|subscriber| {
        if !subscriber.filter.matches(event.method(), event.session()) {
            return !subscriber.queue.is_closed();
        }
        match subscriber.queue.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                subscriber.overflowed.store(true, Ordering::Release);
                subscriber.queue.close();
                false
            }
            Err(TrySendError::Closed(_)) => false,
        }
    });
}

#[derive(Deserialize)]
struct ResultEnvelope<T> {
    result: T,
}

#[derive(Deserialize)]
struct ParamsEnvelope<T> {
    params: T,
}

fn decode_result<T: Deserialize>(text: &str) -> Option<T> {
    json::from_str::<ResultEnvelope<T>>(text).ok().map(|envelope| envelope.result)
}

fn decode_params<T: Deserialize>(text: &str) -> Option<T> {
    json::from_str::<ParamsEnvelope<T>>(text).ok().map(|envelope| envelope.params)
}
