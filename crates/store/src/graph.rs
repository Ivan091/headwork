//! What the agent graph is drawn from (`docs/dashboard.md`): the subagents table the usage reader fills, and one read
//! of every row and event the graph needs, handed to [`headwork_core::graph::build`].

use std::str::FromStr;

use headwork_core::graph::{
    AgentFact, AskFact, Change, Facts, HelperFact, JobFact, MessageFact, PrFact, ReportFact,
    ReviewFact, ReviewNodeFact, SessionFact, TaskFact, TransitionFact,
};
use headwork_core::review::Level;
use headwork_core::{Actor, UnknownWord};
use rusqlite::{Connection, Row, Transaction, params};

use crate::hooks::EVENT_TOOL;
use crate::reports::EVENT_REPORT;
use crate::usage::{EVENT_HELPER_SESSION, UsageBatch};
use crate::{Store, StoreError};

/// The event that records an ask superseded without an answer; its time is when the ask closed.
const EVENT_ASK_SUPERSEDED: &str = "ask-superseded";

/// Records the batch's subagent, or refreshes it, in the batch's own transaction: its first and last times widen to
/// take in the batch's responses, and a meta field the batch lacks keeps what was known.
pub(crate) fn upsert_agent(
    tx: &Transaction<'_>,
    agent: &str,
    batch: &UsageBatch<'_>,
) -> Result<(), StoreError> {
    let first = batch.lines.iter().map(|line| line.at.as_str()).min();
    let last = batch.lines.iter().map(|line| line.at.as_str()).max();
    let meta = batch.agent_meta;
    tx.execute(
        "INSERT INTO agents (agent_id, session_id, agent_type, description, model, spawn_depth, parent_agent_id,
             first_at, last_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?9, ?7, ?8)
         ON CONFLICT (agent_id) DO UPDATE SET
             agent_type = COALESCE(excluded.agent_type, agents.agent_type),
             description = COALESCE(excluded.description, agents.description),
             model = COALESCE(excluded.model, agents.model),
             spawn_depth = COALESCE(excluded.spawn_depth, agents.spawn_depth),
             parent_agent_id = COALESCE(excluded.parent_agent_id, agents.parent_agent_id),
             first_at = CASE WHEN agents.first_at IS NULL OR excluded.first_at < agents.first_at
                 THEN COALESCE(excluded.first_at, agents.first_at) ELSE agents.first_at END,
             last_at = CASE WHEN agents.last_at IS NULL OR excluded.last_at > agents.last_at
                 THEN COALESCE(excluded.last_at, agents.last_at) ELSE agents.last_at END",
        params![
            agent,
            batch.session_id,
            meta.and_then(|meta| meta.agent_type.as_deref()),
            meta.and_then(|meta| meta.description.as_deref()),
            meta.and_then(|meta| meta.model.as_deref()),
            meta.and_then(|meta| meta.spawn_depth),
            first,
            last,
            meta.and_then(|meta| meta.parent_agent_id.as_deref()),
        ],
    )?;
    Ok(())
}

impl Store {
    /// Every fact the graph is drawn from, read in one transaction so they come from one moment.
    ///
    /// # Errors
    ///
    /// Returns a read error, or a stored word this build does not know, naming the row it came from.
    pub fn graph_facts(&self) -> Result<Facts, StoreError> {
        let conn = &self.conn;
        conn.execute_batch("BEGIN DEFERRED")?;
        let facts = read_facts(conn);
        let end = if facts.is_ok() { "COMMIT" } else { "ROLLBACK" };
        conn.execute_batch(end)?;
        facts
    }
}

fn read_facts(conn: &Connection) -> Result<Facts, StoreError> {
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
        reports: rows(conn, REPORTS, params![EVENT_REPORT], report)?,
        transitions: transitions(conn)?,
        pr_changes: changes(conn, PR_CHANGES, "pull request")?,
        job_changes: changes(conn, JOB_CHANGES, "job")?,
        review_changes: changes(conn, REVIEW_CHANGES, "review")?,
    })
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
    let raw = rows(
        conn,
        "SELECT t.id, t.project, t.slug, t.kind, t.state, t.model, t.effort, t.parent_id, t.depends_on, t.lead,
             (SELECT e.actor FROM events e WHERE e.task_id = t.id AND e.type = 'task-created' ORDER BY e.id LIMIT 1),
             t.queued_at, t.last_report
         FROM tasks t ORDER BY t.id",
        [],
        raw_task,
    )?;
    raw.into_iter()
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
        "SELECT id, task_id, role, rc_name, state, started_at, ended_at FROM sessions ORDER BY started_at, id",
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

fn transitions(conn: &Connection) -> Result<Vec<TransitionFact>, StoreError> {
    let raw: Vec<(i64, i64, String, String, String)> = rows(
        conn,
        "SELECT id, task_id, json_extract(data, '$.from'), json_extract(data, '$.to'), at FROM events
         WHERE type = 'transition' AND task_id IS NOT NULL AND json_valid(data)
             AND json_extract(data, '$.from') IS NOT NULL AND json_extract(data, '$.to') IS NOT NULL
         ORDER BY id",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    raw.into_iter()
        .map(|(event, task, from, to, at)| {
            let row = || format!("event {event}");
            Ok(TransitionFact {
                task,
                from: word(&from, row)?,
                to: word(&to, row)?,
                at,
            })
        })
        .collect()
}

const AGENTS: &str =
    "SELECT agent_id, session_id, agent_type, description, model, spawn_depth, first_at, last_at,
         parent_agent_id
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
            Ok(JobFact {
                id,
                task,
                session,
                label,
                state: word(&state, || format!("job {id}"))?,
                started_at,
                ended_at,
            })
        })
        .collect()
}

const PRS: &str = "SELECT p.id, p.task_id, p.number, p.url, p.state,
         COALESCE((SELECT MIN(e.at) FROM events e
             WHERE e.task_id = p.task_id AND instr(e.data, '\"' || p.url || '\"') > 0), t.queued_at)
     FROM prs p JOIN tasks t ON t.id = p.task_id ORDER BY p.id";

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

/// `pr-state` events, keyed by the pull request of their task and URL.
const PR_CHANGES: &str = "SELECT p.id, json_extract(e.data, '$.from'), json_extract(e.data, '$.to'), e.at, e.id
     FROM events e JOIN prs p ON p.task_id = e.task_id AND p.url = json_extract(e.data, '$.url')
     WHERE e.type = 'pr-state' AND json_valid(e.data) AND json_extract(e.data, '$.to') IS NOT NULL ORDER BY e.id";

/// `job-state` events, which name the new state only, and `job-started` events (a job started again after a park),
/// a change to `running`.
const JOB_CHANGES: &str = "SELECT json_extract(data, '$.job'), NULL,
         CASE type WHEN 'job-started' THEN 'running' ELSE json_extract(data, '$.state') END, at, id
     FROM events
     WHERE type IN ('job-state', 'job-started') AND json_valid(data) AND json_extract(data, '$.job') IS NOT NULL
         AND (type = 'job-started' OR json_extract(data, '$.state') IS NOT NULL)
     ORDER BY id";

/// `review-<trigger>` events: every review event with a review id and both states. The type range (`review-` up to
/// `review.`, the next character) is the `LIKE 'review-%'` that the `events_type` index can serve.
const REVIEW_CHANGES: &str = "SELECT json_extract(data, '$.review'), json_extract(data, '$.from'), json_extract(data, '$.to'),
         at, id
     FROM events
     WHERE type >= 'review-' AND type < 'review.' AND json_valid(data) AND json_extract(data, '$.review') IS NOT NULL
         AND json_extract(data, '$.from') IS NOT NULL AND json_extract(data, '$.to') IS NOT NULL
     ORDER BY id";

/// The logged state changes a query returns as `(key, from, to, at, event id)`.
fn changes<S: FromStr<Err = UnknownWord>>(
    conn: &Connection,
    sql: &str,
    what: &str,
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
    raw.into_iter()
        .map(|(key, from, to, at, event)| {
            let row = || format!("event {event} ({what} {key})");
            Ok(Change {
                key,
                from: from.map(|from| word(&from, row)).transpose()?,
                to: word(&to, row)?,
                at,
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
         AND json_extract(data, '$.tool') = 'SendMessage' AND COALESCE(json_extract(data, '$.to'), '') <> ''
     ORDER BY id";

fn message(row: &Row<'_>) -> rusqlite::Result<MessageFact> {
    Ok(MessageFact {
        from: row.get(0)?,
        to: row.get(1)?,
        summary: row.get(2)?,
        at: row.get(3)?,
    })
}

const REPORTS: &str =
    "SELECT session_id, COALESCE(json_extract(data, '$.status'), ''), at FROM events
     WHERE type = ?1 AND session_id IS NOT NULL AND json_valid(data) ORDER BY id";

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
}
