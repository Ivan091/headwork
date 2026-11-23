//! What the agent graph is drawn from (`docs/dashboard.md`): one read of every row and event the graph needs, handed to
//! [`headwork_core::graph::build`]. The event types it reads are the constants their writers use.

use std::str::FromStr;

use headwork_core::graph::{
    AgentFact, AskFact, Change, Facts, FindingFact, HelperFact, JobFact, MessageFact, PrFact,
    ReceivedFact, ReportFact, ReviewFact, ReviewFindingFact, ReviewNodeFact, SessionFact, Spend,
    TaskFact,
};
use headwork_core::review::Level;
use headwork_core::usage::Tokens;
use headwork_core::{Actor, UnknownWord};
use rusqlite::{Connection, Row, params};
use serde::Serialize;

use crate::hooks::{EVENT_ASK_SUPERSEDED, EVENT_PROMPT, EVENT_TOOL};
use crate::park::{EVENT_JOB_FORGOTTEN, EVENT_JOB_STARTED, EVENT_JOB_STATE};
use crate::queries::{EVENT_TASK_CREATED, EVENT_TRANSITION};
use crate::reports::EVENT_REPORT;
use crate::tick::EVENT_PR_STATE;
use crate::usage::EVENT_HELPER_SESSION;
use crate::{Store, StoreError};

/// One logged event as the dashboard's live view and replay see it: what happened and to whom, never its data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventMark {
    /// The row id: the log's order.
    pub id: i64,
    /// When.
    pub at: String,
    /// The event type.
    #[serde(rename = "type")]
    pub kind: String,
    /// The task's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// The span of the event log, for the replay slider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventRange {
    /// The first event's time.
    pub first_at: Option<String>,
    /// The newest event's time.
    pub last_at: Option<String>,
    /// The newest event's id; 0 for an empty log.
    pub last_id: i64,
}

impl Store {
    /// The span of the event log.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn event_range(&self) -> Result<EventRange, StoreError> {
        Ok(self.conn.query_row(
            "SELECT (SELECT at FROM events ORDER BY id LIMIT 1), (SELECT at FROM events ORDER BY id DESC LIMIT 1),
                 COALESCE((SELECT MAX(id) FROM events), 0)",
            [],
            |row| Ok(EventRange { first_at: row.get(0)?, last_at: row.get(1)?, last_id: row.get(2)? }),
        )?)
    }

    /// The newest event's id; 0 for an empty log.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn last_event_id(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| {
                row.get(0)
            })?)
    }

    /// The events after `id`, oldest first, at most `limit`.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn events_after(&self, id: i64, limit: u32) -> Result<Vec<EventMark>, StoreError> {
        rows(
            &self.conn,
            "SELECT e.id, e.at, e.type, t.project || '-' || t.slug, e.session_id
             FROM events e LEFT JOIN tasks t ON t.id = e.task_id
             WHERE e.id > ?1 ORDER BY e.id LIMIT ?2",
            params![id, limit],
            |row| {
                Ok(EventMark {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    kind: row.get(2)?,
                    task: row.get(3)?,
                    session: row.get(4)?,
                })
            },
        )
    }

    /// Every fact the graph is drawn from, read in one transaction so they come from one moment (inside a transaction
    /// already open on this connection, in that one). Events holding a word this build does not know are left out and
    /// listed in [`Facts::skipped`]; a table row with one fails the read, naming the row.
    ///
    /// # Errors
    ///
    /// Returns a read error, or [`StoreError::BadRow`].
    pub fn graph_facts(&self) -> Result<Facts, StoreError> {
        let conn = &self.conn;
        if !conn.is_autocommit() {
            return read_facts(conn);
        }
        conn.execute_batch("BEGIN DEFERRED")?;
        let facts = read_facts(conn);
        let end = conn.execute_batch(if facts.is_ok() { "COMMIT" } else { "ROLLBACK" });
        match (facts, end) {
            (Ok(facts), Ok(())) => Ok(facts),
            (Ok(_), Err(error)) => Err(error.into()),
            (Err(error), _) => Err(error),
        }
    }
}

fn read_facts(conn: &Connection) -> Result<Facts, StoreError> {
    let mut skipped = Vec::new();
    let transitions = changes(conn, &transitions_sql(), "task", &mut skipped)?;
    let pr_changes = changes(conn, &pr_changes_sql(), "pull request", &mut skipped)?;
    let job_changes = changes(conn, &job_changes_sql(), "job", &mut skipped)?;
    let review_changes = changes(conn, REVIEW_CHANGES, "review", &mut skipped)?;
    Ok(Facts {
        tasks: tasks(conn)?,
        sessions: sessions(conn)?,
        agents: rows(conn, AGENTS, [], agent)?,
        helpers: rows(conn, HELPERS, params![EVENT_HELPER_SESSION], helper)?,
        jobs: jobs(conn)?,
        prs: prs(conn)?,
        asks: asks(conn)?,
        reviews: reviews(conn)?,
        review_nodes: review_nodes(conn)?,
        messages: rows(conn, MESSAGES, params![EVENT_TOOL], message)?,
        received: rows(conn, RECEIVED, params![EVENT_PROMPT], received)?,
        reports: rows(conn, REPORTS, params![EVENT_REPORT], report)?,
        transitions,
        pr_changes,
        job_changes,
        review_changes,
        findings: rows(conn, FINDINGS, [], finding)?,
        review_findings: review_findings(conn)?,
        spend: spend(conn)?,
        skipped,
    })
}

const FINDINGS: &str = "SELECT task_id, rule, detail, first_at, resolved_at FROM findings
     WHERE task_id IS NOT NULL ORDER BY id";

fn finding(row: &Row<'_>) -> rusqlite::Result<FindingFact> {
    Ok(FindingFact {
        task: row.get(0)?,
        rule: row.get(1)?,
        detail: row.get(2)?,
        first_at: row.get(3)?,
        resolved_at: row.get(4)?,
    })
}

fn review_findings(conn: &Connection) -> Result<Vec<ReviewFindingFact>, StoreError> {
    type Raw = (
        i64,
        i64,
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let raw: Vec<Raw> = rows(
        conn,
        "SELECT id, review_id, round, node, severity, verdict, resolution FROM review_findings ORDER BY id",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    raw.into_iter()
        .map(|(id, review, round, node, severity, verdict, resolution)| {
            let at = || format!("review finding {id}");
            Ok(ReviewFindingFact {
                review,
                round,
                node,
                severity: word(&severity, at)?,
                verdict: verdict.map(|verdict| word(&verdict, at)).transpose()?,
                resolution: resolution
                    .map(|resolution| word(&resolution, at))
                    .transpose()?,
            })
        })
        .collect()
}

/// Every token kind summed: input, both cache writes, cache reads and output (`Tokens::total`).
const TOTAL: &str = "SUM(input + cache_write_5m + cache_write_1h + cache_read + output)";

fn sums<K: rusqlite::types::FromSql + Ord>(
    conn: &Connection,
    sql: &str,
) -> Result<std::collections::BTreeMap<K, u64>, StoreError> {
    let raw: Vec<(K, i64)> = rows(conn, sql, [], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(raw
        .into_iter()
        .map(|(key, total)| (key, u64::try_from(total).unwrap_or(0)))
        .collect())
}

fn spend(conn: &Connection) -> Result<Spend, StoreError> {
    Ok(Spend {
        tasks: sums(
            conn,
            &format!(
                "SELECT task_id, SUM(t) FROM (
                     SELECT task_id, {TOTAL} AS t FROM usage WHERE task_id IS NOT NULL GROUP BY task_id
                     UNION ALL
                     SELECT task_id, {TOTAL} AS t FROM usage_daily WHERE task_id IS NOT NULL GROUP BY task_id
                 ) GROUP BY task_id"
            ),
        )?,
        sessions: sums(
            conn,
            &format!(
                "SELECT session_id, SUM(t) FROM (
                     SELECT session_id, {TOTAL} AS t FROM usage WHERE agent_id IS NULL GROUP BY session_id
                     UNION ALL
                     SELECT session_id, {TOTAL} AS t FROM usage_daily WHERE agent_type = '' GROUP BY session_id
                 ) GROUP BY session_id"
            ),
        )?,
        agents: sums(conn, &format!("SELECT agent_id, {TOTAL} FROM usage WHERE agent_id IS NOT NULL GROUP BY agent_id"))?,
    })
}

/// Whose tokens to sum by type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendOf<'a> {
    /// A task's own.
    Task(i64),
    /// A session's own, its subagents' left out.
    Session(&'a str),
    /// A subagent's.
    Agent(&'a str),
}

impl Store {
    /// The tokens spent, by type, by a task, a session or a subagent.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn spend_of(&self, whose: SpendOf<'_>) -> Result<Tokens, StoreError> {
        let columns = "COALESCE(SUM(input), 0), COALESCE(SUM(cache_write_5m), 0), COALESCE(SUM(cache_write_1h), 0),
             COALESCE(SUM(cache_read), 0), COALESCE(SUM(output), 0), COALESCE(SUM(thinking), 0),
             COALESCE(SUM(web_searches), 0), COALESCE(SUM(web_fetches), 0)";
        let (live, daily, key): (&str, &str, String) = match whose {
            SpendOf::Task(task) => ("task_id = ?1", "task_id = ?1", task.to_string()),
            SpendOf::Session(session) => (
                "session_id = ?1 AND agent_id IS NULL",
                "session_id = ?1 AND agent_type = ''",
                session.to_owned(),
            ),
            SpendOf::Agent(agent) => ("agent_id = ?1", "0", agent.to_owned()),
        };
        let sql = format!(
            "SELECT {columns} FROM (
                 SELECT input, cache_write_5m, cache_write_1h, cache_read, output, thinking, web_searches, web_fetches
                 FROM usage WHERE {live}
                 UNION ALL
                 SELECT input, cache_write_5m, cache_write_1h, cache_read, output, thinking, web_searches, web_fetches
                 FROM usage_daily WHERE {daily}
             )"
        );
        let numbers: [i64; 8] = self.conn.query_row(&sql, params![key], |row| {
            Ok([
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ])
        })?;
        let [
            input,
            cache_writes_short,
            cache_writes_long,
            reads,
            output,
            thinking,
            searches,
            fetches,
        ] = numbers.map(|number| u64::try_from(number).unwrap_or(0));
        Ok(Tokens {
            input,
            cache_write_5m: cache_writes_short,
            cache_write_1h: cache_writes_long,
            cache_read: reads,
            output,
            thinking,
            web_searches: searches,
            web_fetches: fetches,
        })
    }
}

fn rows<P: rusqlite::Params, T>(
    conn: &Connection,
    sql: &str,
    params: P,
    read: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, StoreError> {
    Ok(conn
        .prepare(sql)?
        .query_map(params, read)?
        .collect::<Result<_, _>>()?)
}

/// Parses a stored word, naming the row it came from when it is unknown.
fn word<T: FromStr<Err = UnknownWord>>(
    text: &str,
    row: impl FnOnce() -> String,
) -> Result<T, StoreError> {
    text.parse()
        .map_err(|source| StoreError::BadRow { row: row(), source })
}

struct RawTask {
    id: i64,
    project: String,
    slug: String,
    kind: String,
    state: String,
    model: String,
    effort: Option<String>,
    parent: Option<i64>,
    depends_on: Option<i64>,
    lead: bool,
    actor: Option<String>,
    queued_at: String,
    last_report: Option<String>,
}

fn raw_task(row: &Row<'_>) -> rusqlite::Result<RawTask> {
    Ok(RawTask {
        id: row.get(0)?,
        project: row.get(1)?,
        slug: row.get(2)?,
        kind: row.get(3)?,
        state: row.get(4)?,
        model: row.get(5)?,
        effort: row.get(6)?,
        parent: row.get(7)?,
        depends_on: row.get(8)?,
        lead: row.get(9)?,
        actor: row.get(10)?,
        queued_at: row.get(11)?,
        last_report: row.get(12)?,
    })
}

fn tasks(conn: &Connection) -> Result<Vec<TaskFact>, StoreError> {
    let sql = format!(
        "SELECT t.id, t.project, t.slug, t.kind, t.state, t.model, t.effort, t.parent_id, t.depends_on, t.lead,
             (SELECT e.actor FROM events e WHERE e.task_id = t.id AND e.type = '{EVENT_TASK_CREATED}' ORDER BY e.id LIMIT 1),
             t.queued_at, t.last_report
         FROM tasks t ORDER BY t.id"
    );
    rows(conn, &sql, [], raw_task)?
        .into_iter()
        .map(|task| {
            let id = task.id;
            let at = || format!("task {id}");
            Ok(TaskFact {
                id,
                name: format!("{}-{}", task.project, task.slug),
                project: task.project,
                kind: task.kind,
                state: word(&task.state, at)?,
                model: word(&task.model, at)?,
                effort: task.effort.map(|effort| word(&effort, at)).transpose()?,
                parent: task.parent,
                depends_on: task.depends_on,
                lead: task.lead,
                created_by: task
                    .actor
                    .map_or(Ok(Actor::Orchestrator), |actor| word(&actor, at))?,
                queued_at: task.queued_at,
                last_report: task.last_report,
            })
        })
        .collect()
}

fn sessions(conn: &Connection) -> Result<Vec<SessionFact>, StoreError> {
    type Raw = (
        String,
        Option<i64>,
        String,
        String,
        String,
        String,
        Option<String>,
    );
    let raw: Vec<Raw> = rows(
        conn,
        "SELECT s.id, s.task_id, s.role, s.rc_name, s.state,
             CASE WHEN f.first_at IS NOT NULL AND f.first_at < s.started_at THEN f.first_at ELSE s.started_at END,
             s.ended_at
         FROM sessions s
         LEFT JOIN (SELECT session_id, MIN(at) AS first_at FROM events WHERE session_id IS NOT NULL GROUP BY session_id) f
             ON f.session_id = s.id
         ORDER BY s.started_at, s.id",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    )?;
    raw.into_iter()
        .map(|(id, task, role, rc_name, state, started_at, ended_at)| {
            let role = word(&role, || format!("session {id}"))?;
            let state = word(&state, || format!("session {id}"))?;
            Ok(SessionFact {
                id,
                task,
                role,
                rc_name,
                state,
                started_at,
                ended_at,
            })
        })
        .collect()
}

/// The logged state changes a query returns as `(key, from, to, at, event id)`. An event with a word this build does
/// not know is left out and named in `skipped`.
fn changes<S: FromStr<Err = UnknownWord>>(
    conn: &Connection,
    sql: &str,
    what: &str,
    skipped: &mut Vec<String>,
) -> Result<Vec<Change<S>>, StoreError> {
    type Raw = (i64, Option<String>, String, String, i64);
    let raw: Vec<Raw> = rows(conn, sql, [], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?;
    let mut found = Vec::with_capacity(raw.len());
    for (key, from, to, at, event) in raw {
        let parsed = from
            .map(|from| from.parse::<S>())
            .transpose()
            .and_then(|from| to.parse::<S>().map(|to| Change { key, from, to, at }));
        match parsed {
            Ok(change) => found.push(change),
            Err(error) => skipped.push(format!("event {event} ({what} {key}): {error}")),
        }
    }
    Ok(found)
}

fn transitions_sql() -> String {
    format!(
        "SELECT task_id, json_extract(data, '$.from'), json_extract(data, '$.to'), at, id FROM events
         WHERE type = '{EVENT_TRANSITION}' AND task_id IS NOT NULL AND json_valid(data)
             AND json_type(data, '$.from') = 'text' AND json_type(data, '$.to') = 'text'
         ORDER BY id"
    )
}

fn pr_changes_sql() -> String {
    format!(
        "SELECT p.id, json_extract(e.data, '$.from'), json_extract(e.data, '$.to'), e.at, e.id
         FROM events e JOIN prs p ON p.task_id = e.task_id AND p.url = json_extract(e.data, '$.url')
         WHERE e.type = '{EVENT_PR_STATE}' AND json_valid(e.data) AND json_type(e.data, '$.to') = 'text'
             AND json_type(e.data, '$.from') IN ('text', 'null')
         ORDER BY e.id"
    )
}

/// `job-state` events name the new state; `job-started` (started again after a park) means `running` and
/// `job-forgotten` (marked over) means `done`.
fn job_changes_sql() -> String {
    format!(
        "SELECT json_extract(data, '$.job'), NULL,
             CASE type WHEN '{EVENT_JOB_STARTED}' THEN 'running' WHEN '{EVENT_JOB_FORGOTTEN}' THEN 'done'
                 ELSE json_extract(data, '$.state') END,
             at, id
         FROM events
         WHERE type IN ('{EVENT_JOB_STATE}', '{EVENT_JOB_STARTED}', '{EVENT_JOB_FORGOTTEN}') AND json_valid(data)
             AND json_type(data, '$.job') = 'integer'
             AND (type <> '{EVENT_JOB_STATE}' OR json_type(data, '$.state') = 'text')
         ORDER BY id"
    )
}

/// `review-<trigger>` events: every review event with a review id and both states. The type range (`review-` up to
/// `review.`, the next character) is the `LIKE 'review-%'` that the `events_type` index can serve.
const REVIEW_CHANGES: &str = "SELECT json_extract(data, '$.review'), json_extract(data, '$.from'),
         json_extract(data, '$.to'), at, id
     FROM events
     WHERE type >= 'review-' AND type < 'review.' AND json_valid(data) AND json_type(data, '$.review') = 'integer'
         AND json_type(data, '$.from') = 'text' AND json_type(data, '$.to') = 'text'
     ORDER BY id";

const AGENTS: &str =
    "SELECT agent_id, session_id, agent_type, description, model, spawn_depth, first_at, last_at,
         parent_agent_id, finished
     FROM agents ORDER BY first_at, agent_id";

fn agent(row: &Row<'_>) -> rusqlite::Result<AgentFact> {
    Ok(AgentFact {
        id: row.get(0)?,
        session: row.get(1)?,
        agent_type: row.get(2)?,
        description: row.get(3)?,
        model: row.get(4)?,
        depth: row.get(5)?,
        first_at: row.get(6)?,
        last_at: row.get(7)?,
        parent: row.get(8)?,
        finished: row.get(9)?,
    })
}

const HELPERS: &str = "SELECT session_id, task_id, MIN(at) FROM events
     WHERE type = ?1 AND session_id IS NOT NULL AND task_id IS NOT NULL GROUP BY session_id, task_id ORDER BY MIN(id)";

fn helper(row: &Row<'_>) -> rusqlite::Result<HelperFact> {
    Ok(HelperFact {
        session: row.get(0)?,
        task: row.get(1)?,
        at: row.get(2)?,
    })
}

fn jobs(conn: &Connection) -> Result<Vec<JobFact>, StoreError> {
    type Raw = (
        i64,
        Option<i64>,
        Option<String>,
        String,
        String,
        String,
        Option<String>,
    );
    let raw: Vec<Raw> = rows(
        conn,
        "SELECT id, task_id, session_id, label, state, started_at, ended_at FROM jobs ORDER BY id",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    raw.into_iter()
        .map(|(id, task, session, label, state, started_at, ended_at)| {
            let state = word(&state, || format!("job {id}"))?;
            Ok(JobFact {
                id,
                task,
                session,
                label,
                state,
                started_at,
                ended_at,
            })
        })
        .collect()
}

struct RawPr {
    id: i64,
    task: i64,
    number: i64,
    url: String,
    state: String,
    first_at: String,
}

fn raw_pr(row: &Row<'_>) -> rusqlite::Result<RawPr> {
    Ok(RawPr {
        id: row.get(0)?,
        task: row.get(1)?,
        number: row.get(2)?,
        url: row.get(3)?,
        state: row.get(4)?,
        first_at: row.get(5)?,
    })
}

const PRS: &str = "SELECT p.id, p.task_id, p.number, p.url, p.state,
         COALESCE((SELECT MIN(e.at) FROM events e
             WHERE e.task_id = p.task_id AND instr(e.data, '\"' || p.url || '\"') > 0), t.queued_at)
     FROM prs p JOIN tasks t ON t.id = p.task_id ORDER BY p.id";

fn prs(conn: &Connection) -> Result<Vec<PrFact>, StoreError> {
    rows(conn, PRS, [], raw_pr)?
        .into_iter()
        .map(|pr| {
            let id = pr.id;
            Ok(PrFact {
                id,
                task: pr.task,
                number: pr.number,
                url: pr.url,
                state: word(&pr.state, || format!("pull request {id}"))?,
                first_at: pr.first_at,
            })
        })
        .collect()
}

struct RawAsk {
    id: i64,
    task: Option<i64>,
    session: Option<String>,
    header: Option<String>,
    question: String,
    state: String,
    created_at: String,
    closed_at: Option<String>,
}

fn raw_ask(row: &Row<'_>) -> rusqlite::Result<RawAsk> {
    Ok(RawAsk {
        id: row.get(0)?,
        task: row.get(1)?,
        session: row.get(2)?,
        header: row.get(3)?,
        question: row.get(4)?,
        state: row.get(5)?,
        created_at: row.get(6)?,
        closed_at: row.get(7)?,
    })
}

fn asks(conn: &Connection) -> Result<Vec<AskFact>, StoreError> {
    let raw = rows(
        conn,
        "SELECT a.id, a.task_id, a.session_id, NULLIF(a.topic, ''), a.question, a.state, a.created_at,
             COALESCE(a.answered_at, (SELECT MIN(e.at) FROM events e
                 WHERE e.type = ?1 AND json_valid(e.data) AND json_extract(e.data, '$.ask') = a.id))
         FROM asks a ORDER BY a.id",
        params![EVENT_ASK_SUPERSEDED],
        raw_ask,
    )?;
    raw.into_iter()
        .map(|ask| {
            let id = ask.id;
            Ok(AskFact {
                id,
                task: ask.task,
                session: ask.session,
                header: ask.header,
                question: ask.question,
                state: word(&ask.state, || format!("ask {id}"))?,
                created_at: ask.created_at,
                closed_at: ask.closed_at,
            })
        })
        .collect()
}

struct RawReview {
    id: i64,
    task: i64,
    part: Option<i64>,
    flow: String,
    level: i64,
    state: String,
    requested_at: String,
    finished_at: Option<String>,
}

fn raw_review(row: &Row<'_>) -> rusqlite::Result<RawReview> {
    Ok(RawReview {
        id: row.get(0)?,
        task: row.get(1)?,
        part: row.get(2)?,
        flow: row.get(3)?,
        level: row.get(4)?,
        state: row.get(5)?,
        requested_at: row.get(6)?,
        finished_at: row.get(7)?,
    })
}

fn reviews(conn: &Connection) -> Result<Vec<ReviewFact>, StoreError> {
    let raw = rows(
        conn,
        "SELECT id, task_id, part_id, flow, level, state, requested_at, finished_at FROM reviews ORDER BY id",
        [],
        raw_review,
    )?;
    raw.into_iter()
        .map(|review| {
            let id = review.id;
            let level = u8::try_from(review.level)
                .ok()
                .and_then(Level::from_number)
                .ok_or(StoreError::OutOfRange(review.level))?;
            Ok(ReviewFact {
                id,
                task: review.task,
                part: review.part,
                flow: review.flow,
                level,
                state: word(&review.state, || format!("review {id}"))?,
                requested_at: review.requested_at,
                finished_at: review.finished_at,
            })
        })
        .collect()
}

fn review_nodes(conn: &Connection) -> Result<Vec<ReviewNodeFact>, StoreError> {
    type Raw = (
        i64,
        i64,
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
    );
    let raw: Vec<Raw> = rows(
        conn,
        "SELECT review_id, round, node, item, state, started_at, finished_at FROM review_nodes
         ORDER BY review_id, round, node, item",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    raw.into_iter()
        .map(
            |(review, round, node, item, state, started_at, finished_at)| {
                let state = word(&state, || format!("review {review} node {node}"))?;
                Ok(ReviewNodeFact {
                    review,
                    round,
                    node,
                    item,
                    state,
                    started_at,
                    finished_at,
                })
            },
        )
        .collect()
}

const MESSAGES: &str = "SELECT session_id, json_extract(data, '$.to'), json_extract(data, '$.summary'), at FROM events
     WHERE type = ?1 AND session_id IS NOT NULL AND json_valid(data)
         AND json_extract(data, '$.tool') = 'SendMessage' AND json_type(data, '$.to') = 'text'
         AND json_extract(data, '$.to') <> '' AND COALESCE(json_type(data, '$.summary'), 'null') IN ('text', 'null')
     ORDER BY id";

fn message(row: &Row<'_>) -> rusqlite::Result<MessageFact> {
    Ok(MessageFact {
        from: row.get(0)?,
        to: row.get(1)?,
        summary: row.get(2)?,
        at: row.get(3)?,
    })
}

const RECEIVED: &str = "SELECT session_id, json_extract(data, '$.from'), at FROM events
     WHERE type = ?1 AND session_id IS NOT NULL AND json_valid(data)
         AND json_type(data, '$.cross_session') = 'true' AND json_type(data, '$.from') = 'text'
         AND json_extract(data, '$.from') <> ''
     ORDER BY id";

fn received(row: &Row<'_>) -> rusqlite::Result<ReceivedFact> {
    Ok(ReceivedFact {
        session: row.get(0)?,
        from: row.get(1)?,
        at: row.get(2)?,
    })
}

const REPORTS: &str =
    "SELECT session_id, COALESCE(json_extract(data, '$.status'), ''), at FROM events
     WHERE type = ?1 AND session_id IS NOT NULL AND json_valid(data)
         AND COALESCE(json_type(data, '$.status'), 'null') IN ('text', 'null')
     ORDER BY id";

fn report(row: &Row<'_>) -> rusqlite::Result<ReportFact> {
    Ok(ReportFact {
        session: row.get(0)?,
        status: row.get(1)?,
        at: row.get(2)?,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]
mod tests {
    use headwork_core::{Actor, Config, PermissionMode, ProjectName, Slug, TaskRequest, TaskShape};

    use crate::{NewPr, NewTask, Store};

    #[test]
    fn a_pull_request_no_event_names_starts_with_its_task() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/config.toml"
        ))
        .unwrap();
        let config = Config::parse(&text, "config.toml").unwrap();
        let mut store = Store::open_in_memory().unwrap();
        store.sync_projects(&config).unwrap();
        let new = NewTask {
            request: TaskRequest {
                project: ProjectName::new("me-sim").unwrap(),
                slug: Slug::new("sand").unwrap(),
                kind: "scene".to_owned(),
                model: None,
                effort: None,
                in_main: false,
                keep: false,
                close: headwork_core::CloseMode::Confirm,
                review: None,
                decision: None,
            },
            brief_path: "briefs/sand.md".to_owned(),
            depends_on: None,
            permission_mode: PermissionMode::Auto,
            shape: TaskShape::Plain,
        };
        let task = store
            .create_task(&config, &new, Actor::Orchestrator)
            .unwrap();
        let pr = NewPr {
            repo: "o/r".to_owned(),
            number: 5,
            url: "https://github.com/o/r/pull/5".to_owned(),
        };
        store
            .record_report(task.id, "s1", "review", "x", &[], Some(&pr))
            .unwrap();
        store
            .conn
            .execute("DELETE FROM events WHERE type = 'report'", [])
            .unwrap();
        let facts = store.graph_facts().unwrap();
        assert_eq!(
            facts.prs[0].first_at, facts.tasks[0].queued_at,
            "the task's start"
        );
    }

    #[test]
    fn inside_an_open_transaction_the_facts_read_in_it() {
        let store = Store::open_in_memory().unwrap();
        store.conn.execute_batch("BEGIN").unwrap();
        assert!(store.graph_facts().is_ok(), "no nested BEGIN");
        store.conn.execute_batch("COMMIT").unwrap();
    }
}
