//! The machine's record of migration notes: which it applied or skipped, per repository (`docs/migrations.md`).

use std::collections::BTreeMap;

use headwork_core::Actor;
use headwork_core::migrate::Settled;
use rusqlite::params;
use serde::Serialize;

use crate::queries::insert_event;
use crate::{NOW, Store, StoreError};

/// The event that records a note applied, or found already applied by its check.
pub const EVENT_MIGRATION_APPLIED: &str = "migration-applied";

/// The event that records a note skipped on purpose.
pub const EVENT_MIGRATION_SKIPPED: &str = "migration-skipped";

/// A recorded note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationRow {
    /// The repository the note belongs to, as its key in the machine's sources.
    pub repo: String,
    /// The note's id.
    pub note_id: String,
    /// How it was settled.
    pub state: Settled,
    /// Who settled it.
    pub actor: String,
    /// When, UTC.
    pub at: String,
    /// What the steps printed.
    pub output: String,
    /// Why it was skipped, or how it counted as applied.
    pub reason: String,
}

impl Store {
    /// Records a note as settled, replacing an earlier record, and logs `migration-applied` or `migration-skipped`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn settle_migration(
        &mut self,
        repo: &str,
        note_id: &str,
        state: Settled,
        output: &str,
        reason: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO migrations_applied (repo, note_id, state, actor, at, output, reason)
                 VALUES (?1, ?2, ?3, ?4, {NOW}, ?5, ?6)
                 ON CONFLICT (repo, note_id) DO UPDATE SET
                     state = excluded.state, actor = excluded.actor, at = excluded.at,
                     output = excluded.output, reason = excluded.reason"
            ),
            params![
                repo,
                note_id,
                state.as_str(),
                actor.as_str(),
                output,
                reason
            ],
        )?;
        let kind = match state {
            Settled::Applied => EVENT_MIGRATION_APPLIED,
            Settled::Skipped => EVENT_MIGRATION_SKIPPED,
        };
        let data = serde_json::json!({ "repo": repo, "note": note_id, "reason": reason });
        insert_event(&tx, actor, None, None, kind, &data)?;
        tx.commit()?;
        Ok(())
    }

    /// The notes recorded for a repository, by id.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn settled_migrations(&self, repo: &str) -> Result<BTreeMap<String, Settled>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT note_id, state FROM migrations_applied WHERE repo = ?1")?;
        let rows = statement.query_map(params![repo], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut settled = BTreeMap::new();
        for row in rows {
            let (id, state) = row?;
            if let Some(state) = Settled::parse(&state) {
                settled.insert(id, state);
            }
        }
        Ok(settled)
    }

    /// Every recorded note, oldest first.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn migration_rows(&self) -> Result<Vec<MigrationRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT repo, note_id, state, actor, at, output, reason FROM migrations_applied ORDER BY at, repo, note_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (repo, note_id, state, actor, at, output, reason) = row?;
            if let Some(state) = Settled::parse(&state) {
                out.push(MigrationRow {
                    repo,
                    note_id,
                    state,
                    actor,
                    at,
                    output,
                    reason,
                });
            }
        }
        Ok(out)
    }
}
