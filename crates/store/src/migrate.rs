//! Numbered schema migrations, tracked in SQLite's `user_version`.

use rusqlite::{Connection, TransactionBehavior};

use crate::error::StoreError;

/// Every migration in order; the schema version is the number applied.
pub const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_queue_rank.sql"),
    include_str!("../migrations/0003_rules_hash.sql"),
    include_str!("../migrations/0004_tick.sql"),
    include_str!("../migrations/0005_park_recaps.sql"),
    include_str!("../migrations/0006_effort.sql"),
    include_str!("../migrations/0007_session_pane.sql"),
    include_str!("../migrations/0008_root_resume.sql"),
    include_str!("../migrations/0009_orchestrator.sql"),
    include_str!("../migrations/0010_closed_task_rows.sql"),
    include_str!("../migrations/0011_job_identity.sql"),
    include_str!("../migrations/0012_helper_worktrees.sql"),
    include_str!("../migrations/0013_leads.sql"),
    include_str!("../migrations/0014_session_mode.sql"),
    include_str!("../migrations/0015_session_identity.sql"),
    include_str!("../migrations/0016_session_waiting.sql"),
    include_str!("../migrations/0017_pr_rows_by_task.sql"),
    include_str!("../migrations/0018_usage.sql"),
    include_str!("../migrations/0019_reviews.sql"),
];

/// Applies the migrations the database lacks, each in its own immediate transaction.
///
/// The version is read again inside each transaction, so two processes opening a new store at once never apply the
/// same migration twice: the second waits for the first and then finds the migration done.
///
/// # Errors
///
/// Returns [`StoreError::SchemaTooNew`] when the database is newer than [`MIGRATIONS`], or the SQLite error of a
/// failing migration, which is rolled back.
pub fn migrate(conn: &mut Connection) -> Result<i64, StoreError> {
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let applied = usize::try_from(found).map_err(|_| StoreError::OutOfRange(found))?;
        let Some(sql) = MIGRATIONS.get(applied) else {
            if applied > MIGRATIONS.len() {
                return Err(StoreError::SchemaTooNew {
                    found,
                    known: MIGRATIONS.len(),
                });
            }
            return Ok(found);
        };
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", found + 1)?;
        tx.commit()?;
    }
}
