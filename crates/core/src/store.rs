use std::fmt;
use std::path::Path;

use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior, params};

use crate::names::{Generation, MachineName, ProfileName, SessionName};
use crate::reconcile::Action;
use crate::state::{AttemptRecord, AttemptState, SessionRecord, SessionState};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS profiles (
    name TEXT PRIMARY KEY
) STRICT;
CREATE TABLE IF NOT EXISTS sessions (
    name TEXT PRIMARY KEY,
    profile TEXT NOT NULL REFERENCES profiles(name),
    image TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('stopped', 'starting', 'ready', 'stopping', 'failed')),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    failure TEXT
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS one_writer_per_profile
    ON sessions(profile) WHERE state IN ('starting', 'ready', 'stopping');
CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY,
    session TEXT NOT NULL REFERENCES sessions(name),
    job TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS attempts (
    id INTEGER PRIMARY KEY,
    run INTEGER NOT NULL REFERENCES runs(id),
    state TEXT NOT NULL CHECK (state IN ('starting', 'running', 'needs-interaction', 'succeeded', 'failed', 'cancelled', 'interrupted'))
) STRICT;
";

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    NoSuchSession(SessionName),
    SessionExists(SessionName),
    ProfileBusy { profile: ProfileName, holder: SessionName },
    NotStartable { session: SessionName, state: SessionState },
    StaleGeneration { session: SessionName, expected: Generation },
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
        db.execute_batch(SCHEMA)?;
        Ok(Store { db })
    }

    pub fn create_session(&mut self, name: &SessionName, profile: &ProfileName, image: &str) -> Result<SessionRecord, StoreError> {
        let tx = self.db.transaction()?;
        tx.execute("INSERT OR IGNORE INTO profiles (name) VALUES (?1)", [profile.as_str()])?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO sessions (name, profile, image, state, generation) VALUES (?1, ?2, ?3, 'stopped', 0)",
            params![name.as_str(), profile.as_str(), image],
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
                "SELECT name, profile, image, state, generation, failure FROM sessions WHERE name = ?1",
                [name.as_str()],
                raw_session,
            )
            .optional()?
            .ok_or_else(|| StoreError::NoSuchSession(name.clone()))
            .and_then(session_record)
    }

    pub fn sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let mut stmt = self.db.prepare("SELECT name, profile, image, state, generation, failure FROM sessions ORDER BY name")?;
        let raw = stmt.query_map([], raw_session)?.collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(session_record).collect()
    }

    /// Records the intent to start before any VM exists: a new generation in `starting`, holding the profile.
    pub fn begin_start(&mut self, name: &SessionName) -> Result<MachineName, StoreError> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = tx
            .query_row(
                "SELECT name, profile, image, state, generation, failure FROM sessions WHERE name = ?1",
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
        let updated = tx.execute(
            "UPDATE sessions SET state = 'starting', generation = ?2, failure = NULL WHERE name = ?1",
            params![name.as_str(), to_sql_generation(generation)?],
        );
        match updated {
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
                let holder: String = tx.query_row(
                    "SELECT name FROM sessions WHERE profile = ?1 AND state IN ('starting', 'ready', 'stopping')",
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

    pub fn create_run(&mut self, session: &SessionName, job: &str) -> Result<(i64, i64), StoreError> {
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO runs (session, job) VALUES (?1, ?2)", params![session.as_str(), job])?;
        let run = tx.last_insert_rowid();
        tx.execute("INSERT INTO attempts (run, state) VALUES (?1, 'starting')", [run])?;
        let attempt = tx.last_insert_rowid();
        tx.commit()?;
        Ok((run, attempt))
    }

    pub fn set_attempt(&mut self, attempt: i64, state: AttemptState) -> Result<(), StoreError> {
        self.db.execute("UPDATE attempts SET state = ?2 WHERE id = ?1", params![attempt, state.as_str()])?;
        Ok(())
    }

    pub fn attempts(&self) -> Result<Vec<AttemptRecord>, StoreError> {
        let mut stmt = self.db.prepare("SELECT id, run, state FROM attempts ORDER BY id")?;
        let raw = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|(id, run, state)| {
                let state = AttemptState::parse(&state).ok_or_else(|| StoreError::Corrupt(format!("attempt state {state:?}")))?;
                Ok(AttemptRecord { id, run, state })
            })
            .collect()
    }

    /// Applies the record-keeping half of a reconciliation plan. Stopping stale machines is the caller's effect.
    pub fn apply(&mut self, actions: &[Action]) -> Result<(), StoreError> {
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
                    tx.execute("UPDATE attempts SET state = 'interrupted' WHERE id = ?1", [attempt])?;
                }
                Action::Adopt { .. } | Action::StopStale { .. } => {}
            }
        }
        tx.commit()?;
        Ok(())
    }
}

type RawSession = (String, String, String, String, i64, Option<String>);

fn raw_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawSession> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
}

fn session_record((name, profile, image, state, generation, failure): RawSession) -> Result<SessionRecord, StoreError> {
    Ok(SessionRecord {
        name: parse_session_name(&name)?,
        profile: ProfileName::new(&profile).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        image,
        state: SessionState::parse(&state).ok_or_else(|| StoreError::Corrupt(format!("session state {state:?}")))?,
        generation: Generation(u64::try_from(generation).map_err(|_| StoreError::Corrupt(format!("generation {generation}")))?),
        failure,
    })
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
    use crate::state::MachineObservation;

    fn name(s: &str) -> SessionName {
        SessionName::new(s).unwrap()
    }

    fn store_with(sessions: &[(&str, &str)]) -> Store {
        let mut store = Store::open_in_memory().unwrap();
        for (session, profile) in sessions {
            store.create_session(&name(session), &ProfileName::new(profile).unwrap(), "ref-alpine-chromium").unwrap();
        }
        store
    }

    #[test]
    fn a_profile_has_at_most_one_writable_session() {
        let mut store = store_with(&[("a", "personal"), ("b", "personal")]);
        store.begin_start(&name("a")).unwrap();
        match store.begin_start(&name("b")) {
            Err(StoreError::ProfileBusy { profile, holder }) => {
                assert_eq!((profile.as_str(), holder.as_str()), ("personal", "a"));
            }
            other => panic!("expected ProfileBusy, got {other:?}"),
        }
        assert_eq!(store.session(&name("b")).unwrap().state, SessionState::Stopped);

        store.transition(&name("a"), Generation(1), SessionState::Stopped, None).unwrap();
        assert_eq!(store.begin_start(&name("b")).unwrap().to_string(), "bctl-b-1");
    }

    #[test]
    fn each_start_records_a_new_generation_before_any_machine_exists() {
        let mut store = store_with(&[("research", "personal")]);
        assert_eq!(store.begin_start(&name("research")).unwrap().to_string(), "bctl-research-1");
        let session = store.session(&name("research")).unwrap();
        assert_eq!((session.state, session.generation), (SessionState::Starting, Generation(1)));
        assert!(matches!(store.begin_start(&name("research")), Err(StoreError::NotStartable { .. })));
        store.transition(&name("research"), Generation(1), SessionState::Failed, Some("boom")).unwrap();
        assert_eq!(store.begin_start(&name("research")).unwrap().to_string(), "bctl-research-2");
    }

    #[test]
    fn a_report_about_an_older_generation_changes_nothing() {
        let mut store = store_with(&[("research", "personal")]);
        store.begin_start(&name("research")).unwrap();
        store.transition(&name("research"), Generation(1), SessionState::Failed, None).unwrap();
        store.begin_start(&name("research")).unwrap();
        assert!(matches!(
            store.transition(&name("research"), Generation(1), SessionState::Ready, None),
            Err(StoreError::StaleGeneration { .. })
        ));
        assert_eq!(store.session(&name("research")).unwrap().state, SessionState::Starting);
    }

    #[test]
    fn restart_reconciliation_interrupts_live_attempts_and_never_requeues_them() {
        let mut store = store_with(&[("research", "personal")]);
        store.begin_start(&name("research")).unwrap();
        store.transition(&name("research"), Generation(1), SessionState::Ready, None).unwrap();
        let (_, attempt) = store.create_run(&name("research"), "mirror-docs").unwrap();
        store.set_attempt(attempt, AttemptState::Running).unwrap();

        let machines = [MachineObservation { name: "bctl-research-1".into(), running: true }];
        let plan = reconcile(&store.sessions().unwrap(), &store.attempts().unwrap(), &machines);
        store.apply(&plan).unwrap();

        assert_eq!(store.session(&name("research")).unwrap().state, SessionState::Ready);
        assert_eq!(store.attempts().unwrap()[0].state, AttemptState::Interrupted);
        let again = reconcile(&store.sessions().unwrap(), &store.attempts().unwrap(), &machines);
        assert!(again.iter().all(|a| !matches!(a, Action::Interrupt { .. })), "{again:?}");
    }

    #[test]
    fn a_vanished_machine_fails_its_session_and_frees_the_profile() {
        let mut store = store_with(&[("a", "personal"), ("b", "personal")]);
        store.begin_start(&name("a")).unwrap();
        let plan = reconcile(&store.sessions().unwrap(), &[], &[]);
        store.apply(&plan).unwrap();
        let a = store.session(&name("a")).unwrap();
        assert_eq!(a.state, SessionState::Failed);
        assert!(a.failure.unwrap().contains("vanished"));
        store.begin_start(&name("b")).unwrap();
    }

    #[test]
    fn state_survives_reopening_the_database() {
        let dir = std::env::temp_dir().join(format!("bctl-core-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        {
            let mut store = Store::open(&path).unwrap();
            store.create_session(&name("research"), &ProfileName::new("personal").unwrap(), "img").unwrap();
            store.begin_start(&name("research")).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.session(&name("research")).unwrap().generation, Generation(1));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
