use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use bctl_core::{
    Action, AllowedRange, AttemptEnd, AttemptRecord, AttemptState, Endpoints, Generation, Interaction, LiveState, MachineName, MachineState, NewRun,
    Outcome as RunOutcome, ProfileName, Progress, Resources, RunKind, RunRecord, SessionName, SessionRecord, SessionState, Store, StoreError, reconcile,
};
use bctl_runtime::{InputSupport, cdp_socket_browser};
use bctl_runtime_smolvm::Removal;
use miniserde::json;

use crate::authority::{Authorities, Control};
use crate::gateway::{self, SharedGateway};
use crate::paths::Paths;
use crate::runtime::{MachineLaunch, Runtime, RuntimeError, RuntimeKind};
use crate::protocol::{
    AttemptView, ErrorCode, InteractionView, JobRequest, LogView, Op, ProfileView, ProgressView, Request, ResourcesView, Response, RunView, SessionView, SessionsPayload,
};
use crate::runs::{self, BoundedLog, Intent, Launch, Ledger, Live, RunConfig, RunFiles};
use crate::page::{self, PageCommand};
use crate::tabs::{self, Browsers, TabCommand};

const READY_TIMEOUT: Duration = Duration::from_secs(180);
const STOP_RUN_TIMEOUT: Duration = Duration::from_secs(30);
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);
const HEALTH_FAILURES: u32 = 3;
const FAULT_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const GC_INTERVAL: Duration = Duration::from_secs(60);
const FAILING_RETRY_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Observed {
    stage: Option<&'static str>,
    input: Option<InputSupport>,
}

pub(crate) struct Daemon {
    paths: Paths,
    store: Mutex<Store>,
    runtime: Runtime,
    observed: Mutex<HashMap<SessionName, Observed>>,
    live: Mutex<HashMap<SessionName, Arc<Live>>>,
    busy: Mutex<HashSet<SessionName>>,
    tearing_down: Mutex<HashSet<SessionName>>,
    teardown_done: Condvar,
    disk_digests: runs::DiskDigests,
    pub(crate) gateway: SharedGateway,
    pub(crate) browsers: Browsers,
    pub(crate) authority: Authorities,
}

pub(crate) struct Refusal(pub(crate) Box<Response>);

impl From<Response> for Refusal {
    fn from(response: Response) -> Self {
        Refusal(Box::new(response))
    }
}

type Outcome = Result<Response, Refusal>;

pub fn run(paths: Paths) -> Result<(), String> {
    fs::create_dir_all(&paths.home).map_err(|e| format!("cannot create {}: {e}", paths.home.display()))?;
    fs::set_permissions(&paths.home, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(paths.lock())
        .map_err(|e| format!("cannot open {}: {e}", paths.lock().display()))?;
    if lock.try_lock().is_err() {
        return Err(format!("another bctl daemon owns {}", paths.home.display()));
    }

    let store = Store::open(&paths.database()).map_err(|e| e.to_string())?;
    let runtime = Runtime::new(RuntimeKind::for_host()?, &paths)?;
    let daemon = Arc::new(Daemon {
        runtime,
        paths,
        store: Mutex::new(store),
        observed: Mutex::default(),
        live: Mutex::default(),
        busy: Mutex::default(),
        tearing_down: Mutex::default(),
        teardown_done: Condvar::new(),
        disk_digests: runs::DiskDigests::default(),
        gateway: Mutex::default(),
        browsers: Mutex::default(),
        authority: Authorities::default(),
    });
    match daemon.runtime.installed(&daemon.paths) {
        Ok(()) => daemon.restore()?,
        Err((missing, _)) => eprintln!(
            "bctl daemon: the {} runtime has no {}; sessions cannot start until it exists",
            daemon.runtime.kind().as_str(),
            missing.display()
        ),
    }
    daemon.collect_garbage();
    let watcher = Arc::clone(&daemon);
    thread::spawn(move || watcher.watch_health());

    let socket = daemon.paths.socket();
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).map_err(|e| format!("cannot listen on {}: {e}", socket.display()))?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    eprintln!("bctl daemon: listening on {}", socket.display());
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.serve(stream));
    }
    drop(lock);
    Ok(())
}

fn internal(error: impl std::fmt::Display) -> Response {
    Response::failure(ErrorCode::Internal, error.to_string(), None)
}

fn store_failure(error: StoreError) -> Response {
    match error {
        StoreError::NoSuchSession(name) => {
            Response::failure(ErrorCode::NoSuchSession, format!("no session named {name}"), Some("see `bctl status`".into()))
        }
        StoreError::ProfileBusy { profile, holder } => Response::failure(
            ErrorCode::ProfileBusy,
            format!("profile {profile} is in use by session {holder}"),
            Some(format!("stop it with `bctl down {holder}` or choose another --profile")),
        ),
        StoreError::NotStartable { session, state } => Response::failure(
            ErrorCode::NotStopped,
            format!("session {session} is {}", state.as_str()),
            Some(format!("run `bctl down {session}` first")),
        ),
        StoreError::NoSuchRun(run) => Response::failure(ErrorCode::NoSuchRun, format!("no run {run}"), Some("see `bctl runs`".into())),
        StoreError::NotResumable { run, state } => Response::failure(
            ErrorCode::NotResumable,
            format!("run {run} is {}, not paused or interrupted", state.as_str()),
            Some("start a new run instead".into()),
        ),
        other => internal(other),
    }
}

fn bad_request(message: impl Into<String>) -> Response {
    Response::failure(ErrorCode::BadRequest, message, None)
}

fn resources_view(resources: Resources) -> ResourcesView {
    ResourcesView { width: resources.viewport[0], height: resources.viewport[1], cpus: resources.cpus, memory_mib: resources.memory_mib }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct RunLedger {
    daemon: Arc<Daemon>,
    session: SessionName,
    cdp_url: String,
}

impl RunLedger {
    fn record(&self, attempt: i64, result: Result<(), StoreError>) {
        if let Err(error) = result {
            eprintln!("bctl daemon: attempt {attempt}: {error}");
        }
    }
}

impl Ledger for RunLedger {
    fn set_pid(&self, attempt: i64, pid: u32) {
        self.record(attempt, self.daemon.store().set_pid(attempt, pid));
    }

    fn running(&self, attempt: i64) {
        self.daemon.authority.of(&self.session, Control::resume_run);
        self.record(attempt, self.daemon.store().set_live(attempt, LiveState::Running, None));
    }

    fn needs_interaction(&self, attempt: i64, interaction: &Interaction) {
        if let Some(controller) = gateway::cdp_url_controller(&self.cdp_url) {
            self.daemon.authority.of(&self.session, |control| control.stand_aside(&controller));
        }
        self.record(attempt, self.daemon.store().set_live(attempt, LiveState::NeedsInteraction, Some(interaction)));
    }

    fn progress(&self, attempt: i64, progress: Progress) {
        self.record(attempt, self.daemon.store().set_progress(attempt, progress));
    }

    fn browser_alive(&self) -> bool {
        self.daemon.cdp_socket(&self.session).is_some_and(|socket| cdp_socket_browser(&socket).is_some())
    }

    fn finish(&self, live: &Live, end: AttemptEnd) {
        let mut sessions = lock(&self.daemon.live);
        self.daemon.authority.of(&self.session, Control::run_ended);
        self.record(live.attempt, self.daemon.store().finish_attempt(live.attempt, &end, now_ms()));
        if sessions.get(&self.session).is_some_and(|current| current.attempt == live.attempt) {
            sessions.remove(&self.session);
        }
        drop(sessions);
        gateway::revoke_cdp_url(&self.daemon, &self.cdp_url);
        self.daemon.collect_garbage();
    }
}

pub(crate) struct Teardown<'a> {
    daemon: &'a Daemon,
    session: SessionName,
}

impl Drop for Teardown<'_> {
    fn drop(&mut self) {
        lock(&self.daemon.tearing_down).remove(&self.session);
        self.daemon.teardown_done.notify_all();
    }
}

pub(crate) struct Busy<'a> {
    daemon: &'a Daemon,
    session: SessionName,
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        lock(&self.daemon.busy).remove(&self.session);
    }
}

impl Daemon {
    pub(crate) fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn busy(&self, session: &SessionName) -> Busy<'_> {
        lock(&self.busy).insert(session.clone());
        Busy { daemon: self, session: session.clone() }
    }

    /// Serializes everything that decides whether a session's VM is gone: failure cleanup, stop, and failed-start cleanup.
    fn teardown(&self, session: &SessionName) -> Teardown<'_> {
        let mut active = lock(&self.tearing_down);
        while active.contains(session) {
            active = self.teardown_done.wait(active).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        active.insert(session.clone());
        Teardown { daemon: self, session: session.clone() }
    }

    fn is_busy(&self, session: &SessionName) -> bool {
        lock(&self.busy).contains(session)
    }

    fn collect_garbage(&self) {
        let mut store = self.store();
        match runs::gc(&self.paths, &mut store, now_ms()) {
            Ok(report) if !(report.expired.is_empty() && report.evicted.is_empty() && report.orphans.is_empty()) => eprintln!(
                "bctl daemon: gc expired {:?}, evicted {:?}, removed orphan dirs {:?}; runs use {} bytes",
                report.expired, report.evicted, report.orphans, report.bytes
            ),
            Ok(_) => {}
            Err(error) => eprintln!("bctl daemon: gc: {error}"),
        }
    }

    fn watch_health(self: &Arc<Self>) {
        let mut failures: HashMap<(SessionName, Generation), u32> = HashMap::new();
        let mut retried: HashMap<SessionName, Instant> = HashMap::new();
        let mut last_gc = Instant::now();
        loop {
            thread::sleep(HEALTH_INTERVAL);
            if last_gc.elapsed() >= GC_INTERVAL {
                last_gc = Instant::now();
                self.collect_garbage();
            }
            let sessions = self.store().sessions().unwrap_or_default();
            let ready: HashSet<(SessionName, Generation)> =
                sessions.iter().filter(|s| s.state == SessionState::Ready).map(|s| (s.name.clone(), s.generation)).collect();
            failures.retain(|key, _| ready.contains(key));
            let failing: Vec<SessionName> = sessions.iter().filter(|s| s.state == SessionState::Failing).map(|s| s.name.clone()).collect();
            retried.retain(|name, _| failing.contains(name));
            for record in sessions.into_iter().filter(|s| s.state == SessionState::Ready) {
                if self.is_busy(&record.name) {
                    continue;
                }
                let Some(socket) = self.cdp_socket(&record.name) else { continue };
                if cdp_socket_browser(&socket).is_some() {
                    failures.remove(&(record.name.clone(), record.generation));
                    continue;
                }
                let key = (record.name.clone(), record.generation);
                let count = failures.entry(key.clone()).or_default();
                *count += 1;
                if *count < HEALTH_FAILURES {
                    continue;
                }
                failures.remove(&key);
                let Ok(current) = self.store().session(&record.name) else { continue };
                if current.state != SessionState::Ready || current.generation != record.generation || self.is_busy(&record.name) {
                    continue;
                }
                let machine = MachineName { session: record.name.clone(), generation: record.generation };
                let cause = self.fault_cause(&machine);
                self.fail_session(&current, &cause);
            }
            for name in failing {
                if self.is_busy(&name) || retried.get(&name).is_some_and(|at| at.elapsed() < FAILING_RETRY_INTERVAL) {
                    continue;
                }
                retried.insert(name.clone(), Instant::now());
                if let Err(error) = self.retry_failing(&name) {
                    eprintln!("bctl daemon: could not remove failing {name}: {error}");
                }
            }
        }
    }

    fn fault_cause(&self, machine: &MachineName) -> String {
        if let Ok(logs) = self.runtime.logs(machine)
            && let Some(line) = logs
                .lines()
                .rev()
                .find(|line| line.starts_with("bctl-guest: ") && (line.contains("exited unexpectedly") || line.contains("signal ")))
        {
            return line.trim_start_matches("bctl-guest: ").trim().to_string();
        }
        match self.runtime.list() {
            Ok(machines) if machines.iter().any(|m| m.name == machine.to_string() && m.state == MachineState::Running) => {
                format!("the browser on {machine} stopped answering CDP")
            }
            _ => format!("machine {machine} stopped unexpectedly"),
        }
    }

    fn fail_session(&self, record: &SessionRecord, cause: &str) {
        eprintln!("bctl daemon: session {} failed: {cause}", record.name);
        self.stop_live_run_for(&record.name, cause);
        self.authority.of(&record.name, Control::reset);
        let _teardown = self.teardown(&record.name);
        let began = self.store().begin_failure(&record.name, record.generation, cause);
        match began {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                eprintln!("bctl daemon: cannot record {}'s failure: {error}", record.name);
                return;
            }
        }
        tabs::close(self, &record.name);
        gateway::revoke_cdp(self, &record.name);
        self.authority.of(&record.name, Control::reset);
        if let Err(error) = self.remove_failing(&record.name, record.generation) {
            let machine = MachineName { session: record.name.clone(), generation: record.generation };
            eprintln!("bctl daemon: could not remove failed {machine}; {} keeps its profile until it is removed: {error}", record.name);
        }
    }

    /// Removes the VM of a failing session and only then releases the session's profile. The caller holds the teardown.
    fn remove_failing(&self, name: &SessionName, generation: Generation) -> Result<(), RuntimeError> {
        let machine = MachineName { session: name.clone(), generation };
        self.runtime.remove(&machine)?;
        self.discard_endpoints(&machine);
        self.observe(name, |o| *o = Observed::default());
        if let Err(error) = self.store().finish_failure(name, generation) {
            eprintln!("bctl daemon: cannot release {name} after removing {machine}: {error}");
        }
        Ok(())
    }

    /// Retries the removal of a session left `failing` by an earlier removal error or a daemon restart.
    fn retry_failing(&self, name: &SessionName) -> Result<(), RuntimeError> {
        let _teardown = self.teardown(name);
        let Ok(record) = self.store().session(name) else { return Ok(()) };
        if record.state != SessionState::Failing {
            return Ok(());
        }
        self.remove_failing(name, record.generation)
    }

    fn stop_live_run_for(&self, session: &SessionName, cause: &str) {
        let Some(live) = lock(&self.live).get(session).cloned() else { return };
        live.fault(cause);
        let deadline = Instant::now() + FAULT_STOP_TIMEOUT;
        while lock(&self.live).get(session).is_some_and(|current| current.attempt == live.attempt) {
            if Instant::now() >= deadline {
                eprintln!("bctl daemon: run {} did not end within {FAULT_STOP_TIMEOUT:?} of its session failing", live.run);
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn discard_endpoints(&self, machine: &MachineName) {
        let socket = self.paths.cdp_socket(&machine.to_string());
        if let Some(dir) = socket.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }

    fn observe(&self, session: &SessionName, update: impl FnOnce(&mut Observed)) {
        let mut observed = self.observed.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        update(observed.entry(session.clone()).or_default());
    }

    /// Restores ownership after a restart: adopt current-generation VMs, stop stale ones, interrupt orphaned attempts.
    fn restore(self: &Arc<Self>) -> Result<(), String> {
        let machines = self.runtime.list().map_err(|e| e.to_string())?;
        let (plan, attempts) = {
            let store = self.store();
            let attempts = store.attempts().map_err(|e| e.to_string())?;
            (reconcile(&store.sessions().map_err(|e| e.to_string())?, &attempts, &machines), attempts)
        };
        for action in &plan {
            if let Action::Interrupt { attempt } = action
                && let Some(record) = attempts.iter().find(|a| a.id == *attempt)
            {
                self.stop_orphan(record);
            }
        }
        self.store().apply(&plan, now_ms()).map_err(|e| e.to_string())?;
        for action in plan {
            match action {
                Action::StopStale { machine } => {
                    eprintln!("bctl daemon: stopping stale {machine}");
                    if let Err(e) = self.runtime.remove(&machine) {
                        eprintln!("bctl daemon: could not stop {machine}: {e}");
                    }
                }
                Action::Adopt { session, machine } => {
                    let record = self.store().session(&session).map_err(|e| e.to_string())?;
                    eprintln!("bctl daemon: adopting {machine} ({})", record.state.as_str());
                    if record.state == SessionState::Failing {
                        if let Err(error) = self.retry_failing(&session) {
                            eprintln!("bctl daemon: could not remove failing {machine}: {error}");
                        }
                    } else if record.state == SessionState::Stopping {
                        self.finish_stop(&record, &machine);
                    } else if let Some(endpoints) = record.endpoints {
                        let daemon = Arc::clone(self);
                        thread::spawn(move || daemon.await_ready(&machine, endpoints));
                    }
                }
                Action::Settle { session, state, reason } => {
                    eprintln!("bctl daemon: {session} settled as {} ({})", state.as_str(), reason.unwrap_or_default());
                }
                Action::Interrupt { attempt } => eprintln!("bctl daemon: attempt {attempt} interrupted"),
            }
        }
        Ok(())
    }

    fn serve(self: &Arc<Self>, stream: UnixStream) {
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let response = match json::from_str::<Request>(&line) {
            Ok(request) => self.handle(&request).unwrap_or_else(|refusal| *refusal.0),
            Err(_) => Response::failure(ErrorCode::BadRequest, "request is not valid JSON", None),
        };
        let mut writer = &stream;
        let _ = writeln!(writer, "{}", json::to_string(&response));
    }

    pub(crate) fn handle(self: &Arc<Self>, request: &Request) -> Outcome {
        let op = Op::parse(&request.op)
            .ok_or_else(|| Response::failure(ErrorCode::BadRequest, format!("unknown operation {:?}", request.op), None))?;
        let session = || -> Result<SessionName, Refusal> {
            let name = request.session.as_deref().ok_or_else(|| Response::failure(ErrorCode::BadRequest, "a session name is required", None))?;
            SessionName::new(name).map_err(|e| Response::failure(ErrorCode::BadRequest, e.to_string(), None).into())
        };
        match op {
            Op::Ping => Ok(Response::empty()),
            Op::Serve => {
                let url = gateway::serve(self).map_err(internal)?;
                Ok(Response { url: Some(url), ..Response::empty() })
            }
            Op::CdpUrl => {
                let name = session()?;
                if self.cdp_socket(&name).is_none() {
                    return Err(Response::failure(ErrorCode::Conflict, format!("session {name} is not ready"), Some(format!("run `bctl up {name}`"))).into());
                }
                let url = gateway::mint_cdp_url(self, &name).map_err(internal)?;
                Ok(Response { url: Some(url), ..Response::empty() })
            }
            Op::CdpRevoke => {
                let name = session()?;
                match &request.url {
                    Some(url) => gateway::revoke_cdp_url(self, url),
                    None => gateway::revoke_cdp(self, &name),
                }
                Ok(Response::empty())
            }
            Op::Up => {
                let allow = if request.allow_private.is_empty() {
                    None
                } else {
                    Some(request.allow_private.iter().map(|r| AllowedRange::parse(r)).collect::<Result<Vec<_>, _>>().map_err(bad_request)?)
                };
                self.up(&session()?, request.profile.as_deref(), request.image.as_deref(), None, allow)
            }
            Op::Down => self.down(&session()?),
            Op::TabList => tabs::handle(self, &session()?, TabCommand::List, request),
            Op::TabNew => tabs::handle(self, &session()?, TabCommand::New, request),
            Op::TabUse => tabs::handle(self, &session()?, TabCommand::Use, request),
            Op::TabClose => tabs::handle(self, &session()?, TabCommand::Close, request),
            Op::PageSnapshot => page::handle(self, &session()?, PageCommand::Snapshot, request),
            Op::PageResolve => page::handle(self, &session()?, PageCommand::Resolve, request),
            Op::PageGoto
            | Op::PageClick
            | Op::PageDblclick
            | Op::PageFill
            | Op::PageType
            | Op::PagePress
            | Op::PageHover
            | Op::PageScroll
            | Op::PageSelect
            | Op::PageCheck
            | Op::PageUncheck
            | Op::PageWait
            | Op::PageGet
            | Op::PageScreenshot
            | Op::PageEval
            | Op::PageDialog => page::act(self, &session()?, op, request),
            Op::PageBatch => page::batch(self, &session()?, request),
            Op::Rm => {
                let name = session()?;
                tabs::close(self, &name);
                let runs: Vec<i64> = self.store().runs(Some(&name)).map_err(internal)?.iter().map(|r| r.id).collect();
                self.store().delete_session(&name).map_err(store_failure)?;
                for run in runs {
                    let dir = self.paths.run_dir(run);
                    if let Err(error) = fs::remove_dir_all(&dir)
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        return Err(internal(format!("forgot {name} but cannot remove {}: {error}", dir.display())).into());
                    }
                }
                let files = self.paths.files_dir(name.as_str());
                if let Err(error) = fs::remove_dir_all(&files)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(internal(format!("forgot {name} but cannot remove {}: {error}", files.display())).into());
                }
                Ok(Response::empty())
            }
            Op::Run => {
                let job = request.job.as_deref().ok_or_else(|| bad_request("run needs a job"))?;
                self.start_run(&session()?, job)
            }
            Op::Runs => {
                let runs = match request.run {
                    Some(id) => vec![self.store().run(id).map_err(store_failure)?],
                    None => {
                        let filter = request.session.as_deref().map(|_| session()).transpose()?;
                        self.store().runs(filter.as_ref()).map_err(internal)?
                    }
                };
                Ok(Response { runs: runs.iter().map(|r| self.run_view(r)).collect(), ..Response::empty() })
            }
            Op::Logs => {
                let id = request.run.ok_or_else(|| bad_request("logs needs a run id"))?;
                self.store().run(id).map_err(store_failure)?;
                let path = RunFiles::new(self.paths.run_dir(id)).log();
                let text = runs::read_log(&path).map_err(internal)?;
                Ok(Response { log: Some(Box::new(LogView { path: path.display().to_string(), text })), ..Response::empty() })
            }
            Op::Pause => match request.run {
                Some(run) => self.pause(run),
                None => self.pause_session(&session()?),
            },
            Op::Resume => match request.run {
                Some(run) => self.resume(run),
                None => self.resume_session(&session()?),
            },
            Op::Cancel => self.cancel(request.run.ok_or_else(|| bad_request("cancel needs a run id"))?),
            Op::Status => {
                let sessions = self.store().sessions().map_err(internal)?;
                Ok(Response { sessions: sessions.iter().map(|s| self.view(s)).collect(), ..Response::empty() })
            }
            Op::Inspect => {
                let record = self.store().session(&session()?).map_err(store_failure)?;
                Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() })
            }
            Op::Profiles => {
                let profiles = self.store().profiles().map_err(internal)?;
                Ok(Response {
                    profiles: profiles
                        .into_iter()
                        .map(|(name, holder)| ProfileView { name: name.to_string(), holder: holder.map(|h| h.to_string()) })
                        .collect(),
                    ..Response::empty()
                })
            }
        }
    }

    fn view(&self, record: &SessionRecord) -> SessionView {
        let observed = self.observed.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let observed = observed.get(&record.name);
        let holds = record.state.holds_machine();
        SessionView {
            name: record.name.to_string(),
            profile: record.profile.to_string(),
            image: record.image.clone(),
            resources: resources_view(record.resources),
            allow_private: record.allow_private.iter().map(|r| r.as_str().to_string()).collect(),
            state: record.state.as_str().to_string(),
            generation: record.generation.0,
            stage: if record.state == SessionState::Starting { observed.and_then(|o| o.stage).map(str::to_string) } else { None },
            failure: record.failure.clone(),
            machine: holds.then(|| MachineName { session: record.name.clone(), generation: record.generation }.to_string()),
            vnc_port: record.endpoints.filter(|_| holds && observed.and_then(|o| o.input) != Some(InputSupport::Headless)).map(|e| e.vnc_port),
            input: observed.and_then(|o| o.input).filter(|_| holds).map(|i| match i {
                InputSupport::Interactive => "interactive".to_string(),
                InputSupport::ViewOnly => "view-only".to_string(),
                InputSupport::Headless => "headless".to_string(),
            }),
            display: observed.and_then(|o| o.input).filter(|_| holds).map(|i| if i == InputSupport::Headless { "headless" } else { "headed" }.to_string()),
            pausable: holds.then_some(self.runtime.kind() == RuntimeKind::SmolVm),
            files_dir: self.files_dir(&record.name).display().to_string(),
            guest_files_dir: bctl_runtime::GUEST_FILES_DIR.to_string(),
        }
    }

    fn up(
        &self,
        name: &SessionName,
        profile: Option<&str>,
        image: Option<&str>,
        resources: Option<Resources>,
        allow_private: Option<Vec<AllowedRange>>,
    ) -> Outcome {
        let profile = ProfileName::new(profile.unwrap_or(name.as_str())).map_err(|e| Response::failure(ErrorCode::BadRequest, e.to_string(), None))?;
        let existing = match self.store().session(name) {
            Ok(record) => Some(record),
            Err(StoreError::NoSuchSession(_)) => None,
            Err(e) => return Err(internal(e).into()),
        };
        let image = image_for_up(image, existing.as_ref().map(|record| record.image.as_str()), self.runtime.kind().default_image());
        let allow_private = allow_private.map(|mut ranges| {
            ranges.sort();
            ranges.dedup();
            ranges
        });
        if let Some(record) = &existing
            && (record.profile != profile
                || record.image != image
                || resources.is_some_and(|r| r != record.resources)
                || allow_private.as_ref().is_some_and(|a| *a != record.allow_private))
        {
            let ResourcesView { width, height, cpus, memory_mib } = resources_view(record.resources);
            let allowed: Vec<&str> = record.allow_private.iter().map(AllowedRange::as_str).collect();
            return Err(Response::failure(
                ErrorCode::Conflict,
                format!(
                    "session {name} exists with profile {}, image {}, a {width}x{height} display, {cpus} CPUs, {memory_mib} MiB, and private ranges [{}]",
                    record.profile,
                    record.image,
                    allowed.join(", ")
                ),
                Some(format!("use those, or `bctl down {name} && bctl rm {name}` to replace it")),
            ).into());
        }
        let image_file = self.runtime.image_path(&self.paths, &image);
        if !image_file.exists() {
            return Err(Response::failure(
                ErrorCode::ImageMissing,
                format!("no image {image} at {}", image_file.display()),
                Some(self.runtime.image_hint(&image)),
            ).into());
        }
        if let Err((missing, hint)) = self.runtime.installed(&self.paths) {
            return Err(Response::failure(
                ErrorCode::RuntimeMissing,
                format!("the {} runtime has no {}", self.runtime.kind().as_str(), missing.display()),
                Some(hint.into()),
            ).into());
        }
        if existing.is_none() {
            let resources = resources.unwrap_or(Resources::DEFAULT);
            self.store().create_session(name, &profile, &image, resources, &allow_private.unwrap_or_default()).map_err(store_failure)?;
        }
        loop {
            let record = self.store().session(name).map_err(store_failure)?;
            match record.state {
                SessionState::Ready => return Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() }),
                SessionState::Starting | SessionState::Stopping => thread::sleep(Duration::from_millis(200)),
                SessionState::Failing => {
                    if let Err(error) = self.retry_failing(name) {
                        return Err(Response::failure(
                            ErrorCode::RuntimeError,
                            format!("session {name} cannot start until its failed VM is removed: {error}"),
                            Some(format!("retry `bctl down {name}`")),
                        ).into());
                    }
                }
                SessionState::Paused => {
                    return Err(Response::failure(
                        ErrorCode::Conflict,
                        format!("session {name} is paused"),
                        Some(format!("resume it with `bctl resume {name}` or stop it with `bctl down {name}`")),
                    ).into());
                }
                SessionState::Stopped | SessionState::Failed => break,
            }
        }

        let configured = self.store().session(name).map_err(store_failure)?;
        if configured.state == SessionState::Failed {
            let previous = MachineName { session: name.clone(), generation: configured.generation };
            if let Err(error) = self.runtime.remove(&previous) {
                eprintln!("bctl daemon: could not remove {previous} from its failed start: {error}");
            }
            self.discard_endpoints(&previous);
        }
        let Resources { viewport: [width, height], cpus, memory_mib } = configured.resources;
        let display = (u16::try_from(width).map_err(internal)?, u16::try_from(height).map_err(internal)?);
        let cpus = u8::try_from(cpus).map_err(internal)?;
        let endpoints = Endpoints { vnc_port: free_port().map_err(internal)? };
        let machine = self.store().begin_start(name, endpoints).map_err(store_failure)?;
        self.observe(name, |o| *o = Observed::default());
        let profile_dir = match self.ensure_profile_dir(&profile).and_then(|dir| clear_stale_browser_lock(&dir).map(|()| dir)) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.store().transition(name, machine.generation, SessionState::Failed, Some(&error));
                return Err(internal(error).into());
            }
        };
        let files_dir = self.files_dir(name);
        if let Err(error) = private_dir(&files_dir) {
            let _ = self.store().transition(name, machine.generation, SessionState::Failed, Some(&error));
            return Err(internal(error).into());
        }
        let image_path = match self.runtime.image_path(&self.paths, &image).canonicalize() {
            Ok(path) => path,
            Err(error) => {
                let message = format!("cannot resolve image {image}: {error}");
                let _ = self.store().transition(name, machine.generation, SessionState::Failed, Some(&message));
                return Err(Response::failure(ErrorCode::ImageMissing, message, None).into());
            }
        };
        let cdp_socket = match self.private_cdp_dir(&machine) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.store().transition(name, machine.generation, SessionState::Failed, Some(&error));
                return Err(internal(error).into());
            }
        };
        let spec = MachineLaunch {
            machine: machine.clone(),
            image: image_path,
            profile_dir,
            files_dir,
            endpoints,
            cdp_socket,
            display,
            memory_mib,
            cpus,
            allow_private: configured.allow_private.iter().map(|r| r.as_str().to_string()).collect(),
        };
        if let Err(error) = self.runtime.launch(&spec) {
            self.fail_start(&machine, &error);
            return Err(Response::failure(ErrorCode::StartFailed, error.to_string(), Some(format!("see `bctl inspect {name}`"))).into());
        }
        self.await_ready(&machine, endpoints);
        let record = self.store().session(name).map_err(store_failure)?;
        if record.state == SessionState::Ready {
            Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() })
        } else {
            Err(Response::failure(
                ErrorCode::StartFailed,
                record.failure.unwrap_or_else(|| format!("session ended {}", record.state.as_str())),
                Some(if record.state == SessionState::Failing {
                    format!("the VM could not be removed yet; retry `bctl down {name}`")
                } else {
                    format!("the VM was removed; retry with `bctl up {name}`")
                }),
            ).into())
        }
    }

    fn pause_session(&self, name: &SessionName) -> Outcome {
        let _busy = self.busy(name);
        let record = self.store().session(name).map_err(store_failure)?;
        if record.state != SessionState::Ready {
            return Err(Response::failure(
                ErrorCode::NotPausable,
                format!("session {name} is {}, not ready", record.state.as_str()),
                Some(format!("run `bctl up {name}` first")),
            ).into());
        }
        tabs::close(self, name);
        let machine = MachineName { session: name.clone(), generation: record.generation };
        if let Err(error) = self.runtime.pause(&machine) {
            return Err(runtime_failure(&error, name).into());
        }
        self.store().transition(name, record.generation, SessionState::Paused, None).map_err(store_failure)?;
        let record = self.store().session(name).map_err(store_failure)?;
        Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() })
    }

    fn resume_session(&self, name: &SessionName) -> Outcome {
        let _busy = self.busy(name);
        let record = self.store().session(name).map_err(store_failure)?;
        if record.state != SessionState::Paused {
            return Err(Response::failure(
                ErrorCode::NotResumable,
                format!("session {name} is {}, not paused", record.state.as_str()),
                Some(format!("see `bctl inspect {name}`")),
            ).into());
        }
        let machine = MachineName { session: name.clone(), generation: record.generation };
        let spec = match self.resume_spec(&record, &machine) {
            Ok(spec) => spec,
            Err(error) => return Err(internal(error).into()),
        };
        let endpoints = spec.endpoints;
        if let Err(error) = self.runtime.resume(&spec) {
            return Err(runtime_failure(&error, name).into());
        }
        self.await_ready(&machine, endpoints);
        let record = self.store().session(name).map_err(store_failure)?;
        if record.state == SessionState::Ready {
            Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() })
        } else {
            Err(Response::failure(
                ErrorCode::StartFailed,
                record.failure.unwrap_or_else(|| format!("session ended {}", record.state.as_str())),
                Some(format!("see `bctl inspect {name}`")),
            ).into())
        }
    }

    fn resume_spec(&self, record: &SessionRecord, machine: &MachineName) -> Result<MachineLaunch, String> {
        let Resources { viewport: [width, height], cpus, memory_mib } = record.resources;
        let display = (
            u16::try_from(width).map_err(|_| format!("viewport width {width} does not fit a display"))?,
            u16::try_from(height).map_err(|_| format!("viewport height {height} does not fit a display"))?,
        );
        let cpus = u8::try_from(cpus).map_err(|_| format!("{cpus} CPUs do not fit the runtime"))?;
        let endpoints = record.endpoints.ok_or_else(|| format!("session {} has no recorded VNC endpoint", record.name))?;
        let image = self.runtime.image_path(&self.paths, &record.image).canonicalize().map_err(|e| format!("cannot resolve image {}: {e}", record.image))?;
        Ok(MachineLaunch {
            machine: machine.clone(),
            image,
            profile_dir: self.paths.profile_dir(record.profile.as_str()),
            files_dir: self.files_dir(&record.name),
            endpoints,
            cdp_socket: self.paths.cdp_socket(&machine.to_string()),
            display,
            memory_mib,
            cpus,
            allow_private: record.allow_private.iter().map(|r| r.as_str().to_string()).collect(),
        })
    }

    /// Creates the user-only directory that will hold the machine's CDP socket and returns the socket path.
    fn private_cdp_dir(&self, machine: &MachineName) -> Result<PathBuf, String> {
        let socket = self.paths.cdp_socket(&machine.to_string());
        if socket.as_os_str().len() > 100 {
            return Err(format!("{} is too long for a Unix socket; set a shorter BCTL_HOME", socket.display()));
        }
        let dir = socket.parent().expect("a socket path has a directory");
        let _ = fs::remove_dir_all(dir);
        for path in [dir.parent().expect("the socket directory has a parent"), dir] {
            fs::create_dir_all(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        }
        Ok(socket)
    }

    fn ensure_profile_dir(&self, profile: &ProfileName) -> Result<std::path::PathBuf, String> {
        let path = self.paths.profile_dir(profile.as_str());
        private_dir(&path)?;
        Ok(path)
    }

    /// The session's files directory, which the guest mounts at [`bctl_runtime::GUEST_FILES_DIR`].
    pub(crate) fn files_dir(&self, session: &SessionName) -> PathBuf {
        self.paths.files_dir(session.as_str())
    }

    pub(crate) fn run_dir(&self, run: i64) -> PathBuf {
        self.paths.run_dir(run)
    }

    pub(crate) fn session_views(&self) -> SessionsPayload {
        let sessions = self.store().sessions().unwrap_or_default();
        SessionsPayload { sessions: sessions.iter().map(|s| self.view(s)).collect() }
    }

    fn ready_endpoints(&self, name: &SessionName) -> Option<Endpoints> {
        let record = self.store().session(name).ok()?;
        (record.state == SessionState::Ready).then_some(record.endpoints).flatten()
    }

    pub(crate) fn cdp_socket(&self, name: &SessionName) -> Option<PathBuf> {
        let record = self.store().session(name).ok()?;
        (record.state == SessionState::Ready).then(|| self.paths.cdp_socket(&MachineName { session: record.name, generation: record.generation }.to_string()))
    }

    /// The VNC port to proxy and whether this runtime has input devices at all.
    pub(crate) fn vnc_target(&self, name: &SessionName) -> Option<(u16, bool)> {
        let endpoints = self.ready_endpoints(name)?;
        let observed = self.observed.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let input = observed.get(name).and_then(|o| o.input);
        if input == Some(InputSupport::Headless) {
            return None;
        }
        Some((endpoints.vnc_port, input == Some(InputSupport::Interactive)))
    }

    pub(crate) fn down_session(&self, name: &SessionName) -> Result<(), String> {
        self.down(name).map(drop).map_err(|refusal| refusal.0.error.map(|e| e.message).unwrap_or_default())
    }

    fn await_ready(&self, machine: &MachineName, endpoints: Endpoints) {
        let session = &machine.session;
        let result = self.runtime.wait_ready(machine, endpoints, &self.paths.cdp_socket(&machine.to_string()), READY_TIMEOUT, |stage| self.observe(session, |o| o.stage = Some(stage.as_str())));
        match result {
            Ok(ready) => {
                self.observe(session, |o| {
                    o.stage = None;
                    o.input = Some(ready.input);
                });
                let transition = self.store().transition(session, machine.generation, SessionState::Ready, None);
                if let Err(e) = transition {
                    eprintln!("bctl daemon: {machine} became ready but the record moved on: {e}");
                }
            }
            Err(error) => self.fail_start(machine, &error),
        }
    }

    fn fail_start(&self, machine: &MachineName, error: &RuntimeError) {
        eprintln!("bctl daemon: {machine} failed to start: {error}");
        if let Ok(logs) = self.runtime.logs(machine) {
            eprintln!("bctl daemon: last lines from {machine}:\n{}", logs.trim_end());
        }
        let _teardown = self.teardown(&machine.session);
        match self.runtime.remove(machine) {
            Ok(()) => {
                self.discard_endpoints(machine);
                let _ = self.store().transition(&machine.session, machine.generation, SessionState::Failed, Some(&error.to_string()));
            }
            Err(e) => {
                eprintln!("bctl daemon: could not remove {machine}; {} keeps its profile until it is removed: {e}", machine.session);
                let _ = self.store().transition(&machine.session, machine.generation, SessionState::Failing, Some(&error.to_string()));
            }
        }
    }

    fn down(&self, name: &SessionName) -> Outcome {
        let _busy = self.busy(name);
        self.stop_live_run(name);
        gateway::revoke_cdp(self, name);
        tabs::close(self, name);
        self.authority.of(name, Control::reset);
        let _teardown = self.teardown(name);
        let record = self.store().session(name).map_err(store_failure)?;
        match record.state {
            SessionState::Stopped => return Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() }),
            SessionState::Failing => {
                let machine = MachineName { session: name.clone(), generation: record.generation };
                if let Err(error) = self.runtime.remove(&machine) {
                    eprintln!("bctl daemon: could not remove failing {machine}: {error}");
                    return Err(Response::failure(ErrorCode::RuntimeError, error.to_string(), Some(format!("retry `bctl down {name}`"))).into());
                }
                self.discard_endpoints(&machine);
                self.observe(name, |o| *o = Observed::default());
                self.store().transition(name, record.generation, SessionState::Stopped, record.failure.as_deref()).map_err(store_failure)?;
                let record = self.store().session(name).map_err(store_failure)?;
                return Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() });
            }
            SessionState::Failed => {
                let machine = MachineName { session: name.clone(), generation: record.generation };
                if let Err(error) = self.runtime.remove(&machine) {
                    eprintln!("bctl daemon: could not remove failed {machine}: {error}");
                }
                self.discard_endpoints(&machine);
                self.store().transition(name, record.generation, SessionState::Stopped, record.failure.as_deref()).map_err(store_failure)?;
                let record = self.store().session(name).map_err(store_failure)?;
                return Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() });
            }
            SessionState::Starting | SessionState::Ready | SessionState::Stopping | SessionState::Paused => {}
        }
        self.store().transition(name, record.generation, SessionState::Stopping, None).map_err(store_failure)?;
        let machine = MachineName { session: name.clone(), generation: record.generation };
        self.finish_stop(&record, &machine);
        let record = self.store().session(name).map_err(store_failure)?;
        match record.state {
            SessionState::Stopped => Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() }),
            _ => Err(Response::failure(
                ErrorCode::RuntimeError,
                record.failure.unwrap_or_else(|| "the VM did not stop".into()),
                Some(format!("retry `bctl down {name}`")),
            ).into()),
        }
    }

    fn start_run(self: &Arc<Self>, session: &SessionName, job: &JobRequest) -> Outcome {
        let (exec, grab) = match (&job.exec, &job.grab) {
            (Some(exec), None) => {
                if exec.argv.first().is_none_or(String::is_empty) {
                    return Err(bad_request("an exec job needs a non-empty argv").into());
                }
                if !std::path::Path::new(&exec.cwd).is_absolute() {
                    return Err(bad_request(format!("exec cwd {:?} is not absolute", exec.cwd)).into());
                }
                (Some(exec.clone()), None)
            }
            (None, Some(grab)) => {
                if grab.urls.is_empty() {
                    return Err(bad_request("a grab job needs at least one URL").into());
                }
                (None, Some(grab.clone()))
            }
            _ => return Err(bad_request("a job is exactly one of exec and grab").into()),
        };
        match &job.browser {
            Some(browser) => {
                let ResourcesView { width, height, cpus, memory_mib } = browser.resources;
                let resources = Resources { viewport: [width, height], cpus, memory_mib };
                self.up(session, Some(&browser.profile), Some(&browser.image), Some(resources), None)?;
            }
            None => {
                let record = self.store().session(session).map_err(|error| match error {
                    StoreError::NoSuchSession(_) => Response::failure(
                        ErrorCode::NoSuchSession,
                        format!("no session named {session}"),
                        Some(format!("start one with `bctl up {session}` first")),
                    ),
                    other => store_failure(other),
                })?;
                self.up(session, Some(record.profile.as_str()), Some(&record.image), None, None)?;
            }
        }
        let record = self.store().session(session).map_err(store_failure)?;
        let browser_version = self.browser_version(session)?;
        let image_path = self.runtime.image_path(&self.paths, &record.image);
        let image_digest = match self.runtime.kind() {
            RuntimeKind::SmolVm => runs::image_digest(&image_path),
            RuntimeKind::Nanos => self.disk_digests.digest(&image_path),
        }.map_err(internal)?;
        let config = RunConfig {
            job: job.name.clone(),
            session: session.to_string(),
            profile: record.profile.to_string(),
            image: record.image.clone(),
            resources: resources_view(record.resources),
            allow_private: record.allow_private.iter().map(|r| r.as_str().to_string()).collect(),
            grab_program: grab.as_ref().map(|_| runs::grab_program()).transpose().map_err(internal)?,
            exec,
            grab,
        };
        let kind = config.kind().map_err(internal)?;
        let mut live = lock(&self.live);
        if let Some(active) = live.get(session) {
            return Err(Response::failure(
                ErrorCode::RunActive,
                format!("session {session} is already running run {}", active.run),
                Some(format!("wait for it, or `bctl cancel {}`", active.run)),
            ).into());
        }
        let spec = NewRun {
            session: session.clone(),
            job: job.name.clone(),
            kind,
            profile: record.profile.clone(),
            image: record.image.clone(),
            image_digest,
            browser_version,
            config: config.to_json(),
            input_digest: config.input_digest(),
            created_ms: now_ms(),
        };
        let (run, attempt) = self.store().create_run(&spec).map_err(store_failure)?;
        self.start_attempt(&mut live, session, run, attempt, 1, &config);
        drop(live);
        self.run_response(run)
    }

    fn browser_version(&self, session: &SessionName) -> Result<String, Refusal> {
        let not_ready = || {
            Refusal::from(Response::failure(ErrorCode::SessionNotReady, format!("session {session} does not answer CDP"), Some(format!("see `bctl inspect {session}`"))))
        };
        let socket = self.cdp_socket(session).ok_or_else(not_ready)?;
        cdp_socket_browser(&socket).ok_or_else(not_ready)
    }

    fn start_attempt(self: &Arc<Self>, live: &mut HashMap<SessionName, Arc<Live>>, session: &SessionName, run: i64, attempt: i64, number: u32, config: &RunConfig) {
        let files = RunFiles::new(self.paths.run_dir(run));
        let launched = automation_cdp_url(self, session).map_err(|e| format!("cannot issue a CDP URL: {e}")).and_then(|cdp_url| {
            let ledger = Arc::new(RunLedger { daemon: Arc::clone(self), session: session.clone(), cdp_url: cdp_url.clone() });
            let shared = self.files_dir(session);
            runs::launch(Launch { run, attempt, number, config, files, cdp_url: cdp_url.clone(), shared }, ledger).inspect_err(|_| gateway::revoke_cdp_url(self, &cdp_url))
        });
        match launched {
            Ok(handle) => {
                live.insert(session.clone(), handle);
            }
            Err(detail) => {
                let end = AttemptEnd::Ended { outcome: RunOutcome::JobError, detail: Some(detail) };
                if let Err(error) = self.store().finish_attempt(attempt, &end, now_ms()) {
                    eprintln!("bctl daemon: attempt {attempt}: {error}");
                }
            }
        }
    }

    fn run_response(&self, run: i64) -> Outcome {
        let record = self.store().run(run).map_err(store_failure)?;
        Ok(Response { runs: vec![self.run_view(&record)], ..Response::empty() })
    }

    fn run_view(&self, record: &RunRecord) -> RunView {
        let attempt_view = |a: &AttemptRecord| AttemptView {
            number: a.number,
            state: a.state.as_str().to_string(),
            outcome: a.outcome.map(|o| o.as_str().to_string()),
            detail: a.detail.clone(),
            pid: a.pid,
            started_ms: a.started_ms,
            ended_ms: a.ended_ms,
        };
        let files = RunFiles::new(self.paths.run_dir(record.id));
        let current = &record.current;
        RunView {
            id: record.id,
            session: record.spec.session.to_string(),
            job: record.spec.job.clone(),
            kind: record.spec.kind.as_str().to_string(),
            state: current.state.as_str().to_string(),
            outcome: current.outcome.map(|o| o.as_str().to_string()),
            detail: current.detail.clone(),
            profile: record.spec.profile.to_string(),
            image: record.spec.image.clone(),
            image_digest: record.spec.image_digest.clone(),
            browser_version: record.spec.browser_version.clone(),
            config: json::from_str(&record.spec.config).unwrap_or_else(|_| json::Value::String(record.spec.config.clone())),
            input_digest: record.spec.input_digest.clone(),
            output_dir: files.output().display().to_string(),
            log: files.log().display().to_string(),
            created_ms: record.spec.created_ms,
            progress: current.progress.map(|p| ProgressView { pages_done: p.pages_done, pages_queued: p.pages_queued, assets_done: p.assets_done }),
            interaction: current.interaction.as_ref().map(|i| InteractionView { reason: i.reason.clone(), url: i.url.clone() }),
            attempts: record.previous.iter().chain(std::iter::once(current)).map(attempt_view).collect(),
        }
    }

    /// The session's live run, if it is waiting for a person.
    pub(crate) fn run_needing_interaction(&self, session: &SessionName) -> Option<i64> {
        let live = lock(&self.live).get(session).cloned()?;
        let record = self.store().run(live.run).ok()?;
        (record.current.id == live.attempt && record.current.state == AttemptState::NeedsInteraction).then_some(live.run)
    }

    fn live_handle(&self, record: &RunRecord) -> Option<Arc<Live>> {
        lock(&self.live).get(&record.spec.session).filter(|live| live.attempt == record.current.id).cloned()
    }

    fn pause(&self, run: i64) -> Outcome {
        let record = self.store().run(run).map_err(store_failure)?;
        if record.spec.kind == RunKind::Exec {
            return Err(Response::failure(
                ErrorCode::NotPausable,
                format!("run {run} is an exec run, which has no event boundary to pause at"),
                Some(format!("`bctl cancel {run}` stops it")),
            ).into());
        }
        match (record.current.state, self.live_handle(&record)) {
            (AttemptState::Paused, _) => {}
            (state, Some(live)) if state.is_live() => live.request(Intent::Pause),
            (state, _) => return Err(not_live(run, state).into()),
        }
        self.run_response(run)
    }

    fn resume(self: &Arc<Self>, run: i64) -> Outcome {
        let record = self.store().run(run).map_err(store_failure)?;
        match record.current.state {
            AttemptState::NeedsInteraction => {
                let live = self.live_handle(&record).ok_or_else(|| not_live(run, record.current.state))?;
                live.resume().map_err(|e| internal(format!("cannot write resume to run {run}'s control channel: {e}")))?;
                self.authority.of(&record.spec.session, Control::resume_run);
                return self.run_response(run);
            }
            AttemptState::Paused | AttemptState::Interrupted => {}
            state => {
                return Err(Response::failure(
                    ErrorCode::NotResumable,
                    format!("run {run} is {}; only a paused, interrupted, or needs-interaction run resumes", state.as_str()),
                    None,
                ).into());
            }
        }
        if record.spec.kind == RunKind::Exec {
            return Err(Response::failure(
                ErrorCode::NotResumable,
                format!("run {run} is an exec run, which keeps no state to continue from"),
                Some("start a new one with `bctl run`".into()),
            ).into());
        }
        let session = record.spec.session.clone();
        if self.cdp_socket(&session).is_none() {
            return Err(Response::failure(ErrorCode::SessionNotReady, format!("session {session} is not ready"), Some(format!("run `bctl up {session}` first"))).into());
        }
        let config: RunConfig = json::from_str(&record.spec.config).map_err(|_| internal(format!("run {run} has an unreadable config")))?;
        let mut live = lock(&self.live);
        if let Some(active) = live.get(&session) {
            return Err(Response::failure(ErrorCode::RunActive, format!("session {session} is already running run {}", active.run), None).into());
        }
        let attempt = self.store().begin_attempt(run, now_ms()).map_err(store_failure)?;
        self.start_attempt(&mut live, &session, run, attempt, record.current.number + 1, &config);
        drop(live);
        self.run_response(run)
    }

    fn cancel(&self, run: i64) -> Outcome {
        let record = self.store().run(run).map_err(store_failure)?;
        match (record.current.state, self.live_handle(&record)) {
            (AttemptState::Paused, _) => self.store().cancel_paused(record.current.id, now_ms()).map_err(internal)?,
            (state, Some(live)) if state.is_live() => live.request(Intent::Cancel),
            (state, _) => return Err(not_live(run, state).into()),
        }
        self.run_response(run)
    }

    fn stop_live_run(&self, session: &SessionName) {
        let Some(live) = lock(&self.live).get(session).cloned() else { return };
        live.request(Intent::Stop);
        let deadline = std::time::Instant::now() + STOP_RUN_TIMEOUT;
        while lock(&self.live).get(session).is_some_and(|current| current.attempt == live.attempt) {
            if std::time::Instant::now() >= deadline {
                eprintln!("bctl daemon: run {} did not stop within {}s; stopping {session} anyway", live.run, STOP_RUN_TIMEOUT.as_secs());
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn stop_orphan(&self, attempt: &AttemptRecord) {
        let files = RunFiles::new(self.paths.run_dir(attempt.run));
        let outcome = runs::stop_orphan(&files, attempt);
        eprintln!("bctl daemon: run {} attempt {} interrupted: {outcome}", attempt.run, attempt.number);
        match BoundedLog::open(&files.log()) {
            Ok(log) => BoundedLog::note(&log, &format!("attempt {} interrupted when the daemon restarted: {outcome}", attempt.number)),
            Err(error) => eprintln!("bctl daemon: cannot open {}: {error}", files.log().display()),
        }
    }

    fn finish_stop(&self, record: &SessionRecord, machine: &MachineName) {
        let generation: Generation = record.generation;
        match self.runtime.stop_and_remove(machine) {
            Ok(removal) => {
                let note = match removal {
                    Removal::Stopped => None,
                    Removal::Forced { stop_failure } => {
                        let note = format!("the guest did not stop gracefully and its VM was deleted by force: {stop_failure}");
                        eprintln!("bctl daemon: {machine}: {note}");
                        Some(note)
                    }
                };
                self.discard_endpoints(machine);
                self.observe(&record.name, |o| *o = Observed::default());
                let _ = self.store().transition(&record.name, generation, SessionState::Stopped, note.as_deref());
            }
            Err(error) => {
                eprintln!("bctl daemon: could not stop {machine}: {error}");
                let _ = self.store().transition(&record.name, generation, SessionState::Stopping, Some(&error.to_string()));
            }
        }
    }
}

fn not_live(run: i64, state: AttemptState) -> Response {
    Response::failure(ErrorCode::NotLive, format!("run {run} is {}", state.as_str()), None)
}

pub(crate) fn now_ms() -> i64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
}

fn automation_cdp_url(daemon: &Arc<Daemon>, session: &SessionName) -> std::io::Result<String> {
    gateway::mint_cdp_url(daemon, session)
}

/// Chromium marks a profile in use with `Singleton*` links naming the guest's hostname, which is the machine
/// name, so a lock left by a removed generation reads as another computer holding the profile. A session that
/// has begun starting holds its profile exclusively, so any lock there is stale.
fn private_dir(path: &std::path::Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| format!("cannot restrict {}: {e}", path.display()))
}

fn clear_stale_browser_lock(profile_dir: &std::path::Path) -> Result<(), String> {
    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let path = profile_dir.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot remove the stale browser lock {}: {error}", path.display())),
        }
    }
    Ok(())
}

fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Opens the daemon log for appending, owner-only.
pub fn open_log(paths: &Paths) -> std::io::Result<File> {
    fs::create_dir_all(&paths.home)?;
    OpenOptions::new().create(true).append(true).mode(0o600).open(paths.log())
}

/// The image `up` starts: the one named, else the one the session was created with, else the default.
fn image_for_up(requested: Option<&str>, recorded: Option<&str>, default: &str) -> String {
    requested.or(recorded).unwrap_or(default).to_string()
}

fn runtime_failure(error: &RuntimeError, session: &SessionName) -> Response {
    match error {
        RuntimeError::Unsupported { .. } => {
            Response::failure(ErrorCode::UnsupportedByRuntime, error.to_string(), Some(format!("stop it with `bctl down {session}` instead")))
        }
        _ => Response::failure(ErrorCode::RuntimeError, error.to_string(), Some(format!("see `bctl inspect {session}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_takes_the_named_image_then_the_recorded_one_then_helium() {
        assert_eq!(image_for_up(Some("ref-alpine-chromium"), Some("helium"), "helium"), "ref-alpine-chromium");
        assert_eq!(image_for_up(None, Some("ref-scratch-chromium"), "helium"), "ref-scratch-chromium");
        assert_eq!(image_for_up(None, None, "helium"), "helium");
        assert_eq!(image_for_up(None, None, "nanos-helium"), "nanos-helium");
    }

    static SERIAL: Mutex<()> = Mutex::new(());

    const FAKE_SMOLVM: &str = r#"#!/bin/sh
ctl="$(dirname "$0")/ctl"
echo "$@" >> "$ctl/calls"
case "$1 $2" in
"machine stop")
    if [ -e "$ctl/stop-fail" ]; then
        echo '{"error":{"code":"BUSY","message":"guest did not confirm filesystem synchronization"}}' >&2
        exit 1
    fi
    ;;
"machine delete")
    touch "$ctl/deleting"
    while [ -e "$ctl/block" ]; do sleep 0.05; done
    if [ -e "$ctl/fail" ]; then
        echo '{"error":{"code":"BUSY","message":"the VM would not stop"}}' >&2
        exit 1
    fi
    ;;
"machine ls")
    if [ -e "$ctl/ls.json" ]; then cat "$ctl/ls.json"; else echo '[]'; fi
    ;;
esac
exit 0
"#;

    struct Fixture {
        daemon: Arc<Daemon>,
        ctl: PathBuf,
        _serial: MutexGuard<'static, ()>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.daemon.paths.home);
        }
    }

    impl Fixture {
        fn new(tag: &str) -> Fixture {
            let serial = SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let home = std::env::temp_dir().join(format!("bctl-fail-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&home);
            let paths = Paths { home };
            let binary = paths.runtime();
            let ctl = binary.parent().unwrap().join("ctl");
            fs::create_dir_all(&ctl).unwrap();
            fs::write(&binary, FAKE_SMOLVM).unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
            fs::create_dir_all(paths.images()).unwrap();
            fs::write(paths.image("img"), b"").unwrap();
            let store = Store::open_in_memory().unwrap();
            let runtime = Runtime::new(RuntimeKind::SmolVm, &paths).unwrap();
            let daemon = Arc::new(Daemon {
                runtime,
                paths,
                store: Mutex::new(store),
                observed: Mutex::default(),
                live: Mutex::default(),
                busy: Mutex::default(),
                tearing_down: Mutex::default(),
                teardown_done: Condvar::new(),
                disk_digests: runs::DiskDigests::default(),
                gateway: Mutex::default(),
                browsers: Mutex::default(),
                authority: Authorities::default(),
            });
            Fixture { daemon, ctl, _serial: serial }
        }

        fn set(&self, flag: &str) {
            fs::write(self.ctl.join(flag), b"").unwrap();
        }

        fn clear(&self, flag: &str) {
            let _ = fs::remove_file(self.ctl.join(flag));
        }

        fn calls(&self) -> String {
            fs::read_to_string(self.ctl.join("calls")).unwrap_or_default()
        }

        fn ready(&self, session: &str, profile: &str) -> (SessionName, MachineName) {
            let name = SessionName::new(session).unwrap();
            let mut store = self.daemon.store();
            store.create_session(&name, &ProfileName::new(profile).unwrap(), "img", Resources::DEFAULT, &[]).unwrap();
            let machine = store.begin_start(&name, Endpoints { vnc_port: 15900 }).unwrap();
            store.transition(&name, machine.generation, SessionState::Ready, None).unwrap();
            drop(store);
            fs::create_dir_all(self.endpoint_dir(&machine)).unwrap();
            (name, machine)
        }

        fn endpoint_dir(&self, machine: &MachineName) -> PathBuf {
            self.daemon.paths.cdp_socket(&machine.to_string()).parent().unwrap().to_path_buf()
        }

        fn record(&self, name: &SessionName) -> SessionRecord {
            self.daemon.store().session(name).unwrap()
        }

        fn second_start_is_refused(&self, rival: &str, profile: &str, holder: &SessionName) {
            let refusal = match self.daemon.up(&SessionName::new(rival).unwrap(), Some(profile), Some("img"), None, None) {
                Err(refusal) => refusal,
                Ok(response) => panic!("a second session started on profile {profile} while {holder} still held it: {:?}", response.sessions),
            };
            let error = refusal.0.error.expect("a refusal names its error");
            assert_eq!(error.code, ErrorCode::ProfileBusy.as_str());
            assert!(error.message.contains(holder.as_str()), "{}", error.message);
        }
    }

    fn wait_for(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(Instant::now() < deadline, "{} never appeared", path.display());
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_failed_session_keeps_its_profile_while_its_vm_is_still_being_removed() {
        let fixture = Fixture::new("blocked");
        let (a, machine) = fixture.ready("a", "shared");
        fixture.set("block");
        let failing = {
            let daemon = Arc::clone(&fixture.daemon);
            let record = fixture.record(&a);
            thread::spawn(move || daemon.fail_session(&record, "browser crashed"))
        };
        wait_for(&fixture.ctl.join("deleting"));

        let held = fixture.record(&a);
        assert_eq!(held.state, SessionState::Failing);
        assert_eq!((held.failure.as_deref(), held.endpoints), (Some("browser crashed"), Some(Endpoints { vnc_port: 15900 })));
        assert!(fixture.endpoint_dir(&machine).exists());
        fixture.second_start_is_refused("b", "shared", &a);
        assert_eq!(fixture.record(&SessionName::new("b").unwrap()).state, SessionState::Stopped);
        assert_eq!(fixture.daemon.store().profiles().unwrap()[0].1, Some(a.clone()));

        fixture.clear("block");
        failing.join().unwrap();
        assert_eq!(fixture.record(&a).state, SessionState::Failed);
        assert_eq!(fixture.record(&a).failure.as_deref(), Some("browser crashed"));
        assert!(!fixture.endpoint_dir(&machine).exists());
        let b = SessionName::new("b").unwrap();
        assert_eq!(fixture.daemon.store().begin_start(&b, Endpoints { vnc_port: 15901 }).unwrap().to_string(), "bctl-b-1");
    }

    #[test]
    fn a_failed_removal_keeps_ownership_and_machine_state_until_a_retry_succeeds() {
        let fixture = Fixture::new("errors");
        let (a, machine) = fixture.ready("a", "shared");
        fixture.set("fail");
        fixture.daemon.fail_session(&fixture.record(&a), "browser crashed");

        let held = fixture.record(&a);
        assert_eq!(held.state, SessionState::Failing);
        assert_eq!((held.generation, held.endpoints), (machine.generation, Some(Endpoints { vnc_port: 15900 })));
        assert!(fixture.endpoint_dir(&machine).exists(), "the endpoints of a VM that may still be alive were discarded");
        fixture.second_start_is_refused("b", "shared", &a);

        assert!(fixture.daemon.retry_failing(&a).is_err());
        assert_eq!(fixture.record(&a).state, SessionState::Failing);
        let refusal = fixture.daemon.up(&a, Some("shared"), Some("img"), None, None).expect_err("up cannot start over a VM that is still being removed");
        assert_eq!(refusal.0.error.unwrap().code, ErrorCode::RuntimeError.as_str());
        fixture.second_start_is_refused("b", "shared", &a);

        fixture.clear("fail");
        fixture.daemon.retry_failing(&a).unwrap();
        let released = fixture.record(&a);
        assert_eq!((released.state, released.failure.as_deref()), (SessionState::Failed, Some("browser crashed")));
        assert!(!fixture.endpoint_dir(&machine).exists());
        let b = SessionName::new("b").unwrap();
        fixture.daemon.store().begin_start(&b, Endpoints { vnc_port: 15901 }).unwrap();
        assert_eq!(fixture.calls().matches("machine delete --name bctl-a-1 -f").count(), 4);
    }

    #[test]
    fn a_restarted_daemon_finishes_the_removal_of_a_failing_session() {
        let fixture = Fixture::new("restore");
        let (a, machine) = fixture.ready("a", "shared");
        fixture.set("fail");
        fixture.daemon.fail_session(&fixture.record(&a), "browser crashed");
        assert_eq!(fixture.record(&a).state, SessionState::Failing);

        fixture.clear("fail");
        fs::write(fixture.ctl.join("ls.json"), r#"[{"name":"bctl-a-1","state":"running"}]"#).unwrap();
        fixture.daemon.restore().unwrap();
        assert_eq!(fixture.record(&a).state, SessionState::Failed);
        assert!(!fixture.endpoint_dir(&machine).exists());
        assert!(fixture.calls().contains("machine delete --name bctl-a-1 -f"));
    }

    #[test]
    fn down_retries_a_failing_session_and_stops_it_only_once_the_vm_is_gone() {
        let fixture = Fixture::new("down");
        let (a, _) = fixture.ready("a", "shared");
        fixture.set("fail");
        fixture.daemon.fail_session(&fixture.record(&a), "browser crashed");

        let refusal = fixture.daemon.down(&a).expect_err("down cannot succeed while the VM is alive");
        assert_eq!(refusal.0.error.unwrap().code, ErrorCode::RuntimeError.as_str());
        assert_eq!(fixture.record(&a).state, SessionState::Failing);

        fixture.clear("fail");
        fixture.daemon.down(&a).ok().expect("down succeeds once the VM is gone");
        let stopped = fixture.record(&a);
        assert_eq!((stopped.state, stopped.failure.as_deref()), (SessionState::Stopped, Some("browser crashed")));
    }

    #[test]
    fn ordinary_down_stops_the_guest_gracefully_before_deleting_its_vm() {
        let fixture = Fixture::new("graceful");
        let (a, _) = fixture.ready("a", "shared");

        fixture.daemon.down(&a).ok().expect("down succeeds");
        let stopped = fixture.record(&a);
        assert_eq!((stopped.state, stopped.failure), (SessionState::Stopped, None));
        let calls = fixture.calls();
        let stop = calls.find("machine stop --name bctl-a-1 --timeout 30").expect("down never asked the guest to stop");
        let delete = calls.find("machine delete --name bctl-a-1 -f").expect("down never deleted the VM");
        assert!(stop < delete, "{calls}");
    }

    #[test]
    fn down_deletes_by_force_when_the_guest_will_not_stop_and_records_that() {
        let fixture = Fixture::new("forced");
        let (a, _) = fixture.ready("a", "shared");
        fixture.set("stop-fail");

        fixture.daemon.down(&a).ok().expect("a forced removal still completes down");
        let stopped = fixture.record(&a);
        assert_eq!(stopped.state, SessionState::Stopped);
        let note = stopped.failure.expect("a forced removal is distinguishable from a graceful one");
        assert!(note.contains("deleted by force") && note.contains("filesystem synchronization"), "{note}");
        assert!(fixture.calls().contains("machine delete --name bctl-a-1 -f"));
    }

    #[test]
    fn a_successful_teardown_releases_the_profile() {
        let fixture = Fixture::new("released");
        let (a, machine) = fixture.ready("a", "shared");
        fixture.daemon.fail_session(&fixture.record(&a), "browser crashed");

        let failed = fixture.record(&a);
        assert_eq!((failed.state, failed.failure.as_deref()), (SessionState::Failed, Some("browser crashed")));
        assert!(!fixture.endpoint_dir(&machine).exists());
        assert!(fixture.calls().contains("machine delete --name bctl-a-1 -f"));
        assert_eq!(fixture.daemon.store().profiles().unwrap()[0].1, None);
        let b = SessionName::new("b").unwrap();
        fixture.daemon.store().create_session(&b, &ProfileName::new("shared").unwrap(), "img", Resources::DEFAULT, &[]).unwrap();
        assert_eq!(fixture.daemon.store().begin_start(&b, Endpoints { vnc_port: 15901 }).unwrap().to_string(), "bctl-b-1");
    }
}
