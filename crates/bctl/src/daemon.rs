use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use bctl_core::{
    Action, AllowedRange, AttemptEnd, AttemptRecord, AttemptState, Endpoints, Generation, Interaction, LiveState, MachineName, MachineState, NewRun,
    Outcome as RunOutcome, ProfileName, Progress, Resources, RunKind, RunRecord, SessionName, SessionRecord, SessionState, Store, StoreError, reconcile,
};
use bctl_runtime::{InputSupport, cdp_socket_browser};
use bctl_runtime_smolvm::{LaunchSource, LaunchSpec, RuntimeError, SmolVm};
use miniserde::json;

use crate::authority::{Authorities, Control};
use crate::gateway::{self, SharedGateway};
use crate::paths::{DEFAULT_IMAGE, Paths};
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

#[derive(Default)]
struct Observed {
    stage: Option<&'static str>,
    input: Option<InputSupport>,
}

pub(crate) struct Daemon {
    paths: Paths,
    store: Mutex<Store>,
    runtime: SmolVm,
    observed: Mutex<HashMap<SessionName, Observed>>,
    live: Mutex<HashMap<SessionName, Arc<Live>>>,
    busy: Mutex<HashSet<SessionName>>,
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
    let daemon = Arc::new(Daemon {
        runtime: SmolVm::new(paths.runtime()),
        paths,
        store: Mutex::new(store),
        observed: Mutex::default(),
        live: Mutex::default(),
        busy: Mutex::default(),
        gateway: Mutex::default(),
        browsers: Mutex::default(),
        authority: Authorities::default(),
    });
    if daemon.paths.runtime().exists() {
        daemon.restore()?;
    } else {
        eprintln!("bctl daemon: no runtime at {}; sessions cannot start until it exists", daemon.paths.runtime().display());
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
        self.record(attempt, self.daemon.store().set_live(attempt, LiveState::Running, None));
    }

    fn needs_interaction(&self, attempt: i64, interaction: &Interaction) {
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
        self.record(live.attempt, self.daemon.store().finish_attempt(live.attempt, &end, now_ms()));
        if sessions.get(&self.session).is_some_and(|current| current.attempt == live.attempt) {
            sessions.remove(&self.session);
        }
        drop(sessions);
        gateway::revoke_cdp_url(&self.daemon, &self.cdp_url);
        self.daemon.collect_garbage();
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
        if let Err(error) = self.store().transition(&record.name, record.generation, SessionState::Failed, Some(cause))
            && !matches!(error, StoreError::StaleGeneration { .. })
        {
            eprintln!("bctl daemon: cannot record {}'s failure: {error}", record.name);
        }
        let machine = MachineName { session: record.name.clone(), generation: record.generation };
        if let Err(error) = self.runtime.remove(&machine) {
            eprintln!("bctl daemon: could not remove failed {machine}: {error}");
        }
        self.discard_endpoints(&machine);
        tabs::close(self, &record.name);
        gateway::revoke_cdp(self, &record.name);
        self.authority.of(&record.name, Control::reset);
        self.observe(&record.name, |o| *o = Observed::default());
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
                    if record.state == SessionState::Stopping {
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
                gateway::revoke_cdp(self, &session()?);
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
            vnc_port: record.endpoints.filter(|_| holds).map(|e| e.vnc_port),
            input: observed.and_then(|o| o.input).filter(|_| holds).map(|i| match i {
                InputSupport::Interactive => "interactive".to_string(),
                InputSupport::ViewOnly => "view-only".to_string(),
                InputSupport::Headless => "headless".to_string(),
            }),
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
        let image = image_for_up(image, existing.as_ref().map(|record| record.image.as_str()));
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
        if !self.paths.image(&image).exists() {
            return Err(Response::failure(
                ErrorCode::ImageMissing,
                format!("no image {image} at {}", self.paths.image(&image).display()),
                Some(format!("build it with `images/build.sh {image}` and copy the archive there")),
            ).into());
        }
        if !self.paths.runtime().exists() {
            return Err(Response::failure(
                ErrorCode::RuntimeMissing,
                format!("no runtime at {}", self.paths.runtime().display()),
                Some("build it with `runtime/build.sh` and copy .work/runtime/smolvm there".into()),
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
        let image_path = match self.paths.image(&image).canonicalize() {
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
        let spec = LaunchSpec {
            machine: machine.clone(),
            source: LaunchSource::Image(image_path),
            profile_dir: Some(profile_dir),
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
                Some(format!("the VM was removed; retry with `bctl up {name}`")),
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
            return Err(Response::failure(ErrorCode::RuntimeError, error.to_string(), Some(format!("see `bctl inspect {name}`"))).into());
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
            return Err(Response::failure(ErrorCode::RuntimeError, error.to_string(), Some(format!("see `bctl inspect {name}`"))).into());
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

    fn resume_spec(&self, record: &SessionRecord, machine: &MachineName) -> Result<LaunchSpec, String> {
        let Resources { viewport: [width, height], cpus, memory_mib } = record.resources;
        let display = (
            u16::try_from(width).map_err(|_| format!("viewport width {width} does not fit a display"))?,
            u16::try_from(height).map_err(|_| format!("viewport height {height} does not fit a display"))?,
        );
        let cpus = u8::try_from(cpus).map_err(|_| format!("{cpus} CPUs do not fit the runtime"))?;
        let endpoints = record.endpoints.ok_or_else(|| format!("session {} has no recorded VNC endpoint", record.name))?;
        let image = self.paths.image(&record.image).canonicalize().map_err(|e| format!("cannot resolve image {}: {e}", record.image))?;
        Ok(LaunchSpec {
            machine: machine.clone(),
            source: LaunchSource::Image(image),
            profile_dir: Some(self.paths.profile_dir(record.profile.as_str())),
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
        fs::create_dir_all(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        Ok(path)
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
        let interactive = observed.get(name).and_then(|o| o.input) == Some(InputSupport::Interactive);
        Some((endpoints.vnc_port, interactive))
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
        if let Err(e) = self.runtime.remove(machine) {
            eprintln!("bctl daemon: could not remove {machine}: {e}");
        }
        self.discard_endpoints(machine);
        let _ = self.store().transition(&machine.session, machine.generation, SessionState::Failed, Some(&error.to_string()));
    }

    fn down(&self, name: &SessionName) -> Outcome {
        let _busy = self.busy(name);
        self.stop_live_run(name);
        gateway::revoke_cdp(self, name);
        tabs::close(self, name);
        self.authority.of(name, Control::reset);
        let record = self.store().session(name).map_err(store_failure)?;
        match record.state {
            SessionState::Stopped => return Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() }),
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
        let image_digest = runs::image_digest(&self.paths.image(&record.image)).map_err(internal)?;
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
            runs::launch(Launch { run, attempt, number, config, files, cdp_url: cdp_url.clone() }, ledger).inspect_err(|_| gateway::revoke_cdp_url(self, &cdp_url))
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
        match self.runtime.remove(machine) {
            Ok(()) => {
                self.discard_endpoints(machine);
                self.observe(&record.name, |o| *o = Observed::default());
                let _ = self.store().transition(&record.name, generation, SessionState::Stopped, None);
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
fn image_for_up(requested: Option<&str>, recorded: Option<&str>) -> String {
    requested.or(recorded).unwrap_or(DEFAULT_IMAGE).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_takes_the_named_image_then_the_recorded_one_then_helium() {
        assert_eq!(image_for_up(Some("ref-alpine-chromium"), Some("helium")), "ref-alpine-chromium");
        assert_eq!(image_for_up(None, Some("ref-scratch-chromium")), "ref-scratch-chromium");
        assert_eq!(image_for_up(None, None), "helium");
    }
}
