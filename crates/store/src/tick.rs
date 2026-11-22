//! What the tick reads and remembers: session clocks, asks that wait too long, pull requests, findings it already
//! reported, screens and file sizes it compared.

use std::collections::BTreeMap;

use headwork_core::{Actor, AskKind, AskState, FindingId};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::queries::insert_event;
use crate::records::{AskId, TaskId};
use crate::{NOW, Store, StoreError};

const AGE: &str = "CAST(strftime('%s', 'now') AS INTEGER) - CAST(strftime('%s', ";

fn age_of(column: &str) -> String {
    format!("{AGE}{column}) AS INTEGER)")
}

fn seconds(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// How old a session is and how long it has been quiet, as SQLite's clock sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SessionClock {
    /// Seconds since the session was recorded.
    pub started_age_s: u64,
    /// Seconds since its last hook event, none when there was none.
    pub idle_age_s: Option<u64>,
    /// A tool call is running.
    pub tool_in_flight: bool,
    /// The rules version it last received.
    pub rules_seen: i64,
}

/// An ask that waits on the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitingAsk {
    /// The row id.
    pub id: AskId,
    /// The task, if any.
    pub task_id: Option<TaskId>,
    /// The question.
    pub question: String,
    /// Seconds since it was created.
    pub age_s: u64,
    /// Seconds since the user was last reminded, none when never.
    pub since_notified_s: Option<u64>,
}

/// A pull request of a task, as the git host reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSeen<'a> {
    /// `owner/name`.
    pub repo: &'a str,
    /// The number.
    pub number: i64,
    /// The URL.
    pub url: &'a str,
    /// `open`, `merged` or `closed`.
    pub state: &'a str,
}

/// An ask the tick put to the user about a task, found by its topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicAsk {
    /// The row id.
    pub id: AskId,
    /// Where it is.
    pub state: AskState,
    /// The answer.
    pub answer: Option<String>,
}

/// One ask by its id, with what decides who may answer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAsk {
    /// The row id.
    pub id: AskId,
    /// The task, if any.
    pub task: Option<TaskId>,
    /// The session whose dialog asked it; none for an ask the tick raised.
    pub session: Option<String>,
    /// The topic of a tick ask, such as `worktree`.
    pub topic: Option<String>,
    /// The question.
    pub question: String,
    /// The options, in order.
    pub options: Vec<String>,
    /// Where it is.
    pub state: AskState,
}

impl Store {
    /// Whether the task was ever dropped, by the orchestrator or the user.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn was_dropped(&self, task: TaskId) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE task_id = ?1 AND type = 'transition'
                            AND json_extract(data, '$.to') = 'dropped')",
            params![task.0],
            |row| row.get(0),
        )?)
    }

    /// The ask with this id.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn ask(&self, id: AskId) -> Result<Option<StoredAsk>, StoreError> {
        type Raw = (
            Option<i64>,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
        );
        let raw: Option<Raw> = self
            .conn
            .query_row(
                "SELECT task_id, session_id, topic, question, options, state FROM asks WHERE id = ?1",
                params![id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .optional()?;
        raw.map(|(task, session, topic, question, options, state)| {
            Ok(StoredAsk {
                id,
                task: task.map(TaskId),
                session,
                topic,
                question,
                options: serde_json::from_str(&options)?,
                state: state.parse()?,
            })
        })
        .transpose()
    }

    /// A session's clocks.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn session_clock(&self, session: &str) -> Result<SessionClock, StoreError> {
        let sql = format!(
            "SELECT {}, CASE WHEN last_event_at IS NULL THEN NULL ELSE {} END, tool_in_flight IS NOT NULL, rules_seen
             FROM sessions WHERE id = ?1",
            age_of("started_at"),
            age_of("last_event_at"),
        );
        let (started, idle, tool, seen): (i64, Option<i64>, bool, i64) =
            self.conn.query_row(&sql, params![session], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
        Ok(SessionClock {
            started_age_s: seconds(started),
            idle_age_s: idle.map(seconds),
            tool_in_flight: tool,
            rules_seen: seen,
        })
    }

    /// Seconds since the task last changed state.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn task_age_s(&self, task: TaskId) -> Result<u64, StoreError> {
        let sql = format!("SELECT {} FROM tasks WHERE id = ?1", age_of("updated_at"));
        let age: i64 = self
            .conn
            .query_row(&sql, params![task.0], |row| row.get(0))?;
        Ok(seconds(age))
    }

    /// The host surfaces of every session of a task that is not closed.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn owned_surfaces(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT s.host_surface FROM sessions s JOIN tasks t ON t.id = s.task_id
             WHERE s.host_surface IS NOT NULL AND t.state <> 'closed'",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Whether any session of the task sent a message to another session.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn report_sent(&self, task: TaskId) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE task_id = ?1 AND type = 'tool-used'
                 AND json_extract(data, '$.tool') = 'SendMessage')",
            params![task.0],
            |row| row.get(0),
        )?)
    }

    /// Whether the user gave the task's lead its go to close: an answered ask headed `Close`, recorded by the hook as a
    /// `close-go` event.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn close_go_given(&self, task: TaskId) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE task_id = ?1 AND type = 'close-go')",
            params![task.0],
            |row| row.get(0),
        )?)
    }

    /// Whether an ask of the task still waits on the user.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn has_waiting_ask(&self, task: TaskId) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM asks WHERE task_id = ?1 AND state IN ('pending', 'open'))",
            params![task.0],
            |row| row.get(0),
        )?)
    }

    /// Supersedes every ask still waiting on a closed task, which no session can act on any more, and returns how many
    /// tasks had one.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn supersede_asks_of_closed(&mut self, actor: Actor) -> Result<usize, StoreError> {
        let tasks: Vec<i64> = {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT a.task_id FROM asks a JOIN tasks t ON t.id = a.task_id \
                 WHERE t.state = 'closed' AND a.state IN ('pending', 'open') ORDER BY a.task_id",
            )?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for task in &tasks {
            self.supersede_asks(TaskId(*task), actor)?;
        }
        Ok(tasks.len())
    }

    /// Open asks of running sessions older than `older_than_s`, oldest first, with how long ago the user was
    /// reminded.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn overdue_asks(&self, older_than_s: u64) -> Result<Vec<WaitingAsk>, StoreError> {
        let limit = i64::try_from(older_than_s).unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT id, task_id, question, {}, CASE WHEN notified_at IS NULL THEN NULL ELSE {} END
             FROM asks WHERE state = 'open' AND {} >= ?1 ORDER BY id",
            age_of("created_at"),
            age_of("notified_at"),
            age_of("created_at"),
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (id, task, question, age, since) = row?;
            Ok(WaitingAsk {
                id: AskId(id),
                task_id: task.map(TaskId),
                question,
                age_s: seconds(age),
                since_notified_s: since.map(seconds),
            })
        })
        .collect()
    }

    /// Records that the user was reminded of an ask.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn mark_ask_notified(&self, ask: AskId) -> Result<(), StoreError> {
        self.conn.execute(
            &format!("UPDATE asks SET notified_at = {NOW} WHERE id = ?1"),
            params![ask.0],
        )?;
        Ok(())
    }

    /// Puts a question of the tick's own to the user, found again by its topic. The ask waits in `pending` until
    /// whoever talks to the user puts it.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn open_topic_ask(
        &mut self,
        task: TaskId,
        topic: &str,
        question: &str,
        options: &[String],
        actor: Actor,
    ) -> Result<AskId, StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO asks (task_id, kind, topic, question, options, state, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', {NOW})"
            ),
            params![
                task.0,
                AskKind::Tool.as_str(),
                topic,
                question,
                serde_json::to_string(options)?
            ],
        )?;
        let id = AskId(tx.last_insert_rowid());
        let data = serde_json::json!({ "ask": id.0, "topic": topic, "question": question });
        insert_event(&tx, actor, Some(task), None, "ask-opened", &data)?;
        tx.commit()?;
        Ok(id)
    }

    /// The newest ask of a task with this topic.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn topic_ask(&self, task: TaskId, topic: &str) -> Result<Option<TopicAsk>, StoreError> {
        let raw: Option<(i64, String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id, state, answer FROM asks WHERE task_id = ?1 AND topic = ?2 ORDER BY id DESC LIMIT 1",
                params![task.0, topic],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        raw.map(|(id, state, answer)| {
            Ok(TopicAsk {
                id: AskId(id),
                state: state.parse()?,
                answer,
            })
        })
        .transpose()
    }

    /// Supersedes one ask that still waits, as when the question went away by itself.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn supersede_ask(
        &mut self,
        ask: AskId,
        task: TaskId,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE asks SET state = 'superseded' WHERE id = ?1 AND state IN ('pending', 'open')",
            params![ask.0],
        )?;
        if changed > 0 {
            insert_event(
                &tx,
                actor,
                Some(task),
                None,
                "ask-superseded",
                &serde_json::json!({ "ask": ask.0 }),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Renames the topic of an ask, so a handled answer is not handled again.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_ask_topic(&self, ask: AskId, topic: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE asks SET topic = ?1 WHERE id = ?2",
            params![topic, ask.0],
        )?;
        Ok(())
    }

    /// Records a task's pull request, or updates its state. A row is the task's and the URL's: another task that names
    /// the same pull request gets a row of its own.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn record_pr(
        &mut self,
        task: TaskId,
        pr: &PrSeen<'_>,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let before: Option<String> = tx
            .query_row(
                "SELECT state FROM prs WHERE task_id = ?1 AND url = ?2",
                params![task.0, pr.url],
                |row| row.get(0),
            )
            .optional()?;
        tx.execute(
            &format!(
                "INSERT INTO prs (task_id, repo, number, url, state, checked_at) VALUES (?1, ?2, ?3, ?4, ?5, {NOW})
                 ON CONFLICT(task_id, url) DO UPDATE SET state = excluded.state, repo = excluded.repo,
                     number = excluded.number, checked_at = {NOW}"
            ),
            params![task.0, pr.repo, pr.number, pr.url, pr.state],
        )?;
        if before.as_deref() != Some(pr.state) {
            let data = serde_json::json!({
                "repo": pr.repo, "number": pr.number, "url": pr.url, "from": before, "to": pr.state,
            });
            insert_event(&tx, actor, Some(task), None, "pr-state", &data)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Notes a finding the first time it is seen. Returns whether it is new: a finding already noted and not
    /// resolved returns `false` and writes nothing, so a standing condition is logged once.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn raise_finding(
        &mut self,
        key: &str,
        rule: FindingId,
        task: Option<TaskId>,
        detail: &serde_json::Value,
        actor: Actor,
    ) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let inserted = tx.execute(
            &format!(
                "INSERT INTO findings (key, rule, task_id, detail, first_at) VALUES (?1, ?2, ?3, ?4, {NOW})
                 ON CONFLICT DO NOTHING"
            ),
            params![
                key,
                rule.as_str(),
                task.map(|id| id.0),
                serde_json::to_string(detail)?
            ],
        )?;
        if inserted > 0 {
            let data = serde_json::json!({ "rule": rule, "key": key, "detail": detail });
            insert_event(&tx, actor, task, None, "finding", &data)?;
        }
        tx.commit()?;
        Ok(inserted > 0)
    }

    /// Resolves the open findings of a rule whose key is not in `still_open`, logging each. Returns the keys.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn resolve_findings(
        &mut self,
        rule: FindingId,
        still_open: &[String],
        actor: Actor,
    ) -> Result<Vec<String>, StoreError> {
        let tx = self.conn.transaction()?;
        let open: Vec<(String, Option<i64>)> = {
            let mut statement = tx.prepare(
                "SELECT key, task_id FROM findings WHERE rule = ?1 AND resolved_at IS NULL ORDER BY id",
            )?;
            let rows = statement
                .query_map(params![rule.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut resolved = Vec::new();
        for (key, task) in open {
            if still_open.contains(&key) {
                continue;
            }
            tx.execute(
                &format!(
                    "UPDATE findings SET resolved_at = {NOW} WHERE key = ?1 AND resolved_at IS NULL"
                ),
                params![key],
            )?;
            let data = serde_json::json!({ "rule": rule, "key": key });
            insert_event(&tx, actor, task.map(TaskId), None, "finding-cleared", &data)?;
            resolved.push(key);
        }
        tx.commit()?;
        Ok(resolved)
    }

    /// The hash of the screen the tick last looked at for a session.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn screen_sample(&self, session: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT hash FROM screen_samples WHERE session_id = ?1",
                params![session],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Remembers the hash of a session's screen.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn set_screen_sample(&self, session: &str, hash: &str) -> Result<(), StoreError> {
        self.conn.execute(
            &format!(
                "INSERT INTO screen_samples (session_id, hash, at) VALUES (?1, ?2, {NOW})
                 ON CONFLICT(session_id) DO UPDATE SET hash = excluded.hash, at = {NOW}"
            ),
            params![session, hash],
        )?;
        Ok(())
    }

    /// Forgets a session's screen, once it moves again.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn clear_screen_sample(&self, session: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM screen_samples WHERE session_id = ?1",
            params![session],
        )?;
        Ok(())
    }

    /// The file sizes of the last tick.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn file_samples(&self) -> Result<BTreeMap<String, u64>, StoreError> {
        let mut statement = self.conn.prepare("SELECT path, size FROM file_samples")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.map(|row| {
            row.map(|(path, size)| (path, seconds(size)))
                .map_err(Into::into)
        })
        .collect()
    }

    /// Replaces the remembered file sizes.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn replace_file_samples(
        &mut self,
        samples: &BTreeMap<String, u64>,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM file_samples", [])?;
        for (path, size) in samples {
            let size = i64::try_from(*size).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO file_samples (path, size) VALUES (?1, ?2)",
                params![path, size],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Seconds since the last finished tick, `None` when no tick ever ran.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn last_tick_age_s(&self) -> Result<Option<u64>, StoreError> {
        let sql = format!(
            "SELECT {} FROM machine WHERE id = 1",
            age_of("last_tick_at")
        );
        let age: Option<i64> = self.conn.query_row(&sql, [], |row| row.get(0))?;
        Ok(age.map(seconds))
    }

    /// Records a finished tick; returns whether a tick ran before.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn record_tick(&self) -> Result<bool, StoreError> {
        let before: Option<String> =
            self.conn
                .query_row("SELECT last_tick_at FROM machine WHERE id = 1", [], |row| {
                    row.get(0)
                })?;
        self.conn.execute(
            &format!("UPDATE machine SET last_tick_at = {NOW} WHERE id = 1"),
            [],
        )?;
        Ok(before.is_some())
    }

    /// The sessions of tasks in a running state whose rules version is older than the machine's.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn sessions_behind_rules(&self) -> Result<Vec<(TaskId, String, i64)>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT t.id, s.id, s.rules_seen FROM sessions s JOIN tasks t ON t.id = s.task_id
             JOIN machine m ON m.id = 1
             WHERE s.state = 'live' AND t.state IN ('working', 'needs_user', 'in_review', 'merged')
                 AND s.rules_seen < m.rules_version
             ORDER BY t.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((TaskId(row.get(0)?), row.get(1)?, row.get(2)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
