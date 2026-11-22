//! Sessions that wait on the user and run nothing else, and so give up their worker slot (`headwork_core::slots`).
//!
//! `sessions.waiting_since` holds the mark: set when such a session was seen, cleared by the session's next hook call
//! or when a check finds it working again. A task counts as waiting only while its newest session is marked and an ask
//! of that session still waits in a dialog.

use headwork_core::Actor;
use rusqlite::params;

use crate::queries::insert_event;
use crate::records::TaskId;
use crate::{NOW, Store, StoreError};

/// The event that records a session's waiting mark changing.
pub const EVENT_SESSION_WAITING: &str = "session-waiting";

/// The ask kinds the user answers in the session's own dialog, which holds the session still until answered.
pub(crate) const DIALOG_ASK_KINDS: &str = "('question', 'review', 'permission')";

/// A running session with an ask waiting in its dialog, as the tick sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitingCandidate {
    /// The Claude session id.
    pub session: String,
    /// Its task.
    pub task: TaskId,
    /// Its `claude` process, when recorded.
    pub claude_pid: Option<u32>,
    /// It carries the waiting mark now.
    pub marked: bool,
}

impl Store {
    /// Whether an ask of the session waits on the user in a dialog.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn dialog_ask_waits(&self, session: &str) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM asks WHERE session_id = ?1 AND state IN ('pending', 'open')
                     AND kind IN {DIALOG_ASK_KINDS})"
            ),
            params![session],
            |row| row.get(0),
        )?)
    }

    /// Sets or clears the session's waiting mark, logging `session-waiting` when it changes. Returns whether it
    /// changed.
    ///
    /// # Errors
    ///
    /// Returns a write error.
    pub fn set_session_waiting(
        &mut self,
        session: &str,
        waiting: bool,
        actor: Actor,
    ) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let changed = if waiting {
            tx.execute(
                &format!("UPDATE sessions SET waiting_since = {NOW} WHERE id = ?1 AND waiting_since IS NULL"),
                params![session],
            )?
        } else {
            tx.execute(
                "UPDATE sessions SET waiting_since = NULL WHERE id = ?1 AND waiting_since IS NOT NULL",
                params![session],
            )?
        };
        if changed > 0 {
            let task: Option<i64> = tx.query_row(
                "SELECT task_id FROM sessions WHERE id = ?1",
                params![session],
                |row| row.get(0),
            )?;
            insert_event(
                &tx,
                actor,
                task.map(TaskId),
                Some(session),
                EVENT_SESSION_WAITING,
                &serde_json::json!({ "waiting": waiting }),
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Running sessions of tasks that either carry the waiting mark or have an ask waiting in their dialog: the ones
    /// whose mark a check may have to set or clear.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn waiting_candidates(&self) -> Result<Vec<WaitingCandidate>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT s.id, s.task_id, s.claude_pid, s.waiting_since IS NOT NULL FROM sessions s
             WHERE s.task_id IS NOT NULL AND s.state NOT IN ('ended', 'gone')
               AND (s.waiting_since IS NOT NULL OR EXISTS (SELECT 1 FROM asks a WHERE a.session_id = s.id
                    AND a.state IN ('pending', 'open') AND a.kind IN {DIALOG_ASK_KINDS}))
             ORDER BY s.rowid"
        ))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(session, task, pid, marked)| {
                Ok(WaitingCandidate {
                    session,
                    task: TaskId(task),
                    claude_pid: pid
                        .map(|pid| u32::try_from(pid).map_err(|_| StoreError::OutOfRange(pid)))
                        .transpose()?,
                    marked,
                })
            })
            .collect()
    }

    /// Whether a task is queued.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn has_queued(&self) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM tasks WHERE state = 'queued')",
            [],
            |row| row.get(0),
        )?)
    }
}
