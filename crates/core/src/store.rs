use std::fmt;
use std::ops::Range;
use std::path::Path;

use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior, params};

use crate::names::{Generation, MachineName, ProfileName, SessionName};
use crate::reconcile::Action;
use crate::state::{
    AllowedRange, AttemptEnd, AttemptRecord, AttemptState, Endpoints, Interaction, LiveState, NewRun, Outcome, Progress, Resources, RunKind, RunRecord, SessionRecord,
    SessionState,
};

const SCHEMA_VERSION: i64 = 4;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS profiles (
    name TEXT PRIMARY KEY
) STRICT;
CREATE TABLE IF NOT EXISTS sessions (
    name TEXT PRIMARY KEY,
    profile TEXT NOT NULL REFERENCES profiles(name),
    image TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('stopped', 'starting', 'ready', 'stopping', 'paused', 'failed')),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    vnc_port INTEGER CHECK (vnc_port BETWEEN 1 AND 65535),
    failure TEXT,
    viewport_width INTEGER NOT NULL,
    viewport_height INTEGER NOT NULL,
    cpus INTEGER NOT NULL,
    memory_mib INTEGER NOT NULL,
    allow_private TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS one_writer_per_profile
    ON sessions(profile) WHERE state IN ('starting', 'ready', 'stopping', 'paused');
CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY,
    session TEXT NOT NULL REFERENCES sessions(name),
    job TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('exec', 'grab')),
    profile TEXT NOT NULL,
    image TEXT NOT NULL,
    image_digest TEXT NOT NULL,
    browser_version TEXT NOT NULL,
    config TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    created_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS attempts (
    id INTEGER PRIMARY KEY,
    run INTEGER NOT NULL REFERENCES runs(id),
    number INTEGER NOT NULL CHECK (number >= 1),
    state TEXT NOT NULL CHECK (state IN ('starting', 'running', 'needs-interaction', 'paused', 'succeeded', 'failed', 'cancelled', 'interrupted')),
    outcome TEXT,
    detail TEXT,
    pid INTEGER,
    started_ms INTEGER NOT NULL,
    ended_ms INTEGER,
    pages_done INTEGER,
    pages_queued INTEGER,
    assets_done INTEGER,
    interaction_reason TEXT,
    interaction_url TEXT,
    UNIQUE (run, number)
) STRICT;
CREATE TABLE IF NOT EXISTS selected_targets (
    session TEXT PRIMARY KEY REFERENCES sessions(name),
    generation INTEGER NOT NULL CHECK (generation >= 1),
    target TEXT NOT NULL CHECK (target <> ''),
    url TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS element_refs (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next INTEGER NOT NULL CHECK (next >= 1)
) STRICT;
INSERT OR IGNORE INTO element_refs (id, next) VALUES (1, 1);
";

const LIVE_STATES: &str = "('starting', 'running', 'needs-interaction')";

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    NoSuchSession(SessionName),
    SessionExists(SessionName),
    ProfileBusy { profile: ProfileName, holder: SessionName },
    NotStartable { session: SessionName, state: SessionState },
    StaleGeneration { session: SessionName, expected: Generation },
    NoSuchRun(i64),
    NotResumable { run: i64, state: AttemptState },
    StaleAttempt(i64),
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "state database: {e}"),
            StoreError::NoSuchSession(s) => write!(f, "no session named {s}"),
            StoreError::SessionExists(s) => write!(f, "a session named {s} already exists"),
            StoreError::ProfileBusy { profile, holder } => write!(f, "profile {profile} is in use by session {holder}"),
            StoreError::NotStartable { session, state } => write!(f, "session {session} is {}, not stopped or failed", state.as_str()),
            StoreError::StaleGeneration { session, expected } => write!(f, "session {session} is no longer at generation {expected}"),
            StoreError::NoSuchRun(run) => write!(f, "no run {run}"),
            StoreError::NotResumable { run, state } => write!(f, "run {run} is {}, not paused or interrupted", state.as_str()),
            StoreError::StaleAttempt(attempt) => write!(f, "attempt {attempt} is no longer live"),
            StoreError::Corrupt(what) => write!(f, "state database holds an invalid value: {what}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> StoreError {
        StoreError::Sqlite(e)
    }
}

pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        Store::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store, StoreError> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Store, StoreError> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "foreign_keys", true)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&db)?;
        Ok(Store { db })
    }

    pub fn create_session(
        &mut self,
        name: &SessionName,
        profile: &ProfileName,
        image: &str,
        resources: Resources,
        allow_private: &[AllowedRange],
    ) -> Result<SessionRecord, StoreError> {
        let mut allow: Vec<&str> = allow_private.iter().map(AllowedRange::as_str).collect();
        allow.sort_unstable();
        allow.dedup();
        let tx = self.db.transaction()?;
        tx.execute("INSERT OR IGNORE INTO profiles (name) VALUES (?1)", [profile.as_str()])?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO sessions (name, profile, image, state, generation, viewport_width, viewport_height, cpus, memory_mib, allow_private)
             VALUES (?1, ?2, ?3, 'stopped', 0, ?4, ?5, ?6, ?7, ?8)",
            params![
                name.as_str(),
                profile.as_str(),
                image,
                resources.viewport[0],
                resources.viewport[1],
                resources.cpus,
                resources.memory_mib,
                allow.join(" ")
            ],
        )?;
        if inserted == 0 {
            return Err(StoreError::SessionExists(name.clone()));
        }
        tx.commit()?;
        self.session(name)
    }

    pub fn session(&self, name: &SessionName) -> Result<SessionRecord, StoreError> {
        self.db
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE name = ?1"),
                [name.as_str()],
                raw_session,
            )
            .optional()?
            .ok_or_else(|| StoreError::NoSuchSession(name.clone()))
            .and_then(session_record)
    }

    pub fn sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let mut stmt = self.db.prepare(&format!("SELECT {SESSION_COLUMNS} FROM sessions ORDER BY name"))?;
        let raw = stmt.query_map([], raw_session)?.collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(session_record).collect()
    }

    /// Records the intent to start before any VM exists: a new generation in `starting`, holding the profile.
    pub fn begin_start(&mut self, name: &SessionName, endpoints: Endpoints) -> Result<MachineName, StoreError> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = tx
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE name = ?1"),
                [name.as_str()],
                raw_session,
            )
            .optional()?
            .ok_or_else(|| StoreError::NoSuchSession(name.clone()))
            .and_then(session_record)?;
        if !matches!(session.state, SessionState::Stopped | SessionState::Failed) {
            return Err(StoreError::NotStartable { session: name.clone(), state: session.state });
        }
        let generation = session.generation.next();
        tx.execute("DELETE FROM selected_targets WHERE session = ?1", [name.as_str()])?;
        let updated = tx.execute(
            "UPDATE sessions SET state = 'starting', generation = ?2, vnc_port = ?3, failure = NULL WHERE name = ?1",
            params![name.as_str(), to_sql_generation(generation)?, endpoints.vnc_port],
        );
        match updated {
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
                let holder: String = tx.query_row(
                    "SELECT name FROM sessions WHERE profile = ?1 AND state IN ('starting', 'ready', 'stopping', 'paused')",
                    [session.profile.as_str()],
                    |row| row.get(0),
                )?;
                return Err(StoreError::ProfileBusy { profile: session.profile, holder: parse_session_name(&holder)? });
            }
            other => other?,
        };
        tx.commit()?;
        Ok(MachineName { session: name.clone(), generation })
    }

    /// Moves a session to `state` only if it is still at `generation`, so a late report about an old VM changes nothing.
    pub fn transition(
        &mut self,
        name: &SessionName,
        generation: Generation,
        state: SessionState,
        failure: Option<&str>,
    ) -> Result<(), StoreError> {
        let changed = self.db.execute(
            "UPDATE sessions SET state = ?3, failure = ?4 WHERE name = ?1 AND generation = ?2",
            params![name.as_str(), to_sql_generation(generation)?, state.as_str(), failure],
        )?;
        if changed == 0 {
            self.session(name)?;
            return Err(StoreError::StaleGeneration { session: name.clone(), expected: generation });
        }
        Ok(())
    }

    /// Forgets a session that holds no machine. Its profile and its runs stay.
    pub fn delete_session(&mut self, name: &SessionName) -> Result<(), StoreError> {
        let session = self.session(name)?;
        if session.state.holds_machine() {
            return Err(StoreError::NotStartable { session: name.clone(), state: session.state });
        }
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM attempts WHERE run IN (SELECT id FROM runs WHERE session = ?1)", [name.as_str()])?;
        tx.execute("DELETE FROM runs WHERE session = ?1", [name.as_str()])?;
        tx.execute("DELETE FROM selected_targets WHERE session = ?1", [name.as_str()])?;
        tx.execute("DELETE FROM sessions WHERE name = ?1", [name.as_str()])?;
        tx.commit()?;
        Ok(())
    }

    /// Every profile with the session currently holding it, if any.
    pub fn profiles(&self) -> Result<Vec<(ProfileName, Option<SessionName>)>, StoreError> {
        let mut stmt = self.db.prepare(
            "SELECT p.name, s.name FROM profiles p LEFT JOIN sessions s ON s.profile = p.name AND s.state IN ('starting', 'ready', 'stopping', 'paused') ORDER BY p.name",
        )?;
        let raw = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|(profile, holder)| {
                let profile = ProfileName::new(&profile).map_err(|e| StoreError::Corrupt(e.to_string()))?;
                Ok((profile, holder.as_deref().map(parse_session_name).transpose()?))
            })
            .collect()
    }

    /// Records a run and its first attempt, `starting`, before any process exists.
    pub fn create_run(&mut self, run: &NewRun) -> Result<(i64, i64), StoreError> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO runs (session, job, kind, profile, image, image_digest, browser_version, config, input_digest, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                run.session.as_str(),
                run.job,
                run.kind.as_str(),
                run.profile.as_str(),
                run.image,
                run.image_digest,
                run.browser_version,
                run.config,
                run.input_digest,
                run.created_ms
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO attempts (run, number, state, started_ms) VALUES (?1, 1, 'starting', ?2)", params![id, run.created_ms])?;
        let attempt = tx.last_insert_rowid();
        tx.commit()?;
        Ok((id, attempt))
    }

    /// Starts the next attempt of a run whose current attempt is paused or interrupted.
    pub fn begin_attempt(&mut self, run: i64, now_ms: i64) -> Result<i64, StoreError> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (number, state): (u32, String) = tx
            .query_row("SELECT number, state FROM attempts WHERE run = ?1 ORDER BY number DESC LIMIT 1", [run], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?
            .ok_or(StoreError::NoSuchRun(run))?;
        let state = parse_attempt_state(&state)?;
        if !matches!(state, AttemptState::Paused | AttemptState::Interrupted) {
            return Err(StoreError::NotResumable { run, state });
        }
        tx.execute("INSERT INTO attempts (run, number, state, started_ms) VALUES (?1, ?2, 'starting', ?3)", params![run, number + 1, now_ms])?;
        let attempt = tx.last_insert_rowid();
        tx.commit()?;
        Ok(attempt)
    }

    pub fn set_pid(&mut self, attempt: i64, pid: u32) -> Result<(), StoreError> {
        self.update_live(attempt, "UPDATE attempts SET pid = ?2 WHERE id = ?1", params![attempt, pid])
    }

    /// Moves a live attempt between live states; `interaction` is kept only while it needs one.
    pub fn set_live(&mut self, attempt: i64, state: LiveState, interaction: Option<&Interaction>) -> Result<(), StoreError> {
        let interaction = interaction.filter(|_| state == LiveState::NeedsInteraction);
        self.update_live(
            attempt,
            "UPDATE attempts SET state = ?2, interaction_reason = ?3, interaction_url = ?4 WHERE id = ?1",
            params![attempt, AttemptState::from(state).as_str(), interaction.map(|i| &i.reason), interaction.map(|i| &i.url)],
        )
    }

    pub fn set_progress(&mut self, attempt: i64, progress: Progress) -> Result<(), StoreError> {
        self.update_live(
            attempt,
            "UPDATE attempts SET pages_done = ?2, pages_queued = ?3, assets_done = ?4 WHERE id = ?1",
            params![attempt, progress.pages_done, progress.pages_queued, progress.assets_done],
        )
    }

    /// Ends a live attempt. An attempt that is no longer live is left as it is and reported stale.
    pub fn finish_attempt(&mut self, attempt: i64, end: &AttemptEnd, now_ms: i64) -> Result<(), StoreError> {
        let (state, outcome, detail) = match end {
            AttemptEnd::Paused { detail } => (AttemptState::Paused, None, Some(detail.as_str())),
            AttemptEnd::Ended { outcome, detail } => (outcome.attempt_state(), Some(outcome.as_str()), detail.as_deref()),
        };
        self.update_live(
            attempt,
            "UPDATE attempts SET state = ?2, outcome = ?3, detail = ?4, ended_ms = ?5, interaction_reason = NULL, interaction_url = NULL WHERE id = ?1",
            params![attempt, state.as_str(), outcome, detail, now_ms],
        )
    }

    /// Ends a paused attempt for good, so its run can no longer be resumed.
    pub fn cancel_paused(&mut self, attempt: i64, now_ms: i64) -> Result<(), StoreError> {
        let changed = self.db.execute(
            "UPDATE attempts SET state = 'cancelled', outcome = 'cancelled', ended_ms = ?2 WHERE id = ?1 AND state = 'paused'",
            params![attempt, now_ms],
        )?;
        if changed == 0 {
            return Err(StoreError::StaleAttempt(attempt));
        }
        Ok(())
    }

    fn update_live(&mut self, attempt: i64, sql: &str, params: impl rusqlite::Params) -> Result<(), StoreError> {
        let guarded = format!("{sql} AND state IN {LIVE_STATES}");
        if self.db.execute(&guarded, params)? == 0 {
            return Err(StoreError::StaleAttempt(attempt));
        }
        Ok(())
    }

    pub fn run(&self, id: i64) -> Result<RunRecord, StoreError> {
        let spec = self
            .db
            .query_row(&format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ?1"), [id], raw_run)
            .optional()?
            .ok_or(StoreError::NoSuchRun(id))?;
        self.assemble(spec)
    }

    /// Every run, newest first, optionally only those of one session.
    pub fn runs(&self, session: Option<&SessionName>) -> Result<Vec<RunRecord>, StoreError> {
        let mut stmt = self.db.prepare(&format!("SELECT {RUN_COLUMNS} FROM runs WHERE ?1 IS NULL OR session = ?1 ORDER BY id DESC"))?;
        let raw = stmt.query_map([session.map(SessionName::as_str)], raw_run)?.collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(|spec| self.assemble(spec)).collect()
    }

    fn assemble(&self, (id, spec): (i64, RawRun)) -> Result<RunRecord, StoreError> {
        let mut stmt = self.db.prepare(&format!("SELECT {ATTEMPT_COLUMNS} FROM attempts WHERE run = ?1 ORDER BY number"))?;
        let mut attempts = stmt.query_map([id], raw_attempt)?.collect::<Result<Vec<_>, _>>()?.into_iter().map(attempt_record).collect::<Result<Vec<_>, _>>()?;
        let current = attempts.pop().ok_or_else(|| StoreError::Corrupt(format!("run {id} has no attempts")))?;
        Ok(RunRecord { id, spec: run_spec(spec)?, previous: attempts, current })
    }

    pub fn attempts(&self) -> Result<Vec<AttemptRecord>, StoreError> {
        let mut stmt = self.db.prepare(&format!("SELECT {ATTEMPT_COLUMNS} FROM attempts ORDER BY id"))?;
        let raw = stmt.query_map([], raw_attempt)?.collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(attempt_record).collect()
    }

    /// The target selected for native commands in `generation`, if that is still the session's generation.
    pub fn selected_target(&self, name: &SessionName, generation: Generation) -> Result<Option<SelectedTarget>, StoreError> {
        Ok(self
            .db
            .query_row(
                "SELECT t.target, t.url FROM selected_targets t JOIN sessions s ON s.name = t.session AND s.generation = t.generation
                 WHERE t.session = ?1 AND t.generation = ?2",
                params![name.as_str(), to_sql_generation(generation)?],
                |row| Ok(SelectedTarget { target: row.get(0)?, url: row.get(1)? }),
            )
            .optional()?)
    }

    /// Selects `target` for `generation`, replacing any earlier selection. `url` is stored as given, so it must
    /// already be stripped of userinfo, query, and fragment. A session that has moved past `generation` is reported stale.
    pub fn select_target(&mut self, name: &SessionName, generation: Generation, target: &SelectedTarget) -> Result<(), StoreError> {
        let changed = self.db.execute(
            "INSERT INTO selected_targets (session, generation, target, url)
             SELECT name, generation, ?3, ?4 FROM sessions WHERE name = ?1 AND generation = ?2
             ON CONFLICT (session) DO UPDATE SET generation = excluded.generation, target = excluded.target, url = excluded.url",
            params![name.as_str(), to_sql_generation(generation)?, target.target, target.url],
        )?;
        if changed == 0 {
            self.session(name)?;
            return Err(StoreError::StaleGeneration { session: name.clone(), expected: generation });
        }
        Ok(())
    }

    /// Clears the selection of `generation` only if it is still `target`, so a concurrent selection survives.
    pub fn release_target(&mut self, name: &SessionName, generation: Generation, target: &str) -> Result<(), StoreError> {
        self.db.execute(
            "DELETE FROM selected_targets WHERE session = ?1 AND generation = ?2 AND target = ?3",
            params![name.as_str(), to_sql_generation(generation)?, target],
        )?;
        Ok(())
    }

    /// Hands out `count` element ref numbers that no earlier call on this database returned, so a restarted daemon never reissues one.
    pub fn reserve_element_refs(&mut self, count: u64) -> Result<Range<u64>, StoreError> {
        let count = i64::try_from(count).map_err(|_| StoreError::Corrupt(format!("cannot reserve {count} element refs")))?;
        let next: i64 = self.db.query_row("UPDATE element_refs SET next = next + ?1 WHERE id = 1 RETURNING next", [count], |row| row.get(0))?;
        let end = u64::try_from(next).map_err(|_| StoreError::Corrupt(format!("element ref counter {next}")))?;
        Ok(end - count as u64..end)
    }

    /// Applies the record-keeping half of a reconciliation plan. Stopping stale machines is the caller's effect.
    pub fn apply(&mut self, actions: &[Action], now_ms: i64) -> Result<(), StoreError> {
        let tx = self.db.transaction()?;
        for action in actions {
            match action {
                Action::Settle { session, state, reason } => {
                    tx.execute(
                        "UPDATE sessions SET state = ?2, failure = ?3 WHERE name = ?1",
                        params![session.as_str(), state.as_str(), reason],
                    )?;
                }
                Action::Interrupt { attempt } => {
                    tx.execute(
                        &format!(
                            "UPDATE attempts SET state = 'interrupted', outcome = 'interrupted', detail = ?2, ended_ms = ?3,
                             interaction_reason = NULL, interaction_url = NULL WHERE id = ?1 AND state IN {LIVE_STATES}"
                        ),
                        params![attempt, INTERRUPTED_DETAIL, now_ms],
                    )?;
                }
                Action::Adopt { .. } | Action::StopStale { .. } => {}
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// A page target chosen for native commands, by CDP `targetId`, with its URL when it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedTarget {
    pub target: String,
    pub url: String,
}

const INTERRUPTED_DETAIL: &str = "bctl stopped while this attempt was live; its effects are unknown and it was not retried";

const SESSION_COLUMNS: &str = "name, profile, image, state, generation, failure, vnc_port, viewport_width, viewport_height, cpus, memory_mib, allow_private";

struct RawSession {
    name: String,
    profile: String,
    image: String,
    state: String,
    generation: i64,
    failure: Option<String>,
    vnc_port: Option<u16>,
    resources: Resources,
    allow_private: String,
}

fn raw_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawSession> {
    Ok(RawSession {
        name: row.get(0)?,
        profile: row.get(1)?,
        image: row.get(2)?,
        state: row.get(3)?,
        generation: row.get(4)?,
        failure: row.get(5)?,
        vnc_port: row.get(6)?,
        resources: Resources { viewport: [row.get(7)?, row.get(8)?], cpus: row.get(9)?, memory_mib: row.get(10)? },
        allow_private: row.get(11)?,
    })
}

fn session_record(raw: RawSession) -> Result<SessionRecord, StoreError> {
    let RawSession { name, profile, image, state, generation, failure, vnc_port, resources, allow_private } = raw;
    Ok(SessionRecord {
        name: parse_session_name(&name)?,
        profile: parse_profile_name(&profile)?,
        image,
        resources,
        allow_private: allow_private.split_whitespace().map(|range| AllowedRange::parse(range).map_err(StoreError::Corrupt)).collect::<Result<_, _>>()?,
        state: SessionState::parse(&state).ok_or_else(|| StoreError::Corrupt(format!("session state {state:?}")))?,
        generation: Generation(u64::try_from(generation).map_err(|_| StoreError::Corrupt(format!("generation {generation}")))?),
        endpoints: vnc_port.map(|vnc_port| Endpoints { vnc_port }),
        failure,
    })
}

const RUN_COLUMNS: &str = "id, session, job, kind, profile, image, image_digest, browser_version, config, input_digest, created_ms";

struct RawRun {
    session: String,
    job: Option<String>,
    kind: String,
    profile: String,
    image: String,
    image_digest: String,
    browser_version: String,
    config: String,
    input_digest: String,
    created_ms: i64,
}

fn raw_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, RawRun)> {
    Ok((
        row.get(0)?,
        RawRun {
            session: row.get(1)?,
            job: row.get(2)?,
            kind: row.get(3)?,
            profile: row.get(4)?,
            image: row.get(5)?,
            image_digest: row.get(6)?,
            browser_version: row.get(7)?,
            config: row.get(8)?,
            input_digest: row.get(9)?,
            created_ms: row.get(10)?,
        },
    ))
}

fn run_spec(raw: RawRun) -> Result<NewRun, StoreError> {
    Ok(NewRun {
        session: parse_session_name(&raw.session)?,
        job: raw.job,
        kind: RunKind::parse(&raw.kind).ok_or_else(|| StoreError::Corrupt(format!("run kind {:?}", raw.kind)))?,
        profile: parse_profile_name(&raw.profile)?,
        image: raw.image,
        image_digest: raw.image_digest,
        browser_version: raw.browser_version,
        config: raw.config,
        input_digest: raw.input_digest,
        created_ms: raw.created_ms,
    })
}

const ATTEMPT_COLUMNS: &str =
    "id, run, number, state, outcome, detail, pid, started_ms, ended_ms, pages_done, pages_queued, assets_done, interaction_reason, interaction_url";

struct RawAttempt {
    id: i64,
    run: i64,
    number: u32,
    state: String,
    outcome: Option<String>,
    detail: Option<String>,
    pid: Option<u32>,
    started_ms: i64,
    ended_ms: Option<i64>,
    progress: (Option<u32>, Option<u32>, Option<u32>),
    interaction: (Option<String>, Option<String>),
}

fn raw_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawAttempt> {
    Ok(RawAttempt {
        id: row.get(0)?,
        run: row.get(1)?,
        number: row.get(2)?,
        state: row.get(3)?,
        outcome: row.get(4)?,
        detail: row.get(5)?,
        pid: row.get(6)?,
        started_ms: row.get(7)?,
        ended_ms: row.get(8)?,
        progress: (row.get(9)?, row.get(10)?, row.get(11)?),
        interaction: (row.get(12)?, row.get(13)?),
    })
}

fn attempt_record(raw: RawAttempt) -> Result<AttemptRecord, StoreError> {
    let corrupt = |what: &str| StoreError::Corrupt(format!("attempt {}: {what}", raw.id));
    let state = parse_attempt_state(&raw.state)?;
    let outcome = raw.outcome.as_deref().map(|o| Outcome::parse(o).ok_or_else(|| corrupt(&format!("outcome {o:?}")))).transpose()?;
    if outcome.map(Outcome::attempt_state) != (!state.is_live() && state != AttemptState::Paused).then_some(state) {
        return Err(corrupt(&format!("state {} does not match outcome {outcome:?}", state.as_str())));
    }
    let progress = match raw.progress {
        (Some(pages_done), Some(pages_queued), Some(assets_done)) => Some(Progress { pages_done, pages_queued, assets_done }),
        (None, None, None) => None,
        _ => return Err(corrupt("partial progress")),
    };
    let interaction = match raw.interaction {
        (Some(reason), Some(url)) => Some(Interaction { reason, url }),
        (None, None) => None,
        _ => return Err(corrupt("partial interaction")),
    };
    Ok(AttemptRecord {
        id: raw.id,
        run: raw.run,
        number: raw.number,
        state,
        outcome,
        detail: raw.detail,
        pid: raw.pid,
        started_ms: raw.started_ms,
        ended_ms: raw.ended_ms,
        progress,
        interaction,
    })
}

fn parse_attempt_state(state: &str) -> Result<AttemptState, StoreError> {
    AttemptState::parse(state).ok_or_else(|| StoreError::Corrupt(format!("attempt state {state:?}")))
}

fn parse_profile_name(name: &str) -> Result<ProfileName, StoreError> {
    ProfileName::new(name).map_err(|e| StoreError::Corrupt(e.to_string()))
}

/// Brings a database from any earlier bctl up to `SCHEMA_VERSION`. Before version 1 no command created runs, so
/// a pre-1 `runs` table can only be empty; one that is not is reported rather than dropped.
fn migrate(db: &Connection) -> Result<(), StoreError> {
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::Corrupt(format!("schema version {version} is newer than this bctl understands ({SCHEMA_VERSION})")));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let has_column = |table: &str, column: &str| -> rusqlite::Result<bool> {
        db.query_row("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2", [table, column], |row| row.get::<_, i64>(0)).map(|n| n > 0)
    };
    let exists = |table: &str| -> rusqlite::Result<bool> {
        db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [table], |row| row.get::<_, i64>(0)).map(|n| n > 0)
    };
    let rebuild_sessions = version < 4 && exists("sessions")?;
    if rebuild_sessions {
        db.pragma_update(None, "foreign_keys", false)?;
    }
    db.execute_batch("BEGIN IMMEDIATE")?;
    let migrated = (|| -> Result<(), StoreError> {
        if exists("runs")? && !has_column("runs", "kind")? {
            let rows: i64 = db.query_row("SELECT (SELECT COUNT(*) FROM runs) + (SELECT COUNT(*) FROM attempts)", [], |row| row.get(0))?;
            if rows > 0 {
                return Err(StoreError::Corrupt(format!("{rows} run rows from a schema before version 1 cannot be migrated")));
            }
            db.execute_batch("DROP TABLE attempts; DROP TABLE runs;")?;
        }
        if exists("sessions")? && !has_column("sessions", "cpus")? {
            let Resources { viewport: [width, height], cpus, memory_mib } = Resources::DEFAULT;
            db.execute_batch(&format!(
                "ALTER TABLE sessions ADD COLUMN viewport_width INTEGER NOT NULL DEFAULT {width};
                 ALTER TABLE sessions ADD COLUMN viewport_height INTEGER NOT NULL DEFAULT {height};
                 ALTER TABLE sessions ADD COLUMN cpus INTEGER NOT NULL DEFAULT {cpus};
                 ALTER TABLE sessions ADD COLUMN memory_mib INTEGER NOT NULL DEFAULT {memory_mib};
                 ALTER TABLE sessions ADD COLUMN allow_private TEXT NOT NULL DEFAULT '';"
            ))?;
        }
        if rebuild_sessions {
            db.execute_batch(
                "DROP INDEX IF EXISTS one_writer_per_profile;
                 CREATE TABLE sessions_v4 (
                     name TEXT PRIMARY KEY,
                     profile TEXT NOT NULL REFERENCES profiles(name),
                     image TEXT NOT NULL,
                     state TEXT NOT NULL CHECK (state IN ('stopped', 'starting', 'ready', 'stopping', 'paused', 'failed')),
                     generation INTEGER NOT NULL CHECK (generation >= 0),
                     vnc_port INTEGER CHECK (vnc_port BETWEEN 1 AND 65535),
                     failure TEXT,
                     viewport_width INTEGER NOT NULL,
                     viewport_height INTEGER NOT NULL,
                     cpus INTEGER NOT NULL,
                     memory_mib INTEGER NOT NULL,
                     allow_private TEXT NOT NULL
                 ) STRICT;
                 INSERT INTO sessions_v4 (name, profile, image, state, generation, vnc_port, failure, viewport_width, viewport_height, cpus, memory_mib, allow_private)
                     SELECT name, profile, image, state, generation, vnc_port, failure, viewport_width, viewport_height, cpus, memory_mib, allow_private FROM sessions;
                 DROP TABLE sessions;
                 ALTER TABLE sessions_v4 RENAME TO sessions;",
            )?;
        }
        db.execute_batch(SCHEMA)?;
        db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    })();
    match migrated {
        Ok(()) => {
            db.execute_batch("COMMIT")?;
            if rebuild_sessions {
                db.pragma_update(None, "foreign_keys", true)?;
            }
        }
        Err(_) => {
            db.execute_batch("ROLLBACK")?;
            if rebuild_sessions {
                db.pragma_update(None, "foreign_keys", true)?;
            }
        }
    }
    migrated
}

fn parse_session_name(name: &str) -> Result<SessionName, StoreError> {
    SessionName::new(name).map_err(|e| StoreError::Corrupt(e.to_string()))
}

fn to_sql_generation(generation: Generation) -> Result<i64, StoreError> {
    i64::try_from(generation.0).map_err(|_| StoreError::Corrupt(format!("generation {generation} overflows")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::reconcile;
    use crate::state::{MachineObservation, MachineState};

    const PORTS: Endpoints = Endpoints { vnc_port: 15900 };

    fn name(s: &str) -> SessionName {
        SessionName::new(s).unwrap()
    }

    fn store_with(sessions: &[(&str, &str)]) -> Store {
        let mut store = Store::open_in_memory().unwrap();
        for (session, profile) in sessions {
            store.create_session(&name(session), &ProfileName::new(profile).unwrap(), "ref-alpine-chromium", Resources::DEFAULT, &[]).unwrap();
        }
        store
    }

    fn new_run(session: &str) -> NewRun {
        NewRun {
            session: name(session),
            job: Some("mirror-docs".into()),
            kind: RunKind::Grab,
            profile: ProfileName::new("personal").unwrap(),
            image: "ref-alpine-chromium".into(),
            image_digest: "sha256:0".into(),
            browser_version: "Chrome/142".into(),
            config: "{}".into(),
            input_digest: "sha256:1".into(),
            created_ms: 10,
        }
    }

    #[test]
    fn a_profile_has_at_most_one_writable_session() {
        let mut store = store_with(&[("a", "personal"), ("b", "personal")]);
        store.begin_start(&name("a"), PORTS).unwrap();
        match store.begin_start(&name("b"), PORTS) {
            Err(StoreError::ProfileBusy { profile, holder }) => {
                assert_eq!((profile.as_str(), holder.as_str()), ("personal", "a"));
            }
            other => panic!("expected ProfileBusy, got {other:?}"),
        }
        assert_eq!(store.session(&name("b")).unwrap().state, SessionState::Stopped);

        store.transition(&name("a"), Generation(1), SessionState::Stopped, None).unwrap();
        assert_eq!(store.begin_start(&name("b"), PORTS).unwrap().to_string(), "bctl-b-1");
    }

    #[test]
    fn each_start_records_a_new_generation_before_any_machine_exists() {
        let mut store = store_with(&[("research", "personal")]);
        assert_eq!(store.begin_start(&name("research"), PORTS).unwrap().to_string(), "bctl-research-1");
        let session = store.session(&name("research")).unwrap();
        assert_eq!((session.state, session.generation), (SessionState::Starting, Generation(1)));
        assert!(matches!(store.begin_start(&name("research"), PORTS), Err(StoreError::NotStartable { .. })));
        store.transition(&name("research"), Generation(1), SessionState::Failed, Some("boom")).unwrap();
        assert_eq!(store.begin_start(&name("research"), PORTS).unwrap().to_string(), "bctl-research-2");
    }

    #[test]
    fn a_report_about_an_older_generation_changes_nothing() {
        let mut store = store_with(&[("research", "personal")]);
        store.begin_start(&name("research"), PORTS).unwrap();
        store.transition(&name("research"), Generation(1), SessionState::Failed, None).unwrap();
        store.begin_start(&name("research"), PORTS).unwrap();
        assert!(matches!(
            store.transition(&name("research"), Generation(1), SessionState::Ready, None),
            Err(StoreError::StaleGeneration { .. })
        ));
        assert_eq!(store.session(&name("research")).unwrap().state, SessionState::Starting);
    }

    #[test]
    fn restart_reconciliation_interrupts_live_attempts_and_never_requeues_them() {
        let mut store = store_with(&[("research", "personal")]);
        store.begin_start(&name("research"), PORTS).unwrap();
        store.transition(&name("research"), Generation(1), SessionState::Ready, None).unwrap();
        let (run, attempt) = store.create_run(&new_run("research")).unwrap();
        store.set_live(attempt, LiveState::Running, None).unwrap();

        let machines = [MachineObservation { name: "bctl-research-1".into(), state: MachineState::Running }];
        let plan = reconcile(&store.sessions().unwrap(), &store.attempts().unwrap(), &machines);
        store.apply(&plan, 20).unwrap();

        assert_eq!(store.session(&name("research")).unwrap().state, SessionState::Ready);
        let current = store.run(run).unwrap().current;
        assert_eq!((current.state, current.outcome, current.ended_ms), (AttemptState::Interrupted, Some(Outcome::Interrupted), Some(20)));
        let again = reconcile(&store.sessions().unwrap(), &store.attempts().unwrap(), &machines);
        assert!(again.iter().all(|a| !matches!(a, Action::Interrupt { .. })), "{again:?}");
    }

    #[test]
    fn a_vanished_machine_fails_its_session_and_frees_the_profile() {
        let mut store = store_with(&[("a", "personal"), ("b", "personal")]);
        store.begin_start(&name("a"), PORTS).unwrap();
        let plan = reconcile(&store.sessions().unwrap(), &[], &[]);
        store.apply(&plan, 0).unwrap();
        let a = store.session(&name("a")).unwrap();
        assert_eq!(a.state, SessionState::Failed);
        assert!(a.failure.unwrap().contains("vanished"));
        store.begin_start(&name("b"), PORTS).unwrap();
    }

    #[test]
    fn state_survives_reopening_the_database() {
        let dir = std::env::temp_dir().join(format!("bctl-core-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        {
            let mut store = Store::open(&path).unwrap();
            store.create_session(&name("research"), &ProfileName::new("personal").unwrap(), "img", Resources::DEFAULT, &[]).unwrap();
            store.begin_start(&name("research"), PORTS).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let session = store.session(&name("research")).unwrap();
        assert_eq!((session.generation, session.endpoints), (Generation(1), Some(PORTS)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_a_paused_or_interrupted_run_gets_another_attempt() {
        let mut store = store_with(&[("research", "personal")]);
        let (run, first) = store.create_run(&new_run("research")).unwrap();
        assert!(matches!(store.begin_attempt(run, 11), Err(StoreError::NotResumable { state: AttemptState::Starting, .. })));

        store.finish_attempt(first, &AttemptEnd::Paused { detail: "paused at a page boundary".into() }, 12).unwrap();
        let second = store.begin_attempt(run, 13).unwrap();
        store.set_live(second, LiveState::NeedsInteraction, Some(&Interaction { reason: "challenge".into(), url: "http://a/".into() })).unwrap();
        let record = store.run(run).unwrap();
        assert_eq!((record.previous.len(), record.current.number, record.current.state), (1, 2, AttemptState::NeedsInteraction));
        assert_eq!(record.current.interaction.as_ref().map(|i| i.url.as_str()), Some("http://a/"));
        assert_eq!((record.previous[0].state, record.previous[0].outcome), (AttemptState::Paused, None));

        store.finish_attempt(second, &AttemptEnd::Ended { outcome: Outcome::LoginRequired, detail: None }, 14).unwrap();
        let current = store.run(run).unwrap().current;
        assert_eq!((current.state, current.outcome, current.interaction), (AttemptState::Failed, Some(Outcome::LoginRequired), None));
        assert!(matches!(store.begin_attempt(run, 15), Err(StoreError::NotResumable { state: AttemptState::Failed, .. })));
    }

    #[test]
    fn a_report_about_an_ended_attempt_changes_nothing() {
        let mut store = store_with(&[("research", "personal")]);
        let (run, attempt) = store.create_run(&new_run("research")).unwrap();
        store.finish_attempt(attempt, &AttemptEnd::Ended { outcome: Outcome::Cancelled, detail: None }, 12).unwrap();
        let progress = Progress { pages_done: 1, pages_queued: 0, assets_done: 0 };
        assert!(matches!(store.set_progress(attempt, progress), Err(StoreError::StaleAttempt(_))));
        assert!(matches!(store.set_live(attempt, LiveState::Running, None), Err(StoreError::StaleAttempt(_))));
        let late = AttemptEnd::Ended { outcome: Outcome::Succeeded, detail: None };
        assert!(matches!(store.finish_attempt(attempt, &late, 13), Err(StoreError::StaleAttempt(_))));
        assert_eq!(store.run(run).unwrap().current.outcome, Some(Outcome::Cancelled));
    }

    fn ready(store: &mut Store, session: &str) -> Generation {
        let machine = store.begin_start(&name(session), PORTS).unwrap();
        store.transition(&name(session), machine.generation, SessionState::Ready, None).unwrap();
        machine.generation
    }

    fn target(id: &str) -> SelectedTarget {
        SelectedTarget { target: id.into(), url: "https://example.com/".into() }
    }

    #[test]
    fn a_selected_target_belongs_to_one_generation_and_a_restart_forgets_it() {
        let mut store = store_with(&[("research", "personal")]);
        let first = ready(&mut store, "research");
        store.select_target(&name("research"), first, &target("A")).unwrap();
        assert_eq!(store.selected_target(&name("research"), first).unwrap(), Some(target("A")));

        store.transition(&name("research"), first, SessionState::Stopped, None).unwrap();
        let second = ready(&mut store, "research");
        assert_eq!(store.selected_target(&name("research"), second).unwrap(), None);
        assert_eq!(store.selected_target(&name("research"), first).unwrap(), None);
        assert!(matches!(store.select_target(&name("research"), first, &target("A")), Err(StoreError::StaleGeneration { .. })));
        assert_eq!(store.selected_target(&name("research"), second).unwrap(), None);
    }

    #[test]
    fn releasing_a_target_keeps_a_selection_that_already_moved_on() {
        let mut store = store_with(&[("research", "personal")]);
        let generation = ready(&mut store, "research");
        store.select_target(&name("research"), generation, &target("A")).unwrap();
        store.select_target(&name("research"), generation, &target("B")).unwrap();
        store.release_target(&name("research"), generation, "A").unwrap();
        assert_eq!(store.selected_target(&name("research"), generation).unwrap(), Some(target("B")));
        store.release_target(&name("research"), generation, "B").unwrap();
        assert_eq!(store.selected_target(&name("research"), generation).unwrap(), None);
    }

    #[test]
    fn a_version_1_database_keeps_its_runs_and_gains_target_selection() {
        let mut store = store_with(&[("research", "personal")]);
        let generation = ready(&mut store, "research");
        let (run, _) = store.create_run(&new_run("research")).unwrap();
        let Store { db } = store;
        db.execute_batch("DROP TABLE selected_targets; PRAGMA user_version = 1;").unwrap();
        let mut store = Store::init(db).unwrap();
        assert_eq!(store.run(run).unwrap().id, run);
        store.select_target(&name("research"), generation, &target("A")).unwrap();
        assert_eq!(store.selected_target(&name("research"), generation).unwrap(), Some(target("A")));
    }

    const PAUSED_LESS_SCHEMA: &str = "
        CREATE TABLE profiles (name TEXT PRIMARY KEY) STRICT;
        CREATE TABLE sessions (
            name TEXT PRIMARY KEY,
            profile TEXT NOT NULL REFERENCES profiles(name),
            image TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('stopped', 'starting', 'ready', 'stopping', 'failed')),
            generation INTEGER NOT NULL CHECK (generation >= 0),
            vnc_port INTEGER CHECK (vnc_port BETWEEN 1 AND 65535),
            failure TEXT,
            viewport_width INTEGER NOT NULL,
            viewport_height INTEGER NOT NULL,
            cpus INTEGER NOT NULL,
            memory_mib INTEGER NOT NULL,
            allow_private TEXT NOT NULL
        ) STRICT;
        CREATE UNIQUE INDEX one_writer_per_profile ON sessions(profile) WHERE state IN ('starting', 'ready', 'stopping');
        INSERT INTO profiles VALUES ('personal');
        INSERT INTO sessions (name, profile, image, state, generation, vnc_port, failure, viewport_width, viewport_height, cpus, memory_mib, allow_private)
            VALUES ('research', 'personal', 'img', 'ready', 1, 15900, NULL, 1280, 800, 2, 4096, '');
        PRAGMA user_version = 3;
    ";

    #[test]
    fn a_version_3_database_gains_the_paused_state_and_a_paused_session_holds_its_profile() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(PAUSED_LESS_SCHEMA).unwrap();
        let mut store = Store::init(db).unwrap();
        assert_eq!(store.session(&name("research")).unwrap().state, SessionState::Ready);
        store.transition(&name("research"), Generation(1), SessionState::Paused, None).unwrap();
        let paused = store.session(&name("research")).unwrap();
        assert_eq!(paused.state, SessionState::Paused);
        assert!(paused.state.holds_machine());

        store.create_session(&name("other"), &ProfileName::new("personal").unwrap(), "img", Resources::DEFAULT, &[]).unwrap();
        assert!(matches!(store.begin_start(&name("other"), PORTS), Err(StoreError::ProfileBusy { .. })));
    }

    #[test]
    fn element_ref_numbers_are_never_handed_out_twice_even_after_a_reopen_or_an_upgrade() {
        let mut store = store_with(&[]);
        let first = store.reserve_element_refs(256).unwrap();
        let second = store.reserve_element_refs(3).unwrap();
        assert_eq!((first, second.clone()), (1..257, 257..260));
        let Store { db } = store;
        let mut store = Store::init(db).unwrap();
        assert_eq!(store.reserve_element_refs(1).unwrap(), 260..261);

        let Store { db } = store;
        db.execute_batch("DROP TABLE element_refs; PRAGMA user_version = 2;").unwrap();
        let mut store = Store::init(db).unwrap();
        assert_eq!(store.reserve_element_refs(2).unwrap(), 1..3, "a version 2 database never handed out element refs");
    }

    const PRE_RUNS_SCHEMA: &str = "
        CREATE TABLE profiles (name TEXT PRIMARY KEY) STRICT;
        CREATE TABLE sessions (name TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(name), image TEXT NOT NULL,
            state TEXT NOT NULL, generation INTEGER NOT NULL, cdp_port INTEGER, vnc_port INTEGER, failure TEXT) STRICT;
        CREATE TABLE runs (id INTEGER PRIMARY KEY, session TEXT NOT NULL REFERENCES sessions(name), job TEXT NOT NULL) STRICT;
        CREATE TABLE attempts (id INTEGER PRIMARY KEY, run INTEGER NOT NULL REFERENCES runs(id), state TEXT NOT NULL) STRICT;
        INSERT INTO profiles VALUES ('personal');
        INSERT INTO sessions VALUES ('research', 'personal', 'img', 'stopped', 3, NULL, NULL, NULL);
    ";

    #[test]
    fn a_database_from_before_run_records_keeps_its_sessions_with_default_resources() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(PRE_RUNS_SCHEMA).unwrap();
        let mut store = Store::init(db).unwrap();
        let session = store.session(&name("research")).unwrap();
        assert_eq!((session.generation, session.resources), (Generation(3), Resources::DEFAULT));
        store.create_run(&new_run("research")).unwrap();
    }

    #[test]
    fn run_rows_from_before_run_records_are_refused_not_dropped() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(PRE_RUNS_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO runs VALUES (1, 'research', 'old'); INSERT INTO attempts VALUES (1, 1, 'running');").unwrap();
        assert!(matches!(Store::init(db), Err(StoreError::Corrupt(_))));
    }
}
