//! The SQLite store: one file holding tasks, sessions, worktrees, pull requests, asks, rules, jobs and the event log.
//!
//! Every state change goes through [`Store`], which asks [`headwork_core`] for the decision and writes the result and
//! its event in one transaction. Refusals are logged as events too, so a wrong deny can be debugged from the log.

mod backup;
mod error;
mod hooks;
mod migrate;
mod park;
mod queries;
mod queue;
mod records;
mod reports;
mod sessions;
mod tick;

use std::path::Path;

use headwork_core::{Actor, MachineMode};
use rusqlite::{Connection, TransactionBehavior, params};

pub use backup::{BACKUP_DIR, BACKUPS_KEPT, Backup};
pub use error::StoreError;
pub use hooks::{
    EVENT_DENY, EVENT_PROMPT, EVENT_READDRESSED, EVENT_TOOL, EVENT_USER_ONLY_ALLOWED,
    EVENT_WOULD_DENY, HookSession, RulesVersion, TurnSoFar,
};
pub use migrate::{MIGRATIONS, migrate};
pub use park::{JobRow, NewJob, NewRecap, ParkRow, RecapRow};
pub use queries::{EventFilter, FABLE_ALLOW, NewAsk, NewSession, NewTask};
pub use queue::AdoptTask;
pub use records::{
    AskId, AskRow, Board, BoardTask, EventRecord, PrRow, ProjectRow, SessionRow, Task, TaskId,
    WorktreeRow,
};
pub use reports::{AddedRule, EVENT_REPORT, NewPr};
pub use sessions::{EndedSession, LivePids, Place};
pub use tick::{PrSeen, SessionClock, StoredAsk, TopicAsk, WaitingAsk};

/// The SQL expression for the current time, UTC, with milliseconds.
pub(crate) const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

/// How long a connection waits for another writer before giving up with `database is locked`.
pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Switches the connection's database to WAL, retrying while another connection holds the lock.
///
/// SQLite does not call the busy handler for this switch: when several processes create a new store at once, all but
/// one get `SQLITE_BUSY` immediately. Once the file is in WAL mode the switch is a no-op that takes no lock.
fn enter_wal(conn: &Connection) -> Result<(), StoreError> {
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.pragma_update(None, "journal_mode", "WAL") {
            Err(rusqlite::Error::SqliteFailure(failure, _))
                if failure.code == rusqlite::ErrorCode::DatabaseBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            outcome => return Ok(outcome?),
        }
    }
}

/// An open store.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    backup: Option<Backup>,
}

impl Store {
    /// Opens or creates the store at `path`, in WAL mode, and migrates it.
    ///
    /// Before migrating an existing store older than this build, it copies it to [`BACKUP_DIR`] beside `path`
    /// (`state-<from>-to-<to>-<millis>-<pid>.db`, the newest [`BACKUPS_KEPT`] kept); [`Store::backup`] tells what
    /// happened. A failed copy does not stop the migration: a store this build cannot open would stop every hook.
    ///
    /// Many processes share one file (the CLI, launchers, hooks, MCP servers, the tick), so a writer waits up to
    /// [`BUSY_TIMEOUT`] for another instead of failing, and every transaction starts `IMMEDIATE`: it takes the write
    /// lock up front, so a transaction that reads and then writes never has to upgrade a read snapshot, which SQLite
    /// refuses at once with `SQLITE_BUSY` whatever the timeout.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the file cannot be opened, a migration fails, or the schema is newer than this
    /// build knows.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        enter_wal(&conn)?;
        let backup = backup::before_migrating(&conn, path);
        let mut store = Self::prepare(conn)?;
        store.backup = backup;
        Ok(store)
    }

    /// Opens a fresh store in memory, for tests.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when SQLite or a migration fails.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
        migrate(&mut conn)?;
        Ok(Self { conn, backup: None })
    }

    /// The copy [`Store::open`] took before migrating, or why it could not; `None` when nothing was migrated.
    #[must_use]
    pub const fn backup(&self) -> Option<&Backup> {
        self.backup.as_ref()
    }

    /// Copies the whole store to a new file at `path` with `VACUUM INTO`, consistent while other processes write it;
    /// `headwork update` smoke-tests a release against such a copy.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error, such as when `path` already exists.
    pub fn copy_to(&self, path: &Path) -> Result<(), StoreError> {
        self.conn
            .execute("VACUUM INTO ?1", [path.to_string_lossy()])?;
        Ok(())
    }

    /// The schema version.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    /// The machine mode.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the row is missing or holds an unknown mode.
    pub fn machine_mode(&self) -> Result<MachineMode, StoreError> {
        let mode: String =
            self.conn
                .query_row("SELECT mode FROM machine WHERE id = 1", [], |row| {
                    row.get(0)
                })?;
        Ok(mode.parse()?)
    }

    /// Sets the machine mode and logs it.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_machine_mode(&mut self, mode: MachineMode, actor: Actor) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let before: String = tx.query_row("SELECT mode FROM machine WHERE id = 1", [], |row| {
            row.get(0)
        })?;
        tx.execute(
            &format!("UPDATE machine SET mode = ?1, updated_at = {NOW} WHERE id = 1"),
            params![mode.as_str()],
        )?;
        let data = serde_json::json!({ "from": before, "to": mode });
        queries::insert_event(&tx, actor, None, None, "machine-mode", &data)?;
        tx.commit()?;
        Ok(())
    }

    /// Appends an event that is not a transition, such as a hook decision.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn append_event(
        &self,
        actor: Actor,
        task: Option<TaskId>,
        session: Option<&str>,
        kind: &str,
        data: &serde_json::Value,
    ) -> Result<(), StoreError> {
        queries::insert_event(&self.conn, actor, task, session, kind, data)
    }
}
