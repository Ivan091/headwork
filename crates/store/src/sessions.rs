//! Sessions as the launcher sees them, and task worktrees.

use headwork_core::{Actor, PlaceKind};
use rusqlite::{OptionalExtension, params};

/// Where the host put a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place<'a> {
    /// The workspace.
    pub workspace: &'a str,
    /// The surface.
    pub surface: &'a str,
    /// The pane holding the surface, when known.
    pub pane: Option<&'a str>,
    /// A pane of its own or a tab, when known.
    pub kind: Option<PlaceKind>,
}

use crate::queries::insert_event;
use crate::records::{SessionRow, TaskId, WorktreeRow};
use crate::{NOW, Store, StoreError};

const SESSION_COLUMNS: &str = "id, state, rc_name, host_workspace, host_surface, launcher_pid, claude_pid, last_event_at, exit_code, \
                               host_pane, host_place";

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
    (Option<String>, Option<String>),
);

const SESSION_WIDTH: usize = 11;

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
        (row.get(9)?, row.get(10)?),
    ))
}

fn seconds(value: Option<i64>) -> Result<Option<u64>, StoreError> {
    value
        .map(|n| u64::try_from(n).map_err(|_| StoreError::OutOfRange(n)))
        .transpose()
}

/// A session that ended, as `reap` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedSession {
    /// Its Remote Control name: the task's name, or the orchestrator's.
    pub name: String,
    /// The launcher process.
    pub launcher_pid: Option<u32>,
    /// The `claude` process.
    pub claude_pid: Option<u32>,
    /// When it started, in seconds since the unix epoch.
    pub started: Option<u64>,
    /// When it ended, in seconds since the unix epoch.
    pub ended: Option<u64>,
    /// The worktree folders of its task, the task's own and its helpers'; none for a main-only task or the orchestrator.
    pub folders: Vec<String>,
}

fn pid(value: Option<i64>) -> Result<Option<u32>, StoreError> {
    value
        .map(|n| u32::try_from(n).map_err(|_| StoreError::OutOfRange(n)))
        .transpose()
}

fn into_row(raw: RawSession) -> Result<SessionRow, StoreError> {
    let (
        id,
        state,
        rc_name,
        host_workspace,
        host_surface,
        launcher,
        claude,
        last_event_at,
        exit,
        (host_pane, host_place),
    ) = raw;
    Ok(SessionRow {
        id,
        state: state.parse()?,
        rc_name,
        host_workspace,
        host_surface,
        host_pane,
        host_place: host_place.map(|word| word.parse()).transpose()?,
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
                |row| Ok((read_session(row)?, row.get(SESSION_WIDTH)?)),
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

    /// Every session that ended or was found gone, oldest first, with its processes and times, for `reap`.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn ended_sessions(&self) -> Result<Vec<EndedSession>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT rc_name, launcher_pid, claude_pid, CAST(strftime('%s', started_at) AS INTEGER),
                    CAST(strftime('%s', ended_at) AS INTEGER), task_id
             FROM sessions WHERE state IN ('ended', 'gone') ORDER BY started_at, rowid",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut folders = self
            .conn
            .prepare("SELECT path FROM worktrees WHERE task_id = ?1 ORDER BY id")?;
        rows.into_iter()
            .map(|(name, launcher, claude, started, ended, task)| {
                let folders = match task {
                    Some(task) => folders
                        .query_map(params![task], |row| row.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?,
                    None => Vec::new(),
                };
                Ok(EndedSession {
                    name,
                    launcher_pid: pid(launcher)?,
                    claude_pid: pid(claude)?,
                    started: seconds(started)?,
                    ended: seconds(ended)?,
                    folders,
                })
            })
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

    /// The orchestrator's newest session that has not ended.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn live_orchestrator_session(&self) -> Result<Option<SessionRow>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions WHERE role = 'orchestrator' AND state IN ('starting', 'live')
                     ORDER BY started_at DESC, rowid DESC LIMIT 1"
                ),
                [],
                read_session,
            )
            .optional()?
            .map(into_row)
            .transpose()
    }

    /// The orchestrator session with this Claude session id, in any state; `None` when the id is unknown or names a
    /// worker's session.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn orchestrator_session(&self, id: &str) -> Result<Option<SessionRow>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1 AND role = 'orchestrator'"
                ),
                params![id],
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
        place: &Place<'_>,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let task: Option<i64> = tx.query_row(
            "SELECT task_id FROM sessions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        let kind = place.kind.map(PlaceKind::as_str);
        tx.execute(
            "UPDATE sessions SET host_workspace = ?1, host_surface = ?2, host_pane = ?3, host_place = ?4 WHERE id = ?5",
            params![place.workspace, place.surface, place.pane, kind, id],
        )?;
        let data = serde_json::json!({
            "workspace": place.workspace,
            "surface": place.surface,
            "pane": place.pane,
            "place": kind,
        });
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

    /// The host surfaces of every session that has not ended.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn live_session_surfaces(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT host_surface FROM sessions WHERE state IN ('starting', 'live') AND host_surface IS NOT NULL",
        )?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The tasks whose sessions have not ended and sit in a host surface, with that surface, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn live_task_surfaces(&self) -> Result<Vec<(TaskId, String)>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT task_id, host_surface FROM sessions
             WHERE state IN ('starting', 'live') AND host_surface IS NOT NULL AND task_id IS NOT NULL
             ORDER BY started_at",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, surface)| (TaskId(id), surface))
            .collect())
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
            Actor::Worker,
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
            Actor::Worker,
            task.map(TaskId),
            Some(id),
            "exited",
            &data,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Ends every session of a task that is still `starting` or `live`, as a close does once its processes are gone;
    /// returns how many it ended.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn end_task_sessions(&mut self, task: TaskId, actor: Actor) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        let ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM sessions WHERE task_id = ?1 AND state IN ('starting', 'live')",
            )?;
            let rows = stmt.query_map(params![task.0], |row| row.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for id in &ids {
            tx.execute(
                &format!(
                    "UPDATE sessions SET state = 'ended', ended_at = COALESCE(ended_at, {NOW}), last_event_at = {NOW}
                     WHERE id = ?1"
                ),
                params![id],
            )?;
            insert_event(
                &tx,
                actor,
                Some(task),
                Some(id),
                "session-state",
                &serde_json::json!({ "state": "ended", "why": "the task closed" }),
            )?;
        }
        tx.commit()?;
        Ok(ids.len())
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
             ON CONFLICT(path) DO UPDATE SET task_id = excluded.task_id, branch = excluded.branch, state = 'present',
               helper = 0",
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
                "SELECT path, branch, state FROM worktrees WHERE task_id = ?1 AND helper = 0 ORDER BY id DESC LIMIT 1",
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
            "UPDATE worktrees SET state = ?1 WHERE task_id = ?2 AND helper = 0",
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

    /// Records a helper worktree a task made for a part of its work, writable for that task and removed when it
    /// closes.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn record_helper_worktree(
        &mut self,
        task: TaskId,
        path: &str,
        branch: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO worktrees (task_id, path, branch, state, helper) VALUES (?1, ?2, ?3, 'present', 1)
             ON CONFLICT(path) DO UPDATE SET task_id = excluded.task_id, branch = excluded.branch, state = 'present',
               helper = 1",
            params![task.0, path, branch],
        )?;
        let data = serde_json::json!({ "path": path, "branch": branch });
        insert_event(
            &tx,
            actor,
            Some(task),
            None,
            "helper-worktree-recorded",
            &data,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A task's helper worktrees that are not removed, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn helper_worktrees(&self, task: TaskId) -> Result<Vec<WorktreeRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT path, branch, state FROM worktrees WHERE task_id = ?1 AND helper = 1 AND state != 'removed'
             ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![task.0], |row| {
                Ok(WorktreeRow {
                    path: row.get(0)?,
                    branch: row.get(1)?,
                    state: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every worktree recorded for a task, its own and helpers, in any state, newest first: the task, the row, and
    /// whether it is a helper.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn recorded_worktrees(&self) -> Result<Vec<(TaskId, WorktreeRow, bool)>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT task_id, path, branch, state, helper FROM worktrees ORDER BY id DESC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    TaskId(row.get(0)?),
                    WorktreeRow {
                        path: row.get(1)?,
                        branch: row.get(2)?,
                        state: row.get(3)?,
                    },
                    row.get::<_, i64>(4)? != 0,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Sets the state of one helper worktree of a task: `removed` or `kept`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_helper_worktree_state(
        &mut self,
        task: TaskId,
        path: &str,
        state: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE worktrees SET state = ?1 WHERE task_id = ?2 AND path = ?3 AND helper = 1",
            params![state, task.0, path],
        )?;
        let data = serde_json::json!({ "path": path, "state": state });
        insert_event(&tx, actor, Some(task), None, "helper-worktree-state", &data)?;
        tx.commit()?;
        Ok(())
    }
}
