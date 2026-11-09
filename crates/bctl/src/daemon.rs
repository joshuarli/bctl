use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use bctl_core::{Action, Endpoints, Generation, MachineName, ProfileName, SessionName, SessionRecord, SessionState, Store, StoreError, reconcile};
use bctl_runtime_smolvm::{InputSupport, LaunchSpec, RuntimeError, SmolVm};
use miniserde::json;

use crate::gateway::{self, SharedGateway};
use crate::paths::Paths;
use crate::protocol::{ErrorCode, Op, ProfileView, Request, Response, SessionView, SessionsPayload};

const DISPLAY: (u16, u16) = (1280, 800);
const MEMORY_MIB: u32 = 4096;
const CPUS: u8 = 2;
const READY_TIMEOUT: Duration = Duration::from_secs(180);

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
    pub(crate) gateway: SharedGateway,
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
        gateway: Mutex::default(),
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
        other => internal(other),
    }
}

impl Daemon {
    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn observe(&self, session: &SessionName, update: impl FnOnce(&mut Observed)) {
        let mut observed = self.observed.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        update(observed.entry(session.clone()).or_default());
    }

    /// Restores ownership after a restart: adopt current-generation VMs, stop stale ones, interrupt orphaned attempts.
    fn restore(self: &Arc<Self>) -> Result<(), String> {
        let machines = self.runtime.list().map_err(|e| e.to_string())?;
        let plan = {
            let store = self.store();
            reconcile(&store.sessions().map_err(|e| e.to_string())?, &store.attempts().map_err(|e| e.to_string())?, &machines)
        };
        self.store().apply(&plan).map_err(|e| e.to_string())?;
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

    fn handle(self: &Arc<Self>, request: &Request) -> Outcome {
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
            Op::Up => self.up(&session()?, request.profile.as_deref(), request.image.as_deref()),
            Op::Down => self.down(&session()?),
            Op::Rm => {
                let name = session()?;
                self.store().delete_session(&name).map_err(store_failure)?;
                Ok(Response::empty())
            }
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
            state: record.state.as_str().to_string(),
            generation: record.generation.0,
            stage: if record.state == SessionState::Starting { observed.and_then(|o| o.stage).map(str::to_string) } else { None },
            failure: record.failure.clone(),
            machine: holds.then(|| MachineName { session: record.name.clone(), generation: record.generation }.to_string()),
            cdp_url: record.endpoints.filter(|_| holds).map(|e| format!("http://127.0.0.1:{}", e.cdp_port)),
            vnc_port: record.endpoints.filter(|_| holds).map(|e| e.vnc_port),
            input: observed.and_then(|o| o.input).filter(|_| holds).map(|i| match i {
                InputSupport::Interactive => "interactive".to_string(),
                InputSupport::ViewOnly => "view-only".to_string(),
            }),
        }
    }

    fn up(&self, name: &SessionName, profile: Option<&str>, image: Option<&str>) -> Outcome {
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
        if let Some(record) = &existing
            && (record.profile != profile || record.image != image)
        {
            return Err(Response::failure(
                ErrorCode::Conflict,
                format!("session {name} exists with profile {} and image {}", record.profile, record.image),
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
            self.store().create_session(name, &profile, &image).map_err(store_failure)?;
        }
        loop {
            let record = self.store().session(name).map_err(store_failure)?;
            match record.state {
                SessionState::Ready => return Ok(Response { sessions: vec![self.view(&record)], ..Response::empty() }),
                SessionState::Starting | SessionState::Stopping => thread::sleep(Duration::from_millis(200)),
                SessionState::Stopped | SessionState::Failed => break,
            }
        }

        let endpoints = Endpoints { cdp_port: free_port().map_err(internal)?, vnc_port: free_port().map_err(internal)? };
        let machine = self.store().begin_start(name, endpoints).map_err(store_failure)?;
        self.observe(name, |o| *o = Observed::default());
        let profile_dir = match self.ensure_profile_dir(&profile) {
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
        let spec = LaunchSpec {
            machine: machine.clone(),
            image: image_path,
            profile_dir: Some(profile_dir),
            endpoints,
            display: DISPLAY,
            memory_mib: MEMORY_MIB,
            cpus: CPUS,
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

    fn ensure_profile_dir(&self, profile: &ProfileName) -> Result<std::path::PathBuf, String> {
        let path = self.paths.profile_dir(profile.as_str());
        fs::create_dir_all(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        Ok(path)
    }

    pub(crate) fn session_views(&self) -> SessionsPayload {
        let sessions = self.store().sessions().unwrap_or_default();
        SessionsPayload { sessions: sessions.iter().map(|s| self.view(s)).collect() }
    }

    fn ready_endpoints(&self, name: &SessionName) -> Option<Endpoints> {
        let record = self.store().session(name).ok()?;
        (record.state == SessionState::Ready).then_some(record.endpoints).flatten()
    }

    pub(crate) fn cdp_port(&self, name: &SessionName) -> Option<u16> {
        self.ready_endpoints(name).map(|e| e.cdp_port)
    }

    /// The VNC port to proxy and whether viewer input may reach the guest; a view-only runtime never accepts input.
    pub(crate) fn vnc_target(&self, name: &SessionName) -> Option<(u16, bctl_rfb::InputPolicy)> {
        let endpoints = self.ready_endpoints(name)?;
        let observed = self.observed.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let interactive = observed.get(name).and_then(|o| o.input) == Some(InputSupport::Interactive);
        Some((endpoints.vnc_port, gateway::input_policy(interactive)))
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
        let result = self.runtime.wait_ready(machine, endpoints, READY_TIMEOUT, |stage| self.observe(session, |o| o.stage = Some(stage.as_str())));
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

    fn finish_stop(&self, record: &SessionRecord, machine: &MachineName) {
        let generation: Generation = record.generation;
        match self.runtime.remove(machine) {
            Ok(()) => {
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

fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Opens the daemon log for appending, owner-only.
pub fn open_log(paths: &Paths) -> std::io::Result<File> {
    fs::create_dir_all(&paths.home)?;
    OpenOptions::new().create(true).append(true).mode(0o600).open(paths.log())
}
