//! What the agent graph is drawn from (`docs/dashboard.md`): the subagents table the usage reader fills, and one read
//! of every row and event the graph needs, handed to [`headwork_core::graph::build`].

use headwork_core::graph::{
    AgentFact, AskFact, Facts, HelperFact, JobFact, MessageFact, PrFact, ReportFact, ReviewFact,
    ReviewNodeFact, SessionFact, TaskFact, TransitionFact,
};
use headwork_core::usage::AgentMeta;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::hooks::EVENT_TOOL;
use crate::records::TaskId;
use crate::reports::EVENT_REPORT;
use crate::usage::EVENT_HELPER_SESSION;
use crate::{Store, StoreError};

/// A subagent the usage reader found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewAgent<'a> {
    /// The agent id from its transcript's name.
    pub agent_id: &'a str,
    /// The session that called it.
    pub session_id: &'a str,
    /// That session's task.
    pub task: Option<TaskId>,
    /// What its meta file says.
    pub meta: &'a AgentMeta,
}

impl Store {
    /// Whether the subagent is recorded already.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn agent_known(&self, agent_id: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM agents WHERE agent_id = ?1",
                params![agent_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Records a subagent, or refreshes it; its first and last times are the earliest and latest of what was stored
    /// before and its responses in `usage`.
    ///
    /// # Errors
    ///
    /// Returns a write error.
    pub fn record_agent(&self, agent: &NewAgent<'_>) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO agents (agent_id, session_id, task_id, agent_type, description, model, spawn_depth, first_at, last_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                 (SELECT MIN(at) FROM usage WHERE agent_id = ?1), (SELECT MAX(at) FROM usage WHERE agent_id = ?1))
             ON CONFLICT (agent_id) DO UPDATE SET
                 task_id = COALESCE(excluded.task_id, agents.task_id),
                 agent_type = COALESCE(excluded.agent_type, agents.agent_type),
                 description = COALESCE(excluded.description, agents.description),
                 model = COALESCE(excluded.model, agents.model),
                 spawn_depth = COALESCE(excluded.spawn_depth, agents.spawn_depth),
                 first_at = CASE WHEN agents.first_at IS NULL OR excluded.first_at < agents.first_at
                     THEN COALESCE(excluded.first_at, agents.first_at) ELSE agents.first_at END,
                 last_at = CASE WHEN agents.last_at IS NULL OR excluded.last_at > agents.last_at
                     THEN COALESCE(excluded.last_at, agents.last_at) ELSE agents.last_at END",
            params![
                agent.agent_id,
                agent.session_id,
                agent.task.map(|task| task.0),
                agent.meta.agent_type,
                agent.meta.description,
                agent.meta.model,
                agent.meta.spawn_depth,
            ],
        )?;
        Ok(())
    }

    /// Every fact the graph is drawn from.
    ///
    /// # Errors
    ///
    /// Returns a read error or an unknown stored word.
    pub fn graph_facts(&self) -> Result<Facts, StoreError> {
        let conn = &self.conn;
        Ok(Facts {
            tasks: tasks(conn)?,
            sessions: sessions(conn)?,
            agents: rows(conn, AGENTS, [], agent)?,
            helpers: rows(conn, HELPERS, params![EVENT_HELPER_SESSION], helper)?,
            jobs: rows(conn, JOBS, [], job)?,
            prs: rows(conn, PRS, [], pr)?,
            asks: rows(conn, ASKS, [], ask)?,
            reviews: rows(conn, REVIEWS, [], review)?,
            review_nodes: rows(conn, REVIEW_NODES, [], review_node)?,
            messages: rows(conn, MESSAGES, params![EVENT_TOOL], message)?,
            reports: rows(conn, REPORTS, params![EVENT_REPORT], report)?,
            transitions: transitions(conn)?,
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

fn tasks(conn: &Connection) -> Result<Vec<TaskFact>, StoreError> {
    type Raw = (
        i64,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
        bool,
        String,
        Option<String>,
    );
    let raw: Vec<Raw> = rows(
        conn,
        "SELECT id, project, slug, kind, state, model, effort, parent_id, depends_on, lead, queued_at, last_report
         FROM tasks ORDER BY id",
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
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
            ))
        },
    )?;
    raw.into_iter()
        .map(
            |(
                id,
                project,
                slug,
                kind,
                state,
                model,
                effort,
                parent,
                depends_on,
                lead,
                queued_at,
                last_report,
            )| {
                Ok(TaskFact {
                    id,
                    name: format!("{project}-{slug}"),
                    project,
                    kind,
                    state: state.parse()?,
                    model,
                    effort,
                    parent,
                    depends_on,
                    lead,
                    queued_at,
                    last_report,
                })
            },
        )
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
            Ok(SessionFact {
                id,
                task,
                role: role.parse()?,
                rc_name,
                state,
                started_at,
                ended_at,
            })
        })
        .collect()
}

fn transitions(conn: &Connection) -> Result<Vec<TransitionFact>, StoreError> {
    let raw: Vec<(i64, String, String, String)> = rows(
        conn,
        "SELECT task_id, json_extract(data, '$.from'), json_extract(data, '$.to'), at FROM events
         WHERE type = 'transition' AND task_id IS NOT NULL AND json_valid(data)
             AND json_extract(data, '$.from') IS NOT NULL AND json_extract(data, '$.to') IS NOT NULL
         ORDER BY id",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    raw.into_iter()
        .map(|(task, from, to, at)| {
            Ok(TransitionFact {
                task,
                from: from.parse()?,
                to: to.parse()?,
                at,
            })
        })
        .collect()
}

const AGENTS: &str = "SELECT agent_id, session_id, task_id, agent_type, description, model, spawn_depth, first_at, last_at
     FROM agents ORDER BY first_at, agent_id";

fn agent(row: &Row<'_>) -> rusqlite::Result<AgentFact> {
    Ok(AgentFact {
        id: row.get(0)?,
        session: row.get(1)?,
        task: row.get(2)?,
        agent_type: row.get(3)?,
        description: row.get(4)?,
        model: row.get(5)?,
        depth: row.get(6)?,
        first_at: row.get(7)?,
        last_at: row.get(8)?,
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

const JOBS: &str =
    "SELECT id, task_id, session_id, label, state, started_at, ended_at FROM jobs ORDER BY id";

fn job(row: &Row<'_>) -> rusqlite::Result<JobFact> {
    Ok(JobFact {
        id: row.get(0)?,
        task: row.get(1)?,
        session: row.get(2)?,
        label: row.get(3)?,
        state: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
    })
}

const PRS: &str = "SELECT p.id, p.task_id, p.number, p.url, p.state,
         COALESCE((SELECT MIN(e.at) FROM events e WHERE e.task_id = p.task_id AND instr(e.data, p.url) > 0), t.queued_at)
     FROM prs p JOIN tasks t ON t.id = p.task_id ORDER BY p.id";

fn pr(row: &Row<'_>) -> rusqlite::Result<PrFact> {
    Ok(PrFact {
        id: row.get(0)?,
        task: row.get(1)?,
        number: row.get(2)?,
        url: row.get(3)?,
        state: row.get(4)?,
        first_at: row.get(5)?,
    })
}

const ASKS: &str = "SELECT id, task_id, session_id, COALESCE(NULLIF(topic, ''), substr(question, 1, 40)), question, state, created_at,
         CASE WHEN state IN ('pending', 'open') THEN NULL ELSE COALESCE(answered_at, created_at) END
     FROM asks ORDER BY id";

fn ask(row: &Row<'_>) -> rusqlite::Result<AskFact> {
    Ok(AskFact {
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

const REVIEWS: &str = "SELECT id, task_id, part_id, flow, level, state, requested_at, finished_at FROM reviews ORDER BY id";

fn review(row: &Row<'_>) -> rusqlite::Result<ReviewFact> {
    Ok(ReviewFact {
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

const REVIEW_NODES: &str =
    "SELECT review_id, round, node, item, state, started_at, finished_at FROM review_nodes
     ORDER BY review_id, round, node, item";

fn review_node(row: &Row<'_>) -> rusqlite::Result<ReviewNodeFact> {
    Ok(ReviewNodeFact {
        review: row.get(0)?,
        round: row.get(1)?,
        node: row.get(2)?,
        item: row.get(3)?,
        state: row.get(4)?,
        started_at: row.get(5)?,
        finished_at: row.get(6)?,
    })
}

const MESSAGES: &str =
    "SELECT session_id, json_extract(data, '$.to'), json_extract(data, '$.summary'), at FROM events
     WHERE type = ?1 AND session_id IS NOT NULL AND json_valid(data)
         AND json_extract(data, '$.tool') = 'SendMessage' AND json_extract(data, '$.to') IS NOT NULL
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
