//! Sessions as the launcher sees them, and task worktrees.

use headwork_core::Actor;
use rusqlite::{OptionalExtension, params};

use crate::queries::insert_event;
use crate::records::{SessionRow, TaskId, WorktreeRow};
use crate::{NOW, Store, StoreError};

const SESSION_COLUMNS: &str = "id, state, rc_name, host_workspace, host_surface, launcher_pid, claude_pid, last_event_at, exit_code";

type RawSession = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
);

/// A live session's id, `claude` process and launcher process.
pub type LivePids = (String, Option<u32>, Option<u32>);

fn read_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawSession> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn pid(value: Option<i64>) -> Result<Option<u32>, StoreError> {
    value
        .map(|n| u32::try_from(n).map_err(|_| StoreError::OutOfRange(n)))
        .transpose()
}

fn into_row(raw: RawSession) -> Result<SessionRow, StoreError> {
    let (id, state, rc_name, host_workspace, host_surface, launcher, claude, last_event_at, exit) =
        raw;
    Ok(SessionRow {
        id,
        state: state.parse()?,
        rc_name,
        host_workspace,
        host_surface,
        launcher_pid: pid(launcher)?,
        claude_pid: pid(claude)?,
        last_event_at,
        exit_code: exit
            .map(|n| i32::try_from(n).map_err(|_| StoreError::OutOfRange(n)))
            .transpose()?,
    })
}

impl Store {
    /// A task's latest session.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn session_of(&self, task: TaskId) -> Result<Option<SessionRow>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions WHERE task_id = ?1 ORDER BY started_at DESC, rowid DESC
                     LIMIT 1"
                ),
                params![task.0],
                read_session,
            )
            .optional()?
            .map(into_row)
            .transpose()
    }

    /// The session with this id and its task.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn session(&self, id: &str) -> Result<Option<(SessionRow, Option<TaskId>)>, StoreError> {
        let found: Option<(RawSession, Option<i64>)> = self
            .conn
            .query_row(
                &format!("SELECT {SESSION_COLUMNS}, task_id FROM sessions WHERE id = ?1"),
                params![id],
                |row| Ok((read_session(row)?, row.get(9)?)),
            )
            .optional()?;
        found
            .map(|(raw, task)| Ok((into_row(raw)?, task.map(TaskId))))
            .transpose()
    }

    /// The processes of every session that has not ended: `(session id, claude pid, launcher pid)`.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn live_session_pids(&self) -> Result<Vec<LivePids>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, claude_pid, launcher_pid FROM sessions WHERE state IN ('starting', 'live') ORDER BY started_at",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, claude, launcher)| Ok((id, pid(claude)?, pid(launcher)?)))
            .collect()
    }

    /// Every session that has not ended, by Remote Control name, with the data of its newest event of `kind`.
    ///
    /// # Errors
    ///
    /// Returns a read error, or a JSON error for an event whose data is not JSON.
    pub fn live_sessions_with_event(
        &self,
        kind: &str,
    ) -> Result<Vec<(String, Option<serde_json::Value>)>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT s.rc_name,
                 (SELECT e.data FROM events e WHERE e.session_id = s.id AND e.type = ?1 ORDER BY e.id DESC LIMIT 1)
             FROM sessions s WHERE s.state IN ('starting', 'live') ORDER BY s.started_at",
        )?;
        let rows = statement
            .query_map(params![kind], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(name, data)| {
                Ok((
                    name,
                    data.map(|data| serde_json::from_str(&data)).transpose()?,
                ))
            })
            .collect()
    }

    /// The data of a session's newest event of `kind`.
    ///
    /// # Errors
    ///
    /// Returns a read error, or a JSON error for an event whose data is not JSON.
    pub fn session_event(
        &self,
        session: &str,
        kind: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let data: Option<String> = self
            .conn
            .query_row(
                "SELECT data FROM events WHERE session_id = ?1 AND type = ?2 ORDER BY id DESC LIMIT 1",
                params![session, kind],
                |row| row.get(0),
            )
            .optional()?;
        Ok(data.map(|data| serde_json::from_str(&data)).transpose()?)
    }

    /// The root's newest session that has not ended.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn live_root_session(&self) -> Result<Option<SessionRow>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions WHERE role = 'root' AND state IN ('starting', 'live')
                     ORDER BY started_at DESC, rowid DESC LIMIT 1"
                ),
                [],
                read_session,
            )
            .optional()?
            .map(into_row)
            .transpose()
    }

    /// Records where the host put a session.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_session_place(
        &mut self,
        id: &str,
        workspace: &str,
        surface: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let task: Option<i64> = tx.query_row(
            "SELECT task_id FROM sessions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        tx.execute(
            "UPDATE sessions SET host_workspace = ?1, host_surface = ?2 WHERE id = ?3",
            params![workspace, surface, id],
        )?;
        let data = serde_json::json!({ "workspace": workspace, "surface": surface });
        insert_event(
            &tx,
            actor,
            task.map(TaskId),
            Some(id),
            "session-placed",
            &data,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records that the launcher started `claude`: both process ids, and how the launcher holds the tree.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn session_launched(
        &mut self,
        id: &str,
        launcher_pid: u32,
        claude_pid: u32,
        ownership: &str,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let task: Option<i64> = tx.query_row(
            "SELECT task_id FROM sessions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        tx.execute(
            &format!(
                "UPDATE sessions SET launcher_pid = ?1, claude_pid = ?2, state = 'live', last_event_at = {NOW}
                 WHERE id = ?3"
            ),
            params![i64::from(launcher_pid), i64::from(claude_pid), id],
        )?;
        let data = serde_json::json!({
            "launcher_pid": launcher_pid, "claude_pid": claude_pid, "tree": ownership,
        });
        insert_event(
            &tx,
            Actor::Child,
            task.map(TaskId),
            Some(id),
            "launched",
            &data,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records that `claude` exited.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn session_ended(&mut self, id: &str, exit_code: Option<i32>) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let task: Option<i64> = tx.query_row(
            "SELECT task_id FROM sessions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        tx.execute(
            &format!(
                "UPDATE sessions SET state = 'ended', exit_code = ?1, ended_at = {NOW}, last_event_at = {NOW}
                 WHERE id = ?2"
            ),
            params![exit_code, id],
        )?;
        let data = serde_json::json!({ "exit_code": exit_code });
        insert_event(
            &tx,
            Actor::Child,
            task.map(TaskId),
            Some(id),
            "exited",
            &data,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records a task's worktree as present.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn record_worktree(
        &mut self,
        task: TaskId,
        path: &str,
        branch: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO worktrees (task_id, path, branch, state) VALUES (?1, ?2, ?3, 'present')
             ON CONFLICT(path) DO UPDATE SET task_id = excluded.task_id, branch = excluded.branch, state = 'present'",
            params![task.0, path, branch],
        )?;
        let data = serde_json::json!({ "path": path, "branch": branch });
        insert_event(&tx, actor, Some(task), None, "worktree-recorded", &data)?;
        tx.commit()?;
        Ok(())
    }

    /// A task's worktree.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn worktree_of(&self, task: TaskId) -> Result<Option<WorktreeRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT path, branch, state FROM worktrees WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                params![task.0],
                |row| {
                    Ok(WorktreeRow {
                        path: row.get(0)?,
                        branch: row.get(1)?,
                        state: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Sets the state of a task's worktree: `removing`, `removed` or `kept`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_worktree_state(
        &mut self,
        task: TaskId,
        state: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE worktrees SET state = ?1 WHERE task_id = ?2",
            params![state, task.0],
        )?;
        insert_event(
            &tx,
            actor,
            Some(task),
            None,
            "worktree-state",
            &serde_json::json!({ "state": state }),
        )?;
        tx.commit()?;
        Ok(())
    }
}
