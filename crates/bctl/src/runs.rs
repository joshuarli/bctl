use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bctl_core::{AttemptEnd, AttemptRecord, Interaction, Outcome, Progress, RunKind};
use miniserde::{Deserialize, Serialize, json};
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use sha2::{Digest, Sha256};

use crate::protocol::{ExecRequest, GrabRequest, ResourcesView};

pub(crate) const LOG_CAP_BYTES: u64 = 8 << 20;
const POLL: Duration = Duration::from_millis(200);
const EXEC_TERM_GRACE: Duration = Duration::from_secs(5);
const GRAB_CANCEL_GRACE: Duration = Duration::from_secs(60);
const GRAB_TERM_GRACE: Duration = Duration::from_secs(15);
const ORPHAN_TERM_GRACE: Duration = Duration::from_secs(15);
const PUMP_DRAIN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RunConfig {
    pub job: Option<String>,
    pub session: String,
    pub profile: String,
    pub image: String,
    pub resources: ResourcesView,
    pub allow_private: Vec<String>,
    pub exec: Option<ExecRequest>,
    pub grab: Option<GrabRequest>,
    pub grab_program: Option<String>,
}

pub(crate) enum Work<'a> {
    Exec(&'a ExecRequest),
    Grab { program: &'a str, grab: &'a GrabRequest },
}

impl RunConfig {
    pub fn work(&self) -> Result<Work<'_>, String> {
        match (&self.exec, &self.grab, &self.grab_program) {
            (Some(exec), None, None) => Ok(Work::Exec(exec)),
            (None, Some(grab), Some(program)) => Ok(Work::Grab { program, grab }),
            _ => Err("a run config names exactly one of exec and grab, and a program only for grab".into()),
        }
    }

    pub fn kind(&self) -> Result<RunKind, String> {
        Ok(match self.work()? {
            Work::Exec(_) => RunKind::Exec,
            Work::Grab { .. } => RunKind::Grab,
        })
    }

    pub fn to_json(&self) -> String {
        json::to_string(self)
    }

    pub fn input_digest(&self) -> String {
        let hash = Sha256::digest(self.to_json().as_bytes());
        format!("sha256:{}", hash.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
}

pub(crate) fn grab_program() -> String {
    std::env::var("BCTL_GRAB").unwrap_or_else(|_| "grab".to_string())
}

pub(crate) struct RunFiles {
    dir: PathBuf,
}

impl RunFiles {
    pub fn new(dir: PathBuf) -> RunFiles {
        RunFiles { dir }
    }

    pub fn output(&self) -> PathBuf {
        self.dir.join("output")
    }

    pub fn log(&self) -> PathBuf {
        self.dir.join("log")
    }

    fn events(&self, attempt: u32) -> PathBuf {
        self.dir.join(format!("attempt-{attempt}.events.jsonl"))
    }

    fn control(&self, attempt: u32) -> PathBuf {
        self.dir.join(format!("attempt-{attempt}.control"))
    }

    fn lock(&self, attempt: u32) -> PathBuf {
        self.dir.join(format!("attempt-{attempt}.lock"))
    }
}

pub(crate) struct BoundedLog {
    file: File,
    written: u64,
    discarded: u64,
}

pub(crate) type SharedLog = Arc<Mutex<BoundedLog>>;

impl BoundedLog {
    pub fn open(path: &Path) -> io::Result<SharedLog> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let written = file.metadata()?.len();
        Ok(Arc::new(Mutex::new(BoundedLog { file, written, discarded: 0 })))
    }

    fn write(&mut self, bytes: &[u8]) {
        let room = LOG_CAP_BYTES.saturating_sub(self.written);
        let kept = bytes.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        if kept > 0 && self.file.write_all(&bytes[..kept]).is_ok() {
            self.written += kept as u64;
        }
        if kept < bytes.len() {
            if self.discarded == 0 {
                let _ = writeln!(self.file, "\n[bctl: the log reached {LOG_CAP_BYTES} bytes; later output is discarded]");
            }
            self.discarded += (bytes.len() - kept) as u64;
        }
    }

    pub fn note(log: &SharedLog, line: &str) {
        lock(log).write(format!("[bctl] {line}\n").as_bytes());
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn read_log(path: &Path) -> io::Result<String> {
    match fs::read(path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Intent {
    Proceed,
    Pause,
    Cancel,
    Stop,
}

pub(crate) struct Live {
    pub run: i64,
    pub attempt: i64,
    pub kind: RunKind,
    control: PathBuf,
    intent: Mutex<Intent>,
}

impl Live {
    pub fn request(&self, intent: Intent) {
        let mut current = lock(&self.intent);
        let rank = |i: Intent| match i {
            Intent::Proceed => 0,
            Intent::Pause => 1,
            Intent::Cancel => 2,
            Intent::Stop => 3,
        };
        if rank(intent) > rank(*current) {
            *current = intent;
        }
    }

    fn intent(&self) -> Intent {
        *lock(&self.intent)
    }

    pub fn resume(&self) -> io::Result<()> {
        append_command(&self.control, "resume")
    }
}

fn append_command(control: &Path, command: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().append(true).open(control)?;
    writeln!(file, "{command}")
}

pub(crate) trait Ledger: Send + Sync + 'static {
    fn set_pid(&self, attempt: i64, pid: u32);
    fn running(&self, attempt: i64);
    fn needs_interaction(&self, attempt: i64, interaction: &Interaction);
    fn progress(&self, attempt: i64, progress: Progress);
    fn browser_alive(&self) -> bool;
    fn finish(&self, live: &Live, end: AttemptEnd);
}

pub(crate) struct Launch<'a> {
    pub run: i64,
    pub attempt: i64,
    pub number: u32,
    pub config: &'a RunConfig,
    pub files: RunFiles,
    pub cdp_url: String,
}

pub(crate) fn launch(launch: Launch<'_>, ledger: Arc<dyn Ledger>) -> Result<Arc<Live>, String> {
    let Launch { run, attempt, number, config, files, cdp_url } = launch;
    let output = files.output();
    fs::create_dir_all(&output).map_err(|e| format!("cannot create {}: {e}", output.display()))?;
    let log = BoundedLog::open(&files.log()).map_err(|e| format!("cannot open {}: {e}", files.log().display()))?;
    let control = files.control(number);
    File::create(&control).map_err(|e| format!("cannot create {}: {e}", control.display()))?;
    let events = files.events(number);
    let work = config.work()?;
    let (program, args, cwd, timeout) = match &work {
        Work::Exec(exec) => (exec.argv[0].clone(), exec.argv[1..].to_vec(), PathBuf::from(&exec.cwd), exec.timeout_secs.map(Duration::from_secs)),
        Work::Grab { program, grab } => ((*program).to_string(), grab_args(grab, &cdp_url, &output, &events, &control), output.clone(), None),
    };
    let kind = config.kind()?;
    BoundedLog::note(&log, &format!("attempt {number}: {program} {args:?}"));
    let mut command = Command::new(&program);
    command
        .args(&args)
        .current_dir(&cwd)
        .env("BCTL_CDP_URL", &cdp_url)
        .env("BCTL_OUTPUT_DIR", &output)
        .env("BCTL_RUN", run.to_string());
    let child = spawn_held(command, &files.lock(number));
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            let detail = format!("cannot start {program}: {error}");
            BoundedLog::note(&log, &detail);
            return Err(detail);
        }
    };
    ledger.set_pid(attempt, child.id());
    let pumps = pump_output(&mut child, &log);
    let live = Arc::new(Live { run, attempt, kind, control, intent: Mutex::new(Intent::Proceed) });
    let supervised = Supervised {
        live: Arc::clone(&live),
        ledger,
        log,
        events: EventTail::new(events),
        lock: files.lock(number),
        output,
        grab_program: match work {
            Work::Grab { program, .. } => Some(program.to_string()),
            Work::Exec(_) => None,
        },
        deadline: timeout.map(|t| (Instant::now() + t, t)),
    };
    if kind == RunKind::Exec {
        supervised.ledger.running(attempt);
    }
    thread::spawn(move || supervised.run(child, pumps));
    Ok(live)
}

fn grab_args(grab: &GrabRequest, cdp_url: &str, output: &Path, events: &Path, control: &Path) -> Vec<String> {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let mut args = vec!["--out".into(), path(output), "--control-url".into(), cdp_url.into(), "--events".into(), path(events), "--control".into(), path(control)];
    if let Some(pages) = grab.max_pages {
        args.extend(["--max-pages".into(), pages.to_string()]);
    }
    if let Some(jobs) = grab.jobs {
        args.extend(["--jobs".into(), jobs.to_string()]);
    }
    if grab.robots {
        args.push("--robots".into());
    }
    if let Some(seconds) = grab.max_runtime_secs {
        args.extend(["--max-runtime".into(), seconds.to_string()]);
    }
    args.push("--".into());
    args.extend(grab.urls.iter().cloned());
    args
}

/// flock(2) locks belong to the open file description, which every process of the group inherits as stdin, so the
/// lock stays held exactly while one of them lives.
fn spawn_held(mut command: Command, lock_path: &Path) -> io::Result<Child> {
    let held = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_path)?;
    held.try_lock().map_err(|e| io::Error::other(format!("{} is already locked: {e}", lock_path.display())))?;
    command.stdin(Stdio::from(held)).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).spawn()
}

fn pump_output(child: &mut Child, log: &SharedLog) -> Vec<JoinHandle<()>> {
    let mut pumps = Vec::new();
    let streams: [Option<Box<dyn Read + Send>>; 2] =
        [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)];
    for mut stream in streams.into_iter().flatten() {
        let log = Arc::clone(log);
        pumps.push(thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                lock(&log).write(&chunk[..n]);
            }
        }));
    }
    pumps
}

fn signal_group(pid: u32, signal: Signal) -> io::Result<()> {
    let pid = i32::try_from(pid).ok().and_then(Pid::from_raw).ok_or_else(|| io::Error::other(format!("pid {pid} is not a process group")))?;
    match kill_process_group(pid, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(errno) => Err(errno.into()),
    }
}

fn group_exists(pid: u32) -> bool {
    i32::try_from(pid).ok().and_then(Pid::from_raw).is_some_and(|pid| test_kill_process_group(pid).is_ok())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finished {
    pub outcome: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GrabEvent {
    Started,
    Progress(Progress),
    NeedsInteraction(Interaction),
    Resumed,
    Finished(Finished),
    Other,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "type")]
    kind: String,
    pages_done: Option<u32>,
    pages_queued: Option<u32>,
    assets_done: Option<u32>,
    reason: Option<String>,
    url: Option<String>,
    outcome: Option<String>,
    detail: Option<String>,
}

/// Grab's event schema asks consumers to ignore record types they do not know.
fn parse_event(line: &str) -> Result<GrabEvent, String> {
    let raw: RawEvent = json::from_str(line).map_err(|_| format!("unreadable event {line:?}"))?;
    let missing = |field: &str| format!("{} event without {field}: {line:?}", raw.kind);
    Ok(match raw.kind.as_str() {
        "started" => GrabEvent::Started,
        "progress" => GrabEvent::Progress(Progress {
            pages_done: raw.pages_done.ok_or_else(|| missing("pages_done"))?,
            pages_queued: raw.pages_queued.ok_or_else(|| missing("pages_queued"))?,
            assets_done: raw.assets_done.ok_or_else(|| missing("assets_done"))?,
        }),
        "needs_interaction" => GrabEvent::NeedsInteraction(Interaction {
            reason: raw.reason.clone().ok_or_else(|| missing("reason"))?,
            url: raw.url.clone().ok_or_else(|| missing("url"))?,
        }),
        "resumed" => GrabEvent::Resumed,
        "finished" => GrabEvent::Finished(Finished {
            outcome: raw.outcome.clone().ok_or_else(|| missing("outcome"))?,
            detail: raw.detail.clone().unwrap_or_default(),
        }),
        _ => GrabEvent::Other,
    })
}

struct EventTail {
    path: PathBuf,
    file: Option<File>,
    offset: u64,
    partial: Vec<u8>,
}

impl EventTail {
    fn new(path: PathBuf) -> EventTail {
        EventTail { path, file: None, offset: 0, partial: Vec::new() }
    }

    fn lines(&mut self) -> Vec<String> {
        if self.file.is_none() {
            self.file = File::open(&self.path).ok();
        }
        let Some(file) = self.file.as_mut() else { return Vec::new() };
        let mut fresh = Vec::new();
        if file.seek(SeekFrom::Start(self.offset)).is_ok() && file.read_to_end(&mut fresh).is_ok() {
            self.offset += fresh.len() as u64;
            self.partial.extend_from_slice(&fresh);
        }
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else { return Vec::new() };
        let complete: Vec<u8> = self.partial.drain(..=end).collect();
        String::from_utf8_lossy(&complete).lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    Code(i32),
    Signal(i32),
}

impl Exit {
    fn from_status(status: ExitStatus) -> Exit {
        match (status.code(), status.signal()) {
            (Some(code), _) => Exit::Code(code),
            (None, Some(signal)) => Exit::Signal(signal),
            (None, None) => Exit::Code(-1),
        }
    }

    fn describe(self) -> String {
        match self {
            Exit::Code(code) => format!("exited with status {code}"),
            Exit::Signal(signal) => format!("was killed by signal {signal}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GrabVerdict {
    Verify,
    Settle(AttemptEnd),
}

fn ended(outcome: Outcome, detail: impl Into<String>) -> AttemptEnd {
    AttemptEnd::Ended { outcome, detail: Some(detail.into()) }
}

pub(crate) fn exec_outcome(exit: Exit, intent: Intent, timed_out: Option<Duration>, browser_alive: bool) -> AttemptEnd {
    match (intent, timed_out, exit) {
        (Intent::Stop, _, _) => ended(Outcome::Cancelled, "the session was stopped"),
        (Intent::Cancel | Intent::Pause, _, _) => ended(Outcome::Cancelled, "cancelled by `bctl cancel`"),
        (Intent::Proceed, Some(limit), _) => ended(Outcome::JobError, format!("timed out after {}s and was stopped", limit.as_secs())),
        (Intent::Proceed, None, Exit::Code(0)) => AttemptEnd::Ended { outcome: Outcome::Succeeded, detail: None },
        (Intent::Proceed, None, exit) if !browser_alive => {
            ended(Outcome::BrowserCrashed, format!("the program {} and the browser no longer answers CDP", exit.describe()))
        }
        (Intent::Proceed, None, exit) => ended(Outcome::JobError, format!("the program {}", exit.describe())),
    }
}

const PAUSED_DETAIL: &str = "paused at a page boundary; `bctl resume` continues from Grab's state.db";

pub(crate) fn grab_outcome(finished: Option<&Finished>, pending: Option<&Interaction>, intent: Intent, exit: Exit, browser_alive: bool) -> GrabVerdict {
    let settle = GrabVerdict::Settle;
    let reported = finished.map(|f| f.outcome.as_str());
    match intent {
        Intent::Stop => return settle(ended(Outcome::Cancelled, "the session was stopped")),
        Intent::Cancel => return settle(ended(Outcome::Cancelled, "cancelled by `bctl cancel`")),
        Intent::Pause if matches!(reported, Some("cancelled" | "interrupted")) => {
            return settle(AttemptEnd::Paused { detail: PAUSED_DETAIL.into() });
        }
        Intent::Pause | Intent::Proceed => {}
    }
    let detail = finished.map(|f| f.detail.clone()).filter(|d| !d.is_empty());
    match reported {
        Some("succeeded") => GrabVerdict::Verify,
        Some("cancelled") => settle(ended(Outcome::Cancelled, detail.unwrap_or_else(|| "Grab reports the crawl cancelled".into()))),
        Some("interrupted") => settle(ended(Outcome::Interrupted, detail.unwrap_or_else(|| "Grab was interrupted".into()))),
        _ if !browser_alive => settle(ended(
            Outcome::BrowserCrashed,
            detail.unwrap_or_else(|| format!("grab {} and the browser no longer answers CDP", exit.describe())),
        )),
        _ if pending.is_some_and(|i| i.reason == "login") => {
            settle(ended(Outcome::LoginRequired, format!("the crawl ended waiting for a login at {}", pending.map_or("", |i| &i.url))))
        }
        _ if pending.is_some() => {
            settle(ended(Outcome::SiteRejected, format!("the crawl ended blocked by a challenge at {}", pending.map_or("", |i| &i.url))))
        }
        Some(_) if detail.as_deref().is_some_and(|d| d.contains("bot-protection")) => {
            settle(ended(Outcome::SiteRejected, detail.unwrap_or_default()))
        }
        Some(outcome) => settle(ended(Outcome::JobError, detail.unwrap_or_else(|| format!("Grab reports the crawl {outcome}")))),
        None => settle(ended(Outcome::JobError, format!("grab {} without a finished event", exit.describe()))),
    }
}

struct Supervised {
    live: Arc<Live>,
    ledger: Arc<dyn Ledger>,
    log: SharedLog,
    events: EventTail,
    lock: PathBuf,
    output: PathBuf,
    grab_program: Option<String>,
    deadline: Option<(Instant, Duration)>,
}

#[derive(Default)]
struct Escalation {
    asked: Option<Instant>,
    termed: Option<Instant>,
    killed: bool,
}

struct GrabState {
    finished: Option<Finished>,
    pending: Option<Interaction>,
}

impl Supervised {
    fn run(mut self, mut child: Child, pumps: Vec<JoinHandle<()>>) {
        let pid = child.id();
        let mut escalation = Escalation::default();
        let mut timed_out = None;
        let mut grab = GrabState { finished: None, pending: None };
        let status = loop {
            self.read_events(&mut grab);
            match child.try_wait() {
                Ok(Some(status)) => break Exit::from_status(status),
                Ok(None) => {}
                Err(error) => {
                    BoundedLog::note(&self.log, &format!("cannot wait for process {pid}: {error}"));
                    break Exit::Code(-1);
                }
            }
            if let Some((deadline, limit)) = self.deadline
                && timed_out.is_none()
                && Instant::now() >= deadline
            {
                BoundedLog::note(&self.log, &format!("timeout of {}s reached; stopping process group {pid}", limit.as_secs()));
                timed_out = Some(limit);
            }
            self.escalate(pid, &mut escalation, timed_out.is_some());
            thread::sleep(POLL);
        };
        self.read_events(&mut grab);
        if let Err(error) = signal_group(pid, Signal::KILL) {
            BoundedLog::note(&self.log, &format!("cannot clear process group {pid}: {error}"));
        }
        drain(pumps);
        let intent = self.live.intent();
        let alive = self.ledger.browser_alive();
        let end = match self.live.kind {
            RunKind::Exec => exec_outcome(status, intent, timed_out, alive),
            RunKind::Grab => match grab_outcome(grab.finished.as_ref(), grab.pending.as_ref(), intent, status, alive) {
                GrabVerdict::Settle(end) => end,
                GrabVerdict::Verify => self.verify(),
            },
        };
        let summary = match &end {
            AttemptEnd::Paused { detail } => format!("paused: {detail}"),
            AttemptEnd::Ended { outcome, detail } => format!("{}{}", outcome.as_str(), detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default()),
        };
        BoundedLog::note(&self.log, &format!("attempt ended {summary}"));
        self.ledger.finish(&self.live, end);
    }

    fn escalate(&self, pid: u32, escalation: &mut Escalation, timed_out: bool) {
        let intent = self.live.intent();
        let stopping = timed_out || intent != Intent::Proceed;
        if !stopping {
            return;
        }
        let now = Instant::now();
        let immediate = timed_out || intent == Intent::Stop || self.live.kind == RunKind::Exec;
        if escalation.asked.is_none() {
            escalation.asked = Some(now);
            if !immediate {
                match append_command(&self.live.control, "cancel") {
                    Ok(()) => BoundedLog::note(&self.log, "asked Grab to stop at the next page boundary"),
                    Err(error) => BoundedLog::note(&self.log, &format!("cannot write cancel to the control channel: {error}")),
                }
            }
        }
        let asked = escalation.asked.unwrap_or(now);
        let (ask_grace, term_grace) = match self.live.kind {
            RunKind::Exec => (Duration::ZERO, EXEC_TERM_GRACE),
            RunKind::Grab => (if immediate { Duration::ZERO } else { GRAB_CANCEL_GRACE }, GRAB_TERM_GRACE),
        };
        if escalation.termed.is_none() && now.duration_since(asked) >= ask_grace {
            escalation.termed = Some(now);
            if let Err(error) = signal_group(pid, Signal::TERM) {
                BoundedLog::note(&self.log, &format!("cannot signal process group {pid}: {error}"));
            }
        }
        if let Some(termed) = escalation.termed
            && !escalation.killed
            && now.duration_since(termed) >= term_grace
        {
            escalation.killed = true;
            BoundedLog::note(&self.log, &format!("process group {pid} ignored SIGTERM for {}s; killing it", term_grace.as_secs()));
            if let Err(error) = signal_group(pid, Signal::KILL) {
                BoundedLog::note(&self.log, &format!("cannot kill process group {pid}: {error}"));
            }
        }
    }

    fn read_events(&mut self, grab: &mut GrabState) {
        if self.live.kind != RunKind::Grab {
            return;
        }
        let mut progress = None;
        for line in self.events.lines() {
            match parse_event(&line) {
                Ok(GrabEvent::Started | GrabEvent::Resumed) => {
                    grab.pending = None;
                    self.ledger.running(self.live.attempt);
                }
                Ok(GrabEvent::Progress(p)) => progress = Some(p),
                Ok(GrabEvent::NeedsInteraction(interaction)) => {
                    BoundedLog::note(&self.log, &format!("Grab needs {} at {}; `bctl resume {}` continues", interaction.reason, interaction.url, self.live.run));
                    self.ledger.needs_interaction(self.live.attempt, &interaction);
                    grab.pending = Some(interaction);
                }
                Ok(GrabEvent::Finished(finished)) => grab.finished = Some(finished),
                Ok(GrabEvent::Other) => {}
                Err(error) => BoundedLog::note(&self.log, &error),
            }
        }
        if let Some(progress) = progress {
            self.ledger.progress(self.live.attempt, progress);
        }
    }

    fn verify(&self) -> AttemptEnd {
        let Some(program) = &self.grab_program else { return ended(Outcome::JobError, "a grab attempt has no Grab program") };
        let mut mirrors: Vec<PathBuf> = fs::read_dir(&self.output)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join("_grab/state.db").is_file())
            .collect();
        mirrors.sort();
        if mirrors.is_empty() {
            return ended(Outcome::VerificationFailed, format!("no mirror with a _grab/state.db under {}", self.output.display()));
        }
        for mirror in &mirrors {
            BoundedLog::note(&self.log, &format!("verifying {}", mirror.display()));
            let mut command = Command::new(program);
            command.arg("verify").arg(mirror).current_dir(&self.output);
            let mut child = match spawn_held(command, &self.lock) {
                Ok(child) => child,
                Err(error) => return ended(Outcome::VerificationFailed, format!("cannot start `{program} verify`: {error}")),
            };
            let pid = child.id();
            self.ledger.set_pid(self.live.attempt, pid);
            let pumps = pump_output(&mut child, &self.log);
            let mut escalation = Escalation::default();
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Exit::from_status(status),
                    Ok(None) => {}
                    Err(error) => return ended(Outcome::VerificationFailed, format!("cannot wait for `grab verify`: {error}")),
                }
                if matches!(self.live.intent(), Intent::Cancel | Intent::Stop) {
                    escalation.asked.get_or_insert_with(Instant::now);
                    if escalation.termed.is_none() {
                        escalation.termed = Some(Instant::now());
                        let _ = signal_group(pid, Signal::TERM);
                    }
                }
                thread::sleep(POLL);
            };
            let _ = signal_group(pid, Signal::KILL);
            drain(pumps);
            match self.live.intent() {
                Intent::Stop => return ended(Outcome::Cancelled, "the session was stopped during verification"),
                Intent::Cancel => return ended(Outcome::Cancelled, "cancelled by `bctl cancel` during verification"),
                Intent::Pause | Intent::Proceed => {}
            }
            if status != Exit::Code(0) {
                return ended(
                    Outcome::VerificationFailed,
                    format!("`grab verify {}` {}; see `bctl logs {}`", mirror.display(), status.describe(), self.live.run),
                );
            }
        }
        AttemptEnd::Ended { outcome: Outcome::Succeeded, detail: Some(format!("verified {} mirror(s)", mirrors.len())) }
    }
}

fn drain(pumps: Vec<JoinHandle<()>>) {
    let deadline = Instant::now() + PUMP_DRAIN;
    while pumps.iter().any(|p| !p.is_finished()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    for pump in pumps.into_iter().filter(|p| p.is_finished()) {
        let _ = pump.join();
    }
}

/// A process group that exists without holding the attempt's lock cannot be told apart from a reused id.
pub(crate) fn stop_orphan(files: &RunFiles, attempt: &AttemptRecord) -> String {
    let path = files.lock(attempt.number);
    let lock = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return "no process had been started".into(),
        Err(error) => return format!("cannot open {}: {error}", path.display()),
    };
    if lock.try_lock().is_ok() {
        return match attempt.pid {
            Some(pid) if group_exists(pid) => {
                format!("process group {pid} still exists but no longer holds the attempt's lock, so it was left alone")
            }
            _ => "its process had already exited".into(),
        };
    }
    let Some(pid) = attempt.pid else { return format!("{} is locked but the attempt recorded no process", path.display()) };
    let _ = signal_group(pid, Signal::TERM);
    let deadline = Instant::now() + ORPHAN_TERM_GRACE;
    while Instant::now() < deadline {
        if lock.try_lock().is_ok() {
            return format!("process group {pid} outlived the daemon and was stopped with SIGTERM");
        }
        thread::sleep(POLL);
    }
    let _ = signal_group(pid, Signal::KILL);
    format!("process group {pid} outlived the daemon and ignored SIGTERM, so it was killed")
}

/// An OCI layout archive names its image by the manifest digest in `index.json`.
pub(crate) fn image_digest(archive: &Path) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Index {
        manifests: Vec<Descriptor>,
    }
    #[derive(Deserialize)]
    struct Descriptor {
        digest: String,
    }
    let index = tar_member(archive, "index.json").map_err(|e| format!("cannot read {}: {e}", archive.display()))?;
    let index: Index = json::from_str(&String::from_utf8_lossy(&index)).map_err(|_| format!("{} has an unreadable index.json", archive.display()))?;
    match index.manifests.as_slice() {
        [one] => Ok(one.digest.clone()),
        many => Err(format!("{} lists {} manifests, expected one", archive.display(), many.len())),
    }
}

fn tar_member(archive: &Path, wanted: &str) -> io::Result<Vec<u8>> {
    let mut file = File::open(archive)?;
    let mut header = [0u8; 512];
    loop {
        file.read_exact(&mut header)?;
        if header.iter().all(|&b| b == 0) {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("no {wanted} in the archive")));
        }
        let field = |range: std::ops::Range<usize>| String::from_utf8_lossy(&header[range]).trim_end_matches('\0').to_string();
        let size_text = field(124..136);
        let size = u64::from_str_radix(size_text.trim(), 8).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("bad tar size {size_text:?}")))?;
        let name = field(0..100);
        let prefix = field(345..500);
        let full = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if full.trim_start_matches("./") == wanted {
            let mut body = vec![0u8; usize::try_from(size).map_err(io::Error::other)?];
            file.read_exact(&mut body)?;
            return Ok(body);
        }
        file.seek(SeekFrom::Current(i64::try_from(size.div_ceil(512) * 512).map_err(io::Error::other)?))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(outcome: &str, detail: &str) -> Finished {
        Finished { outcome: outcome.into(), detail: detail.into() }
    }

    fn challenge() -> Interaction {
        Interaction { reason: "challenge".into(), url: "http://site/".into() }
    }

    fn outcome_of(verdict: GrabVerdict) -> Option<Outcome> {
        match verdict {
            GrabVerdict::Settle(AttemptEnd::Ended { outcome, .. }) => Some(outcome),
            _ => None,
        }
    }

    #[test]
    fn an_exec_attempt_is_categorized_by_what_stopped_it() {
        let ok = Exit::Code(0);
        assert!(matches!(exec_outcome(ok, Intent::Proceed, None, true), AttemptEnd::Ended { outcome: Outcome::Succeeded, .. }));
        assert!(matches!(exec_outcome(Exit::Code(3), Intent::Proceed, None, true), AttemptEnd::Ended { outcome: Outcome::JobError, detail: Some(d) } if d.contains("status 3")));
        assert!(matches!(exec_outcome(Exit::Code(1), Intent::Proceed, None, false), AttemptEnd::Ended { outcome: Outcome::BrowserCrashed, .. }));
        assert!(matches!(exec_outcome(Exit::Signal(15), Intent::Proceed, Some(Duration::from_secs(2)), true), AttemptEnd::Ended { outcome: Outcome::JobError, detail: Some(d) } if d.contains("timed out after 2s")));
        assert!(matches!(exec_outcome(Exit::Signal(15), Intent::Cancel, None, true), AttemptEnd::Ended { outcome: Outcome::Cancelled, .. }));
        assert!(matches!(exec_outcome(ok, Intent::Cancel, None, true), AttemptEnd::Ended { outcome: Outcome::Cancelled, .. }), "a cancel wins over a late clean exit");
    }

    #[test]
    fn a_grab_success_is_verified_before_it_counts() {
        assert_eq!(grab_outcome(Some(&finished("succeeded", "crawl completed")), None, Intent::Proceed, Exit::Code(0), true), GrabVerdict::Verify);
    }

    #[test]
    fn a_paused_grab_is_resumable_and_a_cancelled_one_is_not() {
        let paused = grab_outcome(Some(&finished("cancelled", "cancel requested")), Some(&challenge()), Intent::Pause, Exit::Code(0), true);
        assert!(matches!(paused, GrabVerdict::Settle(AttemptEnd::Paused { .. })));
        let cancelled = grab_outcome(Some(&finished("cancelled", "cancel requested")), None, Intent::Cancel, Exit::Code(0), true);
        assert_eq!(outcome_of(cancelled), Some(Outcome::Cancelled));
        let interrupted = grab_outcome(Some(&finished("interrupted", "SIGTERM")), None, Intent::Proceed, Exit::Code(130), true);
        assert_eq!(outcome_of(interrupted), Some(Outcome::Interrupted));
    }

    #[test]
    fn a_failed_grab_names_the_browser_the_site_or_the_job() {
        let failed = finished("failed", "1 site failed");
        assert_eq!(outcome_of(grab_outcome(Some(&failed), None, Intent::Proceed, Exit::Code(1), false)), Some(Outcome::BrowserCrashed));
        assert_eq!(outcome_of(grab_outcome(Some(&failed), Some(&challenge()), Intent::Proceed, Exit::Code(1), true)), Some(Outcome::SiteRejected));
        let login = Interaction { reason: "login".into(), url: "http://site/login".into() };
        assert_eq!(outcome_of(grab_outcome(Some(&failed), Some(&login), Intent::Proceed, Exit::Code(1), true)), Some(Outcome::LoginRequired));
        let waf = finished("failed", "page is behind a bot-protection challenge");
        assert_eq!(outcome_of(grab_outcome(Some(&waf), None, Intent::Proceed, Exit::Code(1), true)), Some(Outcome::SiteRejected));
        assert_eq!(outcome_of(grab_outcome(Some(&failed), None, Intent::Proceed, Exit::Code(1), true)), Some(Outcome::JobError));
        assert_eq!(outcome_of(grab_outcome(None, None, Intent::Proceed, Exit::Signal(9), true)), Some(Outcome::JobError));
    }

    #[test]
    fn grab_events_follow_the_published_schema_and_unknown_types_are_ignored() {
        assert_eq!(parse_event(r#"{"type":"started","version":1,"urls":["http://a/"],"out":"/o"}"#), Ok(GrabEvent::Started));
        assert_eq!(
            parse_event(r#"{"type":"progress","pages_done":2,"pages_queued":3,"assets_done":4}"#),
            Ok(GrabEvent::Progress(Progress { pages_done: 2, pages_queued: 3, assets_done: 4 }))
        );
        assert_eq!(parse_event(r#"{"type":"needs_interaction","reason":"challenge","url":"http://site/"}"#), Ok(GrabEvent::NeedsInteraction(challenge())));
        assert_eq!(parse_event(r#"{"type":"finished","outcome":"succeeded","detail":"crawl completed"}"#), Ok(GrabEvent::Finished(finished("succeeded", "crawl completed"))));
        assert_eq!(parse_event(r#"{"type":"heartbeat"}"#), Ok(GrabEvent::Other));
        assert!(parse_event(r#"{"type":"progress","pages_done":2}"#).is_err());
        assert!(parse_event("not json").is_err());
    }

    #[test]
    fn a_run_log_keeps_its_cap_across_attempts() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../.work/tmp/bounded-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log");
        let chunk = vec![b'x'; usize::try_from(LOG_CAP_BYTES / 2).unwrap() + 1];
        lock(&BoundedLog::open(&path).unwrap()).write(&chunk);
        let second = BoundedLog::open(&path).unwrap();
        lock(&second).write(&chunk);
        lock(&second).write(b"more");
        let text = read_log(&path).unwrap();
        assert!(text.len() as u64 <= LOG_CAP_BYTES + 100, "{} bytes", text.len());
        assert_eq!(text.matches("later output is discarded").count(), 1);
        assert_eq!(lock(&second).discarded, 6);
        fs::remove_dir_all(&dir).unwrap();
    }
}
