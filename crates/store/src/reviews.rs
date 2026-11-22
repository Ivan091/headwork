//! Reviews, their node runs and their findings (`docs/review-flows.md`, section 7).
//!
//! Every state change goes through [`headwork_core::review::apply`]; the store counts the facts its guards read from the
//! findings, and the caller adds the ones only the flow can tell (the round complete, another round due).

use headwork_core::review::{
    Instance, Level, NodeId, Progress, ReviewFacts, ReviewRefusal, ReviewTrigger,
};
use headwork_core::{Actor, FindingVerdict, NodeState, Resolution, ReviewState, Severity};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;

use crate::queries::insert_event;
use crate::records::TaskId;
use crate::{NOW, Store, StoreError};

/// The event a new review logs.
pub const EVENT_REVIEW_REQUESTED: &str = "review-requested";

/// A review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewRow {
    /// The row id.
    pub id: i64,
    /// The implementing lead's task.
    pub task_id: TaskId,
    /// The reviewer lead's part, once started.
    pub part_id: Option<TaskId>,
    /// The flow's name.
    pub flow: String,
    /// The level.
    pub level: Level,
    /// The user's focus; empty when none.
    pub focus: String,
    /// The commit the diff starts at.
    pub base_sha: Option<String>,
    /// The commit under review.
    pub head_sha: Option<String>,
    /// The round, from 1.
    pub round: u8,
    /// Where it is.
    pub state: ReviewState,
    /// When the user asked for it.
    pub requested_at: String,
    /// When it finished or was cancelled.
    pub finished_at: Option<String>,
}

/// A finding a node returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindingRow {
    /// The row id, the number the implementing lead answers by.
    pub id: i64,
    /// The review.
    pub review_id: i64,
    /// The round it came from.
    pub round: u8,
    /// The node that reported it.
    pub node: String,
    /// The file, if it names one.
    pub file: Option<String>,
    /// The line, if it names one.
    pub line: Option<u32>,
    /// How much it weighs.
    pub severity: Severity,
    /// What is wrong.
    pub summary: String,
    /// The concrete failing scenario.
    pub scenario: String,
    /// What the verify stage concluded, if it ran.
    pub verdict: Option<FindingVerdict>,
    /// Where it stands with the implementing lead, once delivered.
    pub resolution: Option<Resolution>,
    /// The lead's or reviewer's note on the resolution.
    pub note: Option<String>,
    /// The reviewer lead already argued back once on its dispute.
    pub argued: bool,
}

/// A finding to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFinding<'a> {
    /// The file.
    pub file: Option<&'a str>,
    /// The line.
    pub line: Option<u32>,
    /// How much it weighs.
    pub severity: Severity,
    /// What is wrong.
    pub summary: &'a str,
    /// The failing scenario.
    pub scenario: &'a str,
}

/// How many findings wait on whom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct FindingCounts {
    /// Open, fix rejected, or kept by the user: the implementing lead's turn.
    pub awaiting_lead: u32,
    /// Fixed or disputed with no verdict yet: the reviewer lead's turn.
    pub awaiting_verdict: u32,
    /// Put to the user, unanswered.
    pub asked: u32,
    /// Fixed this round (a fix accepted or still awaiting its verdict).
    pub fixed: u32,
}

impl FindingCounts {
    /// The guard facts these counts give, with the two only the flow can tell.
    #[must_use]
    pub const fn facts(self, round_complete: bool, round_due: bool) -> ReviewFacts {
        ReviewFacts {
            round_complete,
            awaiting_lead: self.awaiting_lead,
            awaiting_verdict: self.awaiting_verdict,
            asked: self.asked,
            round_due,
        }
    }
}

/// Why a review was not recorded or moved.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The task already has an open review.
    #[error("review {id} of this task is still {state}; finish it before asking for another")]
    Running {
        /// The open review.
        id: i64,
        /// Its state.
        state: ReviewState,
    },
    /// No review with this id.
    #[error("no review with id {0}")]
    Unknown(i64),
    /// The state machine refused.
    #[error("review {id}: {refusal}")]
    Refused {
        /// The review.
        id: i64,
        /// Why.
        #[source]
        refusal: ReviewRefusal,
    },
}

impl From<rusqlite::Error> for ReviewError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(StoreError::Sqlite(error))
    }
}

const REVIEW_COLUMNS: &str = "id, task_id, part_id, flow, level, focus, base_sha, head_sha, round, state, requested_at, finished_at";

type RawReview = (
    i64,
    i64,
    Option<i64>,
    String,
    i64,
    String,
    (Option<String>, Option<String>),
    i64,
    String,
    String,
    Option<String>,
);

fn read_review(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawReview> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        (row.get(6)?, row.get(7)?),
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
    ))
}

fn small(value: i64) -> Result<u8, StoreError> {
    u8::try_from(value).map_err(|_| StoreError::OutOfRange(value))
}

fn into_review(raw: RawReview) -> Result<ReviewRow, StoreError> {
    let (
        id,
        task,
        part,
        flow,
        level,
        focus,
        (base_sha, head_sha),
        round,
        state,
        requested_at,
        finished_at,
    ) = raw;
    Ok(ReviewRow {
        id,
        task_id: TaskId(task),
        part_id: part.map(TaskId),
        flow,
        level: Level::from_number(small(level)?).ok_or(StoreError::OutOfRange(level))?,
        focus,
        base_sha,
        head_sha,
        round: small(round)?,
        state: state.parse()?,
        requested_at,
        finished_at,
    })
}

const FINDING_COLUMNS: &str = "id, review_id, round, node, file, line, severity, summary, scenario, verdict, resolution, note, argued";

type RawFinding = (
    i64,
    i64,
    i64,
    String,
    Option<String>,
    Option<i64>,
    String,
    String,
    String,
    (Option<String>, Option<String>),
    Option<String>,
    bool,
);

fn read_finding(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawFinding> {
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
        row.get(11)?,
        row.get(12)?,
    ))
}

fn into_finding(raw: RawFinding) -> Result<FindingRow, StoreError> {
    let (
        id,
        review_id,
        round,
        node,
        file,
        line,
        severity,
        summary,
        scenario,
        (verdict, resolution),
        remark,
        argued,
    ) = raw;
    Ok(FindingRow {
        id,
        review_id,
        round: small(round)?,
        node,
        file,
        line: line
            .map(|n| u32::try_from(n).map_err(|_| StoreError::OutOfRange(n)))
            .transpose()?,
        severity: severity.parse()?,
        summary,
        scenario,
        verdict: verdict.map(|word| word.parse()).transpose()?,
        resolution: resolution.map(|word| word.parse()).transpose()?,
        note: remark,
        argued,
    })
}

fn review_in(conn: &Connection, id: i64) -> Result<Option<ReviewRow>, StoreError> {
    conn.query_row(
        &format!("SELECT {REVIEW_COLUMNS} FROM reviews WHERE id = ?1"),
        params![id],
        read_review,
    )
    .optional()?
    .map(into_review)
    .transpose()
}

fn open_review_in(conn: &Connection, task: TaskId) -> Result<Option<ReviewRow>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {REVIEW_COLUMNS} FROM reviews WHERE (task_id = ?1 OR part_id = ?1) \
             AND state NOT IN ('done', 'cancelled') ORDER BY id DESC LIMIT 1"
        ),
        params![task.0],
        read_review,
    )
    .optional()?
    .map(into_review)
    .transpose()
}

fn counts_in(conn: &Connection, review: &ReviewRow) -> Result<FindingCounts, StoreError> {
    let count = |sql: &str| -> Result<u32, StoreError> {
        let n: i64 = conn.query_row(sql, params![review.id, review.round], |row| row.get(0))?;
        u32::try_from(n).map_err(|_| StoreError::OutOfRange(n))
    };
    Ok(FindingCounts {
        awaiting_lead: count(
            "SELECT COUNT(*) FROM review_findings WHERE review_id = ?1 AND round <= ?2 \
             AND resolution IN ('open', 'fix-rejected', 'user-kept')",
        )?,
        awaiting_verdict: count(
            "SELECT COUNT(*) FROM review_findings WHERE review_id = ?1 AND round <= ?2 \
             AND resolution IN ('fixed', 'disputed')",
        )?,
        asked: count(
            "SELECT COUNT(*) FROM review_findings WHERE review_id = ?1 AND round <= ?2 AND resolution = 'asked'",
        )?,
        fixed: count(
            "SELECT COUNT(*) FROM review_findings WHERE review_id = ?1 AND round = ?2 \
             AND resolution IN ('fixed', 'fix-accepted')",
        )?,
    })
}

fn node_state_in(
    tx: &Transaction<'_>,
    review: &ReviewRow,
    instance: &Instance,
) -> Result<Option<NodeState>, StoreError> {
    tx.query_row(
        "SELECT state FROM review_nodes WHERE review_id = ?1 AND round = ?2 AND node = ?3 AND item = ?4",
        params![review.id, review.round, instance.node.as_str(), instance.item],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|word| word.parse().map_err(StoreError::from))
    .transpose()
}

impl Store {
    /// Records a review the user asked for, in state `requested`, with its event.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Running`] when the task already has an open review, or a store error.
    pub fn request_review(
        &mut self,
        task: TaskId,
        flow: &str,
        level: Level,
        focus: &str,
        actor: Actor,
    ) -> Result<ReviewRow, ReviewError> {
        let tx = self.conn.transaction()?;
        if let Some(open) = open_review_in(&tx, task)? {
            return Err(ReviewError::Running {
                id: open.id,
                state: open.state,
            });
        }
        tx.execute(
            &format!(
                "INSERT INTO reviews (task_id, flow, level, focus, state, requested_at) \
                 VALUES (?1, ?2, ?3, ?4, 'requested', {NOW})"
            ),
            params![task.0, flow, level.number(), focus],
        )?;
        let id = tx.last_insert_rowid();
        insert_event(
            &tx,
            actor,
            Some(task),
            None,
            EVENT_REVIEW_REQUESTED,
            &serde_json::json!({ "review": id, "flow": flow, "level": level.number(), "focus": focus }),
        )?;
        let row = review_in(&tx, id)?.ok_or(ReviewError::Unknown(id))?;
        tx.commit()?;
        Ok(row)
    }

    /// A review by id.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn review(&self, id: i64) -> Result<Option<ReviewRow>, StoreError> {
        review_in(&self.conn, id)
    }

    /// The open review of a task, found from the implementing lead's task or the reviewer lead's part.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn open_review(&self, task: TaskId) -> Result<Option<ReviewRow>, StoreError> {
        open_review_in(&self.conn, task)
    }

    /// Every review of a task, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn reviews_of(&self, task: TaskId) -> Result<Vec<ReviewRow>, StoreError> {
        let raws = self
            .conn
            .prepare(&format!(
                "SELECT {REVIEW_COLUMNS} FROM reviews WHERE task_id = ?1 ORDER BY id"
            ))?
            .query_map(params![task.0], read_review)?
            .collect::<Result<Vec<_>, _>>()?;
        raws.into_iter().map(into_review).collect()
    }

    /// Records the part that runs the review and the commits it covers.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Unknown`] or a store error.
    pub fn set_review_target(
        &mut self,
        id: i64,
        part: Option<TaskId>,
        base_sha: &str,
        head_sha: &str,
    ) -> Result<ReviewRow, ReviewError> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE reviews SET part_id = COALESCE(?2, part_id), base_sha = ?3, head_sha = ?4 WHERE id = ?1",
            params![id, part.map(|task| task.0), base_sha, head_sha],
        )?;
        if changed == 0 {
            return Err(ReviewError::Unknown(id));
        }
        let row = review_in(&tx, id)?.ok_or(ReviewError::Unknown(id))?;
        tx.commit()?;
        Ok(row)
    }

    /// How many findings of the review wait on whom, up to its current round.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn finding_counts(&self, review: &ReviewRow) -> Result<FindingCounts, StoreError> {
        counts_in(&self.conn, review)
    }

    /// Moves a review through its state machine and logs `review-<trigger>`.
    ///
    /// `round_complete` and `round_due` are the facts only the flow can tell; the finding counts are read here, in the
    /// same transaction. A step from `rechecking` or `asking` back to `running` starts the next round.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Refused`] with the state machine's reason, logged as `review-refused`, or a store error.
    pub fn move_review(
        &mut self,
        id: i64,
        trigger: ReviewTrigger,
        round_complete: bool,
        round_due: bool,
        actor: Actor,
    ) -> Result<ReviewRow, ReviewError> {
        let tx = self.conn.transaction()?;
        let review = review_in(&tx, id)?.ok_or(ReviewError::Unknown(id))?;
        let facts = counts_in(&tx, &review)?.facts(round_complete, round_due);
        let to = match headwork_core::review::apply(review.state, trigger, facts) {
            Ok(to) => to,
            Err(refusal) => {
                drop(tx);
                insert_event(
                    &self.conn,
                    actor,
                    Some(review.task_id),
                    None,
                    "review-refused",
                    &serde_json::json!({ "review": id, "trigger": trigger.name(), "reason": refusal.to_string() }),
                )?;
                return Err(ReviewError::Refused { id, refusal });
            }
        };
        let next_round = to == ReviewState::Running && review.state != ReviewState::Requested;
        tx.execute(
            &format!(
                "UPDATE reviews SET state = ?2, round = round + ?3, \
                 finished_at = CASE WHEN ?2 IN ('done', 'cancelled') THEN {NOW} ELSE finished_at END WHERE id = ?1"
            ),
            params![id, to.as_str(), i64::from(next_round)],
        )?;
        insert_event(
            &tx,
            actor,
            Some(review.task_id),
            None,
            &format!("review-{}", trigger.name()),
            &serde_json::json!({
                "review": id,
                "from": review.state.as_str(),
                "to": to.as_str(),
                "round": review.round + u8::from(next_round),
            }),
        )?;
        let row = review_in(&tx, id)?.ok_or(ReviewError::Unknown(id))?;
        tx.commit()?;
        Ok(row)
    }

    /// What the review's current round has run: every node instance and the findings of each node.
    ///
    /// # Errors
    ///
    /// Returns a read error, or a stored word or name this build does not know.
    pub fn review_progress(&self, review: &ReviewRow) -> Result<Progress, StoreError> {
        let mut progress = Progress::default();
        let nodes = self
            .conn
            .prepare(
                "SELECT node, item, state FROM review_nodes WHERE review_id = ?1 AND round = ?2",
            )?
            .query_map(params![review.id, review.round], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (node, item, state) in nodes {
            let node = NodeId::new(&node)?;
            progress
                .instances
                .insert(Instance { node, item }, state.parse()?);
        }
        let findings = self
            .conn
            .prepare("SELECT node, id FROM review_findings WHERE review_id = ?1 AND round = ?2 ORDER BY id")?
            .query_map(params![review.id, review.round], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (node, finding) in findings {
            let node = NodeId::new(&node)?;
            progress.findings.entry(node).or_default().push(finding);
        }
        Ok(progress)
    }

    /// Marks node instances of the current round `running`; instances already taken are left as they are.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub fn take_review_nodes(
        &mut self,
        review: &ReviewRow,
        instances: &[Instance],
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for instance in instances {
            tx.execute(
                &format!(
                    "INSERT OR IGNORE INTO review_nodes (review_id, round, node, item, state, started_at) \
                     VALUES (?1, ?2, ?3, ?4, 'running', {NOW})"
                ),
                params![review.id, review.round, instance.node.as_str(), instance.item],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Records a node instance's end (`done` or `failed`) with the findings it returned, and logs `review-node`.
    /// Returns the new findings' ids, in order.
    ///
    /// # Errors
    ///
    /// Returns a store error; an instance that was never taken is recorded as it ends.
    pub fn finish_review_node(
        &mut self,
        review: &ReviewRow,
        instance: &Instance,
        state: NodeState,
        findings: &[NewFinding<'_>],
        actor: Actor,
    ) -> Result<Vec<i64>, StoreError> {
        let tx = self.conn.transaction()?;
        if node_state_in(&tx, review, instance)?.is_none() {
            tx.execute(
                &format!(
                    "INSERT INTO review_nodes (review_id, round, node, item, state, started_at) \
                     VALUES (?1, ?2, ?3, ?4, 'running', {NOW})"
                ),
                params![
                    review.id,
                    review.round,
                    instance.node.as_str(),
                    instance.item
                ],
            )?;
        }
        tx.execute(
            &format!(
                "UPDATE review_nodes SET state = ?5, finished_at = {NOW} \
                 WHERE review_id = ?1 AND round = ?2 AND node = ?3 AND item = ?4"
            ),
            params![
                review.id,
                review.round,
                instance.node.as_str(),
                instance.item,
                state.as_str()
            ],
        )?;
        let mut ids = Vec::with_capacity(findings.len());
        for finding in findings {
            tx.execute(
                "INSERT INTO review_findings (review_id, round, node, file, line, severity, summary, scenario) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    review.id,
                    review.round,
                    instance.node.as_str(),
                    finding.file,
                    finding.line,
                    finding.severity.as_str(),
                    finding.summary,
                    finding.scenario
                ],
            )?;
            ids.push(tx.last_insert_rowid());
        }
        insert_event(
            &tx,
            actor,
            Some(review.task_id),
            None,
            "review-node",
            &serde_json::json!({
                "review": review.id, "round": review.round, "node": instance.node.as_str(), "item": instance.item,
                "state": state.as_str(), "findings": ids,
            }),
        )?;
        tx.commit()?;
        Ok(ids)
    }

    /// Records a verify node's verdict on one finding.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub fn set_finding_verdict(
        &mut self,
        finding: i64,
        verdict: FindingVerdict,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE review_findings SET verdict = ?2 WHERE id = ?1",
            params![finding, verdict.as_str()],
        )?;
        Ok(())
    }

    /// Records where a finding stands, with a note; `argued` marks that the reviewer lead argued back once.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub fn resolve_finding(
        &mut self,
        finding: i64,
        resolution: Resolution,
        note: Option<&str>,
        argued: bool,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE review_findings SET resolution = ?2, note = COALESCE(?3, note), argued = argued OR ?4 WHERE id = ?1",
            params![finding, resolution.as_str(), note, argued],
        )?;
        Ok(())
    }

    /// Every finding of a review, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a read error, or a stored word this build does not know.
    pub fn review_findings(&self, review: i64) -> Result<Vec<FindingRow>, StoreError> {
        let raws = self
            .conn
            .prepare(&format!(
                "SELECT {FINDING_COLUMNS} FROM review_findings WHERE review_id = ?1 ORDER BY id"
            ))?
            .query_map(params![review], read_finding)?
            .collect::<Result<Vec<_>, _>>()?;
        raws.into_iter().map(into_finding).collect()
    }
}
