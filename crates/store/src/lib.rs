//! The SQLite store: one file holding tasks, sessions, worktrees, pull requests, asks, rules, jobs and the event log.
//!
//! Every state change goes through [`Store`], which asks [`headwork_core`] for the decision and writes the result and
//! its event in one transaction. Refusals are logged as events too, so a wrong deny can be debugged from the log.

mod error;
mod migrate;
mod queries;
mod records;

use std::path::Path;

use headwork_core::{Actor, MachineMode};
use rusqlite::{Connection, params};

pub use error::StoreError;
pub use migrate::{MIGRATIONS, migrate};
pub use queries::{EventFilter, FABLE_ALLOW, NewAsk, NewSession, NewTask};
pub use records::{
    AskId, AskRow, Board, BoardTask, EventRecord, PrRow, ProjectRow, SessionRow, Task, TaskId,
};

/// The SQL expression for the current time, UTC, with milliseconds.
pub(crate) const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

/// An open store.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens or creates the store at `path`, in WAL mode, and migrates it.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the file cannot be opened, a migration fails, or the schema is newer than this
    /// build knows.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::prepare(conn)
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
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Self { conn })
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
