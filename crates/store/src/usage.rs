//! Token usage read from Claude Code transcripts (`docs/usage.md`): one row per API response in `usage`, older days
//! summed in `usage_daily`, and how far each transcript was read in `usage_files`.
//!
//! The response id is the key, so a reread, a crash halfway or copies of one response change nothing. A row's task and
//! role are fixed when it is written, from the session row, so a later close or rename does not move old numbers.

use std::collections::BTreeMap;

use headwork_core::usage::{Tokens, UsageLine, usage_role};
use headwork_core::{Role, UsageGroup, UsageRole};
use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};
use serde::Serialize;

use crate::records::TaskId;
use crate::{NOW, Store, StoreError};

/// The event that records a helper `claude` a worker started with `helper_tab`, under the helper's session id.
///
/// Its transcript counts under the worker's task as role `helper`. The event log is the record, so no schema change is
/// needed.
pub const EVENT_HELPER_SESSION: &str = "helper-session";

/// The event the daily roll-up logs.
pub const EVENT_USAGE_ROLLED_UP: &str = "usage-rolled-up";

/// What one roll-up moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RolledUp {
    /// Responses before this UTC day were summed.
    pub before_day: String,
    /// Per-response rows summed and deleted.
    pub rows: usize,
    /// Daily rows written or added to.
    pub days: usize,
}

/// Whose tokens a row counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageTag {
    /// The task, none for the orchestrator and unmanaged sessions.
    pub task: Option<TaskId>,
    /// The role.
    pub role: UsageRole,
}

impl UsageTag {
    /// A session headwork did not start.
    pub const UNMANAGED: Self = Self {
        task: None,
        role: UsageRole::Unmanaged,
    };
}

/// How far a transcript was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMark {
    /// Bytes read, always the end of a whole line.
    pub read_to: u64,
    /// The file's size then.
    pub size: u64,
    /// Its modification time then, in milliseconds since the Unix epoch.
    pub mtime_ms: i64,
}

/// One transcript's newly read responses, written with its new mark in one transaction.
#[derive(Debug, Clone, Copy)]
pub struct UsageBatch<'a> {
    /// The transcript's path.
    pub path: &'a str,
    /// Its Claude session.
    pub session_id: &'a str,
    /// The subagent, for a subagent transcript.
    pub agent_id: Option<&'a str>,
    /// The subagent's type.
    pub agent_type: Option<&'a str>,
    /// Whose tokens they are.
    pub tag: UsageTag,
    /// The folder to record when a line names none.
    pub dir: &'a str,
    /// The responses.
    pub lines: &'a [UsageLine],
    /// The mark after them.
    pub mark: FileMark,
}

/// What `usage_totals` sums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageQuery {
    /// One total per value of this.
    pub by: UsageGroup,
    /// Only responses at or after this UTC instant; a summed day counts when it starts at or after it.
    pub since: Option<String>,
    /// Only these tasks.
    pub tasks: Option<Vec<TaskId>>,
    /// Break unmanaged sessions down like the others instead of one `unmanaged` line.
    pub unmanaged: bool,
}

/// One group's total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageTotal {
    /// The group's value.
    pub key: String,
    /// API responses.
    pub responses: u64,
    /// The numbers.
    #[serde(flatten)]
    pub tokens: Tokens,
    /// The numbers by the model that spent them, for the estimate.
    #[serde(skip)]
    pub models: ByModel,
}

/// Tokens by the model that spent them.
pub type ByModel = BTreeMap<String, Tokens>;

impl UsageTotal {
    /// An empty total named `key`.
    #[must_use]
    pub fn named(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            responses: 0,
            tokens: Tokens::default(),
            models: ByModel::new(),
        }
    }

    /// Adds another total's numbers.
    pub fn add(&mut self, other: &Self) {
        self.responses = self.responses.saturating_add(other.responses);
        self.tokens += other.tokens;
        for (model, tokens) in &other.models {
            *self.models.entry(model.clone()).or_default() += *tokens;
        }
    }
}

/// Every task's tokens by model, its own and with its parts'.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskTokens {
    own: BTreeMap<TaskId, ByModel>,
    tree: BTreeMap<TaskId, ByModel>,
}

impl TaskTokens {
    /// The task's own tokens by model.
    #[must_use]
    pub fn own(&self, task: TaskId) -> ByModel {
        self.own.get(&task).cloned().unwrap_or_default()
    }

    /// The task's tokens by model, with its parts'.
    #[must_use]
    pub fn tree(&self, task: TaskId) -> ByModel {
        self.tree.get(&task).cloned().unwrap_or_default()
    }
}

/// The sum of every model's tokens.
#[must_use]
pub fn all_models(by_model: &ByModel) -> Tokens {
    let mut sum = Tokens::default();
    for tokens in by_model.values() {
        sum += *tokens;
    }
    sum
}

fn stored(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

fn read_count(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::OutOfRange(value))
}

fn insert_line(
    tx: &Transaction<'_>,
    batch: &UsageBatch<'_>,
    line: &UsageLine,
) -> Result<usize, StoreError> {
    let tokens = line.tokens;
    Ok(tx.execute(
        "INSERT INTO usage (message_id, session_id, agent_id, agent_type, task_id, role, dir, at, model, effort, speed,
             service_tier, input, cache_write_5m, cache_write_1h, cache_read, output, thinking, web_searches, web_fetches)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT (message_id) DO NOTHING",
        params![
            line.message_id,
            batch.session_id,
            batch.agent_id,
            batch.agent_type,
            batch.tag.task.map(|task| task.0),
            batch.tag.role.as_str(),
            line.cwd.as_deref().unwrap_or(batch.dir),
            line.at,
            line.model,
            line.effort,
            line.speed,
            line.service_tier,
            stored(tokens.input),
            stored(tokens.cache_write_5m),
            stored(tokens.cache_write_1h),
            stored(tokens.cache_read),
            stored(tokens.output),
            stored(tokens.thinking),
            stored(tokens.web_searches),
            stored(tokens.web_fetches),
        ],
    )?)
}

const TOKEN_SUMS: &str = "SUM(r.responses), SUM(r.input), SUM(r.cache_write_5m), SUM(r.cache_write_1h), SUM(r.cache_read),
     SUM(r.output), SUM(r.thinking), SUM(r.web_searches), SUM(r.web_fetches)";

const ROWS: &str = "WITH r AS (
         SELECT at, session_id, COALESCE(agent_type, '') AS agent_type, task_id, role, dir, model,
                COALESCE(effort, '') AS effort, 1 AS responses, input, cache_write_5m, cache_write_1h, cache_read, output,
                thinking, web_searches, web_fetches
         FROM usage
         UNION ALL
         SELECT day || 'T00:00:00.000Z', session_id, agent_type, task_id, role, dir, model, effort, responses, input,
                cache_write_5m, cache_write_1h, cache_read, output, thinking, web_searches, web_fetches
         FROM usage_daily
     )";

const fn group_key(by: UsageGroup) -> (&'static str, bool) {
    match by {
        UsageGroup::Project => ("t.project", true),
        UsageGroup::Task => ("t.project || '-' || t.slug", true),
        UsageGroup::Kind => ("t.kind", true),
        UsageGroup::Model => ("r.model", false),
        UsageGroup::Effort => ("CASE r.effort WHEN '' THEN '-' ELSE r.effort END", false),
        UsageGroup::Role => ("r.role", false),
        UsageGroup::Agent => (
            "CASE r.agent_type WHEN '' THEN 'main' ELSE r.agent_type END",
            false,
        ),
        UsageGroup::Folder => ("r.dir", false),
        UsageGroup::Day => ("substr(r.at, 1, 10)", false),
        UsageGroup::Session => ("r.session_id", false),
    }
}

fn read_tokens(row: &rusqlite::Row<'_>, from: usize) -> rusqlite::Result<[i64; 9]> {
    let mut sums = [0; 9];
    for (offset, sum) in sums.iter_mut().enumerate() {
        *sum = row.get::<_, Option<i64>>(from + offset)?.unwrap_or(0);
    }
    Ok(sums)
}

fn tokens_of(sums: [i64; 9]) -> Result<(u64, Tokens), StoreError> {
    let [responses, counts @ ..] = sums;
    let [
        input,
        write_short,
        write_long,
        cache_read,
        output,
        thinking,
        searches,
        fetches,
    ] = counts.map(read_count);
    Ok((
        read_count(responses)?,
        Tokens {
            input: input?,
            cache_write_5m: write_short?,
            cache_write_1h: write_long?,
            cache_read: cache_read?,
            output: output?,
            thinking: thinking?,
            web_searches: searches?,
            web_fetches: fetches?,
        },
    ))
}

impl Store {
    /// Whose tokens a session's rows count: its task and role when the store knows the session, else none.
    ///
    /// # Errors
    ///
    /// Returns a read error, or [`StoreError::UnknownWord`] for a stored role this build does not know.
    pub fn usage_tag(&self, session: &str) -> Result<Option<UsageTag>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT s.role, s.task_id, COALESCE(t.lead, 0), t.parent_id IS NOT NULL
             FROM sessions s LEFT JOIN tasks t ON t.id = s.task_id WHERE s.id = ?1",
        )?;
        let mut rows = statement.query(params![session])?;
        let Some(row) = rows.next()? else {
            return self.helper_tag(session);
        };
        let role: Role = row.get::<_, String>(0)?.parse()?;
        let task: Option<i64> = row.get(1)?;
        let lead: bool = row.get(2)?;
        let part: bool = row.get(3)?;
        Ok(Some(UsageTag {
            task: task.map(TaskId),
            role: usage_role(role, lead, part),
        }))
    }

    fn helper_tag(&self, session: &str) -> Result<Option<UsageTag>, StoreError> {
        let task: Option<i64> = self
            .conn
            .prepare(
                "SELECT task_id FROM events WHERE type = ?1 AND session_id = ?2 AND task_id IS NOT NULL
                 ORDER BY id DESC LIMIT 1",
            )?
            .query_row(params![EVENT_HELPER_SESSION, session], |row| row.get(0))
            .optional()?;
        Ok(task.map(|task| UsageTag {
            task: Some(TaskId(task)),
            role: UsageRole::Helper,
        }))
    }

    /// Records a helper `claude` a worker's `helper_tab` starts under `session`, so its transcript counts under the
    /// task as role `helper`.
    ///
    /// # Errors
    ///
    /// Returns a write error.
    pub fn record_helper_session(
        &self,
        task: TaskId,
        session: &str,
        argv: &[String],
        actor: headwork_core::Actor,
    ) -> Result<(), StoreError> {
        crate::queries::insert_event(
            &self.conn,
            actor,
            Some(task),
            Some(session),
            EVENT_HELPER_SESSION,
            &serde_json::json!({ "argv": argv }),
        )
    }

    /// The last UTC day the roll-up summed: responses on or before it live only in `usage_daily`, so a reread skips
    /// them.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn usage_rolled_to(&self) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT MAX(day) FROM usage_daily", [], |row| row.get(0))?)
    }

    /// Sums every per-response row before the UTC day `before_day` (`YYYY-MM-DD`) into `usage_daily`, per day,
    /// session, subagent type, model and effort, and deletes them, in one transaction, logging `usage-rolled-up`.
    /// Totals do not change, since every query reads both tables. None when nothing was old enough.
    ///
    /// # Errors
    ///
    /// Returns a write error; nothing is moved then.
    pub fn roll_up_usage(
        &mut self,
        before_day: &str,
        actor: headwork_core::Actor,
    ) -> Result<Option<RolledUp>, StoreError> {
        let tx = self.conn.transaction()?;
        let days = tx.execute(
            "INSERT INTO usage_daily (day, session_id, agent_type, task_id, role, dir, model, effort, responses, input,
                 cache_write_5m, cache_write_1h, cache_read, output, thinking, web_searches, web_fetches)
             SELECT substr(at, 1, 10), session_id, COALESCE(agent_type, ''), MAX(task_id), MAX(role), MAX(dir), model,
                    COALESCE(effort, ''), COUNT(*), SUM(input), SUM(cache_write_5m), SUM(cache_write_1h),
                    SUM(cache_read), SUM(output), SUM(thinking), SUM(web_searches), SUM(web_fetches)
             FROM usage WHERE substr(at, 1, 10) < ?1
             GROUP BY substr(at, 1, 10), session_id, COALESCE(agent_type, ''), model, COALESCE(effort, '')
             ON CONFLICT (day, session_id, agent_type, model, effort) DO UPDATE SET
                 responses = responses + excluded.responses, input = input + excluded.input,
                 cache_write_5m = cache_write_5m + excluded.cache_write_5m,
                 cache_write_1h = cache_write_1h + excluded.cache_write_1h,
                 cache_read = cache_read + excluded.cache_read, output = output + excluded.output,
                 thinking = thinking + excluded.thinking, web_searches = web_searches + excluded.web_searches,
                 web_fetches = web_fetches + excluded.web_fetches",
            params![before_day],
        )?;
        let rows = tx.execute(
            "DELETE FROM usage WHERE substr(at, 1, 10) < ?1",
            params![before_day],
        )?;
        if rows == 0 {
            return Ok(None);
        }
        let rolled = RolledUp {
            before_day: before_day.to_owned(),
            rows,
            days,
        };
        crate::queries::insert_event(
            &tx,
            actor,
            None,
            None,
            EVENT_USAGE_ROLLED_UP,
            &serde_json::to_value(&rolled)?,
        )?;
        tx.commit()?;
        Ok(Some(rolled))
    }

    /// How far a transcript was read, none when it never was.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn usage_file(&self, path: &str) -> Result<Option<FileMark>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT read_to, size, mtime_ms FROM usage_files WHERE path = ?1")?;
        let mut rows = statement.query(params![path])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(FileMark {
            read_to: read_count(row.get(0)?)?,
            size: read_count(row.get(1)?)?,
            mtime_ms: row.get(2)?,
        }))
    }

    /// Writes a transcript's newly read responses and its new mark in one transaction; returns how many responses were
    /// new. A response already stored is left as it is.
    ///
    /// # Errors
    ///
    /// Returns a write error; nothing is written then.
    pub fn record_usage(&mut self, batch: &UsageBatch<'_>) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0;
        for line in batch.lines {
            inserted += insert_line(&tx, batch, line)?;
        }
        tx.execute(
            &format!(
                "INSERT INTO usage_files (path, session_id, agent_id, read_to, size, mtime_ms, read_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, {NOW})
                 ON CONFLICT (path) DO UPDATE SET read_to = excluded.read_to, size = excluded.size,
                     mtime_ms = excluded.mtime_ms, read_at = excluded.read_at"
            ),
            params![
                batch.path,
                batch.session_id,
                batch.agent_id,
                stored(batch.mark.read_to),
                stored(batch.mark.size),
                batch.mark.mtime_ms,
            ],
        )?;
        tx.commit()?;
        Ok(inserted)
    }

    /// Tags the unmanaged rows of sessions the store has learned since (`headwork import` adopting them) with their
    /// task and role; returns how many rows moved.
    ///
    /// # Errors
    ///
    /// Returns a read or write error.
    pub fn retag_usage(&mut self) -> Result<usize, StoreError> {
        let sessions: Vec<String> = self
            .conn
            .prepare(
                "SELECT DISTINCT u.session_id FROM usage u
                 WHERE u.role = 'unmanaged' AND (EXISTS (SELECT 1 FROM sessions s WHERE s.id = u.session_id)
                     OR EXISTS (SELECT 1 FROM events e WHERE e.type = ?1 AND e.session_id = u.session_id))",
            )?
            .query_map(params![EVENT_HELPER_SESSION], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        let mut moved = 0;
        for session in sessions {
            let Some(tag) = self.usage_tag(&session)? else {
                continue;
            };
            let tx = self.conn.transaction()?;
            for table in ["usage", "usage_daily"] {
                moved += tx.execute(
                    &format!("UPDATE {table} SET task_id = ?1, role = ?2 WHERE session_id = ?3 AND role = 'unmanaged'"),
                    params![tag.task.map(|task| task.0), tag.role.as_str(), session],
                )?;
            }
            tx.commit()?;
        }
        Ok(moved)
    }

    /// Every task's token total, across the per-response rows and the summed days.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn usage_task_tokens(&self) -> Result<TaskTokens, StoreError> {
        let rows = self
            .conn
            .prepare(&format!(
                "{ROWS}
                 SELECT r.task_id, t.parent_id, r.model, {TOKEN_SUMS}
                 FROM r JOIN tasks t ON t.id = r.task_id
                 GROUP BY r.task_id, r.model"
            ))?
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                    read_tokens(row, 3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut tokens = TaskTokens::default();
        for (task, parent, model, sums) in rows {
            let (_, sum) = tokens_of(sums)?;
            *tokens
                .own
                .entry(TaskId(task))
                .or_default()
                .entry(model.clone())
                .or_default() += sum;
            for tree in [Some(task), parent].into_iter().flatten() {
                *tokens
                    .tree
                    .entry(TaskId(tree))
                    .or_default()
                    .entry(model.clone())
                    .or_default() += sum;
            }
        }
        Ok(tokens)
    }

    /// Token totals per group, across the per-response rows and the summed days alike, largest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn usage_totals(&self, query: &UsageQuery) -> Result<Vec<UsageTotal>, StoreError> {
        let (key, by_task) = group_key(query.by);
        let unmanaged_key = if query.unmanaged {
            if by_task { "r.dir" } else { key }
        } else {
            "'unmanaged'"
        };
        let no_task_key = if by_task { "r.role" } else { key };
        let mut filters = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(since) = &query.since {
            values.push(since.clone().into());
            filters.push(format!("r.at >= ?{}", values.len()));
        }
        if let Some(tasks) = &query.tasks {
            let mut marks = Vec::new();
            for task in tasks {
                values.push(task.0.into());
                marks.push(format!("?{}", values.len()));
            }
            filters.push(format!("r.task_id IN ({})", marks.join(", ")));
        }
        let filter = if filters.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", filters.join(" AND "))
        };
        let sql = format!(
            "{ROWS}
             SELECT CASE WHEN r.role = 'unmanaged' THEN {unmanaged_key}
                         WHEN r.task_id IS NULL THEN {no_task_key}
                         ELSE {key} END AS k,
                    r.model,
                    {TOKEN_SUMS}
             FROM r LEFT JOIN tasks t ON t.id = r.task_id
             {filter}
             GROUP BY k, r.model"
        );
        let rows = self
            .conn
            .prepare(&sql)?
            .query_map(params_from_iter(values), |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    read_tokens(row, 2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut totals: BTreeMap<String, UsageTotal> = BTreeMap::new();
        for (key, model, sums) in rows {
            let (responses, tokens) = tokens_of(sums)?;
            let key = key.unwrap_or_else(|| "-".to_owned());
            let total = totals.entry(key.clone()).or_insert_with(|| UsageTotal {
                key,
                responses: 0,
                tokens: Tokens::default(),
                models: ByModel::new(),
            });
            total.responses = total.responses.saturating_add(responses);
            total.tokens += tokens;
            *total.models.entry(model).or_default() += tokens;
        }
        let mut totals: Vec<UsageTotal> = totals.into_values().collect();
        totals.sort_by(|a, b| {
            b.tokens
                .total()
                .cmp(&a.tokens.total())
                .then_with(|| a.key.cmp(&b.key))
        });
        Ok(totals)
    }
}
