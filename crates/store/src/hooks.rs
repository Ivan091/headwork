//! What the Claude Code hooks read and write: the calling session, the rules version, recent tool calls and the turn
//! since the last prompt.

use headwork_core::{Actor, Role, SessionState};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::queries::insert_event;
use crate::records::{AskId, TaskId};
use crate::{NOW, Store, StoreError};

/// The event a `UserPromptSubmit` hook records; a turn starts there.
pub const EVENT_PROMPT: &str = "prompt";
/// The event a `PostToolUse` hook records for a tool the checks count.
pub const EVENT_TOOL: &str = "tool-used";
/// The event of a deny that blocked.
pub const EVENT_DENY: &str = "deny";
/// The event of a deny that shadow mode only logged.
pub const EVENT_WOULD_DENY: &str = "would-deny";
/// The event that records a user-only command the user allowed for one task.
pub const EVENT_USER_ONLY_ALLOWED: &str = "user-only-allowed";
/// The event of a session message to an orchestrator address that a `PreToolUse` hook sent on to the live address.
pub const EVENT_READDRESSED: &str = "readdressed";

type RawHookSession = (String, Option<i64>, String, String, i64, Option<String>);

/// The calling session, as a hook sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookSession {
    /// The Claude session id.
    pub id: String,
    /// The task, none for the orchestrator.
    pub task_id: Option<TaskId>,
    /// The role.
    pub role: Role,
    /// The process state.
    pub state: SessionState,
    /// The rules version it last received.
    pub rules_seen: i64,
    /// The host pane.
    pub host_surface: Option<String>,
}

/// The rules version and the hash of the rules it stands for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RulesVersion {
    /// Bumped on every change.
    pub version: i64,
    /// The hash of the rules files at that version.
    pub hash: Option<String>,
}

/// What a session did since its last prompt.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent fact the stop gate reads"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct TurnSoFar {
    /// It asked with `AskUserQuestion`.
    pub asked: bool,
    /// It sent a message to another session.
    pub messaged: bool,
    /// It reported `done` or `blocked` through the MCP `report` tool.
    pub reported: bool,
    /// It started background work (a background `Bash`, `Monitor`, `Agent` or `Workflow`) and awaits its notification.
    pub background: bool,
    /// Stops denied (or that would have been) since the prompt.
    pub stops_denied: u32,
}

impl Store {
    /// The session with this id, as a hook sees it.
    ///
    /// # Errors
    ///
    /// Returns a read error or an unknown stored word.
    pub fn hook_session(&self, id: &str) -> Result<Option<HookSession>, StoreError> {
        let raw: Option<RawHookSession> = self
            .conn
            .query_row(
                "SELECT id, task_id, role, state, rules_seen, host_surface FROM sessions WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        raw.map(|(id, task, role, state, rules_seen, host_surface)| {
            Ok(HookSession {
                id,
                task_id: task.map(TaskId),
                role: role.parse()?,
                state: state.parse()?,
                rules_seen,
                host_surface,
            })
        })
        .transpose()
    }

    /// Marks a hook event for the session: `last_event_at` moves on.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn touch_session(&self, id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            &format!("UPDATE sessions SET last_event_at = {NOW} WHERE id = ?1"),
            params![id],
        )?;
        Ok(())
    }

    /// The current rules version.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn rules_version(&self) -> Result<RulesVersion, StoreError> {
        Ok(self.conn.query_row(
            "SELECT rules_version, rules_hash FROM machine WHERE id = 1",
            [],
            |row| {
                Ok(RulesVersion {
                    version: row.get(0)?,
                    hash: row.get(1)?,
                })
            },
        )?)
    }

    /// Records the hash of the rules files; a hash that differs from the stored one bumps the version and is logged.
    /// Returns the version that holds afterwards.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn observe_rules(&mut self, hash: &str, actor: Actor) -> Result<i64, StoreError> {
        let tx = self.conn.transaction()?;
        let (version, stored): (i64, Option<String>) = tx.query_row(
            "SELECT rules_version, rules_hash FROM machine WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if stored.as_deref() == Some(hash) {
            return Ok(version);
        }
        let next = version + 1;
        tx.execute(
            &format!("UPDATE machine SET rules_version = ?1, rules_hash = ?2, updated_at = {NOW} WHERE id = 1"),
            params![next, hash],
        )?;
        let data = serde_json::json!({ "version": next, "hash": hash });
        insert_event(&tx, actor, None, None, "rules-changed", &data)?;
        tx.commit()?;
        Ok(next)
    }

    /// Records that a session received the rules of `version`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_rules_seen(
        &mut self,
        session: &str,
        task: Option<TaskId>,
        version: i64,
        chars: usize,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE sessions SET rules_seen = ?1 WHERE id = ?2",
            params![version, session],
        )?;
        let data = serde_json::json!({ "version": version, "chars": chars });
        insert_event(
            &tx,
            Actor::Hook,
            task,
            Some(session),
            "rules-injected",
            &data,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Whether the session recorded a call of `tool` within the last `seconds`.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn used_tool_within(
        &self,
        session: &str,
        tool: &str,
        seconds: u32,
    ) -> Result<bool, StoreError> {
        let since = format!("-{seconds} seconds");
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE session_id = ?1 AND type = ?2
                 AND json_extract(data, '$.tool') = ?3
                 AND at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?4))",
            params![session, EVENT_TOOL, tool, since],
            |row| row.get(0),
        )?)
    }

    /// The UTC instant `when` names, as the store writes event times (`2026-11-19T10:00:00.000Z`); `None` when SQLite
    /// does not read it as a date or date and time, with or without an offset.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn instant_of(&self, when: &str) -> Result<Option<String>, StoreError> {
        Ok(self.conn.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1)",
            params![when],
            |row| row.get(0),
        )?)
    }

    /// The UTC instant `seconds` before now, as the store writes event times.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn instant_ago(&self, seconds: u64) -> Result<String, StoreError> {
        Ok(self.conn.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
            params![format!("-{seconds} seconds")],
            |row| row.get(0),
        )?)
    }

    /// Records that the user allowed a task to run a user-only command, answering an `Allow` ask. The event log is
    /// the record, so no schema change is needed.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn allow_user_only(
        &mut self,
        task: TaskId,
        session: &str,
        pattern: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        insert_event(
            &self.conn,
            actor,
            Some(task),
            Some(session),
            EVENT_USER_ONLY_ALLOWED,
            &serde_json::json!({ "pattern": pattern }),
        )?;
        Ok(())
    }

    /// The user-only commands the user allowed for a task.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn user_only_allowed(&self, task: TaskId) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT json_extract(data, '$.pattern') FROM events
                 WHERE task_id = ?1 AND type = ?2 AND json_extract(data, '$.pattern') IS NOT NULL ORDER BY 1",
        )?;
        let rows = stmt.query_map(params![task.0, EVENT_USER_ONLY_ALLOWED], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Whether the user typed a prompt into the session within the last `seconds`; a message from another session
    /// does not count.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn user_prompted_within(&self, session: &str, seconds: u32) -> Result<bool, StoreError> {
        let since = format!("-{seconds} seconds");
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE session_id = ?1 AND type = ?2
                 AND NOT COALESCE(json_extract(data, '$.cross_session'), 0)
                 AND at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?3))",
            params![session, EVENT_PROMPT, since],
            |row| row.get(0),
        )?)
    }

    /// Supersedes the session's own asks that are still open, as when the user typed past a dialog instead of
    /// answering it, or the session asks again. Asks the store raised itself (a topic, such as `trust`) stay. Returns
    /// how many were superseded.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn supersede_session_asks(
        &mut self,
        session: &str,
        actor: Actor,
    ) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        let open: Vec<(i64, Option<i64>)> = {
            let mut stmt = tx.prepare(
                "SELECT id, task_id FROM asks WHERE session_id = ?1 AND state = 'open' AND topic IS NULL ORDER BY id",
            )?;
            let rows = stmt.query_map(params![session], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        for (id, task) in &open {
            tx.execute(
                "UPDATE asks SET state = 'superseded' WHERE id = ?1",
                params![id],
            )?;
            insert_event(
                &tx,
                actor,
                task.map(TaskId),
                Some(session),
                "ask-superseded",
                &serde_json::json!({ "ask": id }),
            )?;
        }
        tx.commit()?;
        Ok(open.len())
    }

    /// What the session did since its last prompt.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn turn_so_far(&self, session: &str) -> Result<TurnSoFar, StoreError> {
        let since: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM events WHERE session_id = ?1 AND type = ?2",
            params![session, EVENT_PROMPT],
            |row| row.get(0),
        )?;
        let used = |tool: &str| -> Result<bool, StoreError> {
            Ok(self.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE session_id = ?1 AND id > ?2 AND type = ?3
                     AND json_extract(data, '$.tool') = ?4)",
                params![session, since, EVENT_TOOL, tool],
                |row| row.get(0),
            )?)
        };
        let stops: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND id > ?2 AND type IN (?3, ?4)
                 AND json_extract(data, '$.hook') = 'Stop'",
            params![session, since, EVENT_DENY, EVENT_WOULD_DENY],
            |row| row.get(0),
        )?;
        let reported: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE session_id = ?1 AND id > ?2 AND type = ?3
                 AND json_extract(data, '$.status') IN ('done', 'blocked'))",
            params![session, since, crate::EVENT_REPORT],
            |row| row.get(0),
        )?;
        let background: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE session_id = ?1 AND id > ?2 AND type = ?3
                 AND json_extract(data, '$.background') = 1)",
            params![session, since, EVENT_TOOL],
            |row| row.get(0),
        )?;
        Ok(TurnSoFar {
            background,
            reported,
            asked: used("AskUserQuestion")?,
            messaged: used("SendMessage")?,
            stops_denied: u32::try_from(stops).map_err(|_| StoreError::OutOfRange(stops))?,
        })
    }

    /// The newest open ask of a session.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn open_ask_of(&self, session: &str) -> Result<Option<AskId>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM asks WHERE session_id = ?1 AND state = 'open' ORDER BY id DESC LIMIT 1",
                params![session],
                |row| row.get(0),
            )
            .optional()?
            .map(AskId))
    }

    /// Whether the user allowed Fable for a task: its Fable ask was answered with the allow choice.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn fable_allowed(&self, task: TaskId) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM tasks t JOIN asks a ON a.id = t.fable_ask
                 WHERE t.id = ?1 AND a.state = 'answered' AND a.answer = ?2)",
            params![task.0, crate::FABLE_ALLOW],
            |row| row.get(0),
        )?)
    }
}
