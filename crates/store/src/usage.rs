//! Token usage read from Claude Code transcripts (`docs/usage.md`): one row per API response in `usage`, older days
//! summed in `usage_daily`, and how far each transcript was read in `usage_files`.
//!
//! The response id is the key, so a reread, a crash halfway or copies of one response change nothing. A row's task and
//! role are fixed when it is written, from the session row, so a later close or rename does not move old numbers.

use headwork_core::usage::{Tokens, UsageLine, usage_role};
use headwork_core::{Role, UsageGroup, UsageRole};
use rusqlite::{Transaction, params, params_from_iter};
use serde::Serialize;

use crate::records::TaskId;
use crate::{NOW, Store, StoreError};

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
            return Ok(None);
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
                "SELECT DISTINCT u.session_id FROM usage u JOIN sessions s ON s.id = u.session_id
                 WHERE u.role = 'unmanaged'",
            )?
            .query_map([], |row| row.get(0))?
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
                    {TOKEN_SUMS}
             FROM r LEFT JOIN tasks t ON t.id = r.task_id
             {filter}
             GROUP BY k
             ORDER BY SUM(r.input + r.cache_write_5m + r.cache_write_1h + r.cache_read + r.output) DESC, k"
        );
        let rows = self
            .conn
            .prepare(&sql)?
            .query_map(params_from_iter(values), |row| {
                Ok((row.get::<_, Option<String>>(0)?, read_tokens(row, 1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(key, sums)| {
                let (responses, tokens) = tokens_of(sums)?;
                Ok(UsageTotal {
                    key: key.unwrap_or_else(|| "-".to_owned()),
                    responses,
                    tokens,
                })
            })
            .collect()
    }
}
