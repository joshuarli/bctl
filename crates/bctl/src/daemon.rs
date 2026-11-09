use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use bctl_core::{
    Action, AllowedRange, AttemptEnd, AttemptRecord, AttemptState, Endpoints, Generation, Interaction, LiveState, MachineName, NewRun, Outcome as RunOutcome,
    ProfileName, Progress, Resources, RunKind, RunRecord, SessionName, SessionRecord, SessionState, Store, StoreError, reconcile,
};
use bctl_runtime_smolvm::{InputSupport, LaunchSpec, RuntimeError, SmolVm, cdp_socket_browser};
use miniserde::json;

use crate::gateway::{self, SharedGateway};
use crate::paths::Paths;
use crate::protocol::{
    AttemptView, ErrorCode, InteractionView, JobRequest, LogView, Op, ProfileView, ProgressView, Request, ResourcesView, Response, RunView, SessionView, SessionsPayload,
};
use crate::runs::{self, BoundedLog, Intent, Launch, Ledger, Live, RunConfig, RunFiles};
use crate::tabs::{self, Browsers, TabCommand};

const READY_TIMEOUT: Duration = Duration::from_secs(180);
const STOP_RUN_TIMEOUT: Duration = Duration::from_secs(30);

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
    pub(crate) gateway: SharedGateway,
    pub(crate) browsers: Browsers,
}

type Outcome = Result<Response, Response>;

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
        gateway: Mutex::default(),
        browsers: Mutex::default(),
    });
    if daemon.paths.runtime().exists() {
        daemon.restore()?;
    } else {
        eprintln!("bctl daemon: no runtime at {}; sessions cannot start until it exists", daemon.paths.runtime().display());
    }

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
    }
}

impl Daemon {
    pub(crate) fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
            Ok(request) => self.handle(&request).unwrap_or_else(|failure| failure),
            Err(_) => Response::failure(ErrorCode::BadRequest, "request is not valid JSON", None),
        };
        let mut writer = &stream;
        let _ = writeln!(writer, "{}", json::to_string(&response));
    }

    pub(crate) fn handle(self: &Arc<Self>, request: &Request) -> Outcome {
        let op = Op::parse(&request.op)
            .ok_or_else(|| Response::failure(ErrorCode::BadRequest, format!("unknown operation {:?}", request.op), None))?;
        let session = || -> Result<SessionName, Response> {
            let name = request.session.as_deref().ok_or_else(|| Response::failure(ErrorCode::BadRequest, "a session name is required", None))?;
            SessionName::new(name).map_err(|e| Response::failure(ErrorCode::BadRequest, e.to_string(), None))
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
                    return Err(Response::failure(ErrorCode::Conflict, format!("session {name} is not ready"), Some(format!("run `bctl up {name}`"))));
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
                        return Err(internal(format!("forgot {name} but cannot remove {}: {error}", dir.display())));
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
            Op::Pause => self.pause(request.run.ok_or_else(|| bad_request("pause needs a run id"))?),
            Op::Resume => self.resume(request.run.ok_or_else(|| bad_request("resume needs a run id"))?),
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
            Err(e) => return Err(internal(e)),
        };
        let image = match (image, &existing) {
            (Some(image), _) => image.to_string(),
            (None, Some(record)) => record.image.clone(),
            (None, None) => return Err(self.image_required()),
        };
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
            ));
        }
        if !self.paths.image(&image).exists() {
            return Err(Response::failure(
                ErrorCode::ImageMissing,
                format!("no image {image} at {}", self.paths.image(&image).display()),
                Some(format!("build it with `images/build.sh {image}` and copy the archive there")),
            ));
        }
        if !self.paths.runtime().exists() {
            return Err(Response::failure(
                ErrorCode::RuntimeMissing,
                format!("no runtime at {}", self.paths.runtime().display()),
                Some("build it with `runtime/build.sh` and copy .work/runtime/smolvm there".into()),
            ));
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
                SessionState::Stopped | SessionState::Failed => break,
            }
        }

        let configured = self.store().session(name).map_err(store_failure)?;
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
                return Err(internal(error));
            }
        };
        let image_path = match self.paths.image(&image).canonicalize() {
            Ok(path) => path,
            Err(error) => {
                let message = format!("cannot resolve image {image}: {error}");
                let _ = self.store().transition(name, machine.generation, SessionState::Failed, Some(&message));
                return Err(Response::failure(ErrorCode::ImageMissing, message, None));
            }
        };
        let cdp_socket = match self.private_cdp_dir(&machine) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.store().transition(name, machine.generation, SessionState::Failed, Some(&error));
                return Err(internal(error));
            }
        };
        let spec = LaunchSpec {
            machine: machine.clone(),
            image: image_path,
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
            return Err(Response::failure(ErrorCode::StartFailed, error.to_string(), Some(format!("see `bctl inspect {name}`"))));
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
            ))
        }
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
        self.down(name).map(drop).map_err(|failure| failure.error.map(|e| e.message).unwrap_or_default())
    }

    fn image_required(&self) -> Response {
        let available: Vec<String> = fs::read_dir(self.paths.images())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".tar").map(str::to_string))
            .collect();
        Response::failure(
            ErrorCode::ImageRequired,
            "a new session needs --image; there is no default image until the Helium image exists",
            Some(if available.is_empty() { "no images installed".into() } else { format!("installed: {}", available.join(", ")) }),
        )
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
        let _ = self.store().transition(&machine.session, machine.generation, SessionState::Failed, Some(&error.to_string()));
    }

    fn down(&self, name: &SessionName) -> Outcome {
        self.stop_live_run(name);
        gateway::revoke_cdp(self, name);
        tabs::close(self, name);
        let record = self.store().session(name).map_err(store_failure)?;
        match record.state {
            SessionState::Stopped => return Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() }),
            SessionState::Failed => {
                self.store().transition(name, record.generation, SessionState::Stopped, record.failure.as_deref()).map_err(store_failure)?;
                let record = self.store().session(name).map_err(store_failure)?;
                return Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() });
            }
            SessionState::Starting | SessionState::Ready | SessionState::Stopping => {}
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
            )),
        }
    }

    fn start_run(self: &Arc<Self>, session: &SessionName, job: &JobRequest) -> Outcome {
        let (exec, grab) = match (&job.exec, &job.grab) {
            (Some(exec), None) => {
                if exec.argv.first().is_none_or(String::is_empty) {
                    return Err(bad_request("an exec job needs a non-empty argv"));
                }
                if !std::path::Path::new(&exec.cwd).is_absolute() {
                    return Err(bad_request(format!("exec cwd {:?} is not absolute", exec.cwd)));
                }
                (Some(exec.clone()), None)
            }
            (None, Some(grab)) => {
                if grab.urls.is_empty() {
                    return Err(bad_request("a grab job needs at least one URL"));
                }
                (None, Some(grab.clone()))
            }
            _ => return Err(bad_request("a job is exactly one of exec and grab")),
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
                        Some(format!("start one with `bctl up {session} --image IMAGE` first")),
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
            ));
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

    fn browser_version(&self, session: &SessionName) -> Result<String, Response> {
        let not_ready = || {
            Response::failure(ErrorCode::SessionNotReady, format!("session {session} does not answer CDP"), Some(format!("see `bctl inspect {session}`")))
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
            ));
        }
        match (record.current.state, self.live_handle(&record)) {
            (AttemptState::Paused, _) => {}
            (state, Some(live)) if state.is_live() => live.request(Intent::Pause),
            (state, _) => return Err(not_live(run, state)),
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
                ));
            }
        }
        if record.spec.kind == RunKind::Exec {
            return Err(Response::failure(
                ErrorCode::NotResumable,
                format!("run {run} is an exec run, which keeps no state to continue from"),
                Some("start a new one with `bctl run`".into()),
            ));
        }
        let session = record.spec.session.clone();
        if self.cdp_socket(&session).is_none() {
            return Err(Response::failure(ErrorCode::SessionNotReady, format!("session {session} is not ready"), Some(format!("run `bctl up {session}` first"))));
        }
        let config: RunConfig = json::from_str(&record.spec.config).map_err(|_| internal(format!("run {run} has an unreadable config")))?;
        let mut live = lock(&self.live);
        if let Some(active) = live.get(&session) {
            return Err(Response::failure(ErrorCode::RunActive, format!("session {session} is already running run {}", active.run), None));
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
            (state, _) => return Err(not_live(run, state)),
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
                let _ = fs::remove_dir_all(self.paths.cdp_socket(&machine.to_string()).parent().expect("a socket path has a directory"));
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
