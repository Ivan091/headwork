//! The epic runner's store side (`docs/backlog.md`, section 13): the facts one epic's step is decided from, the claim of
//! a child before its spawn, the re-plan child with its `epic_replans` rows, and the churn block.
//!
//! Each write runs in an immediate transaction that reads what it decides on again, so two runners in two processes (a
//! tick and a close's finish) never claim one child twice, add two re-plans or block an epic twice.

use headwork_core::backlog::{
    BacklogActor, BacklogDraft, BacklogItem, BacklogOwner, BacklogStatus, BlockReason, EpicChild,
    EpicFacts, ItemKind, LinkedTask, Target,
};
use headwork_core::{Actor, EXPLORE_KIND};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::backlog::{
    BacklogFilter, BacklogMove, EVENT_BACKLOG_MOVED, ItemShape, add_shaped, check, load, today,
    write_move,
};
use crate::queries::insert_event;
use crate::{NOW, Store, StoreError};

/// The event of a re-plan child the runner added (`epic`, `child`, `reason`, `causes`).
pub const EVENT_EPIC_REPLAN: &str = "epic-replan";

/// The event of an epic the runner blocked for the user's decision after too many re-plans (`epic`, `replans`).
pub const EVENT_EPIC_CHURN: &str = "epic-churn";

/// The start of the block note of an epic blocked for churn: `epic-churn: <n> re-plans`.
pub const CHURN_NOTE: &str = "epic-churn";

/// What a claim of a child found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpicClaim {
    /// The child moved to in progress with no task; the caller spawns its task and links it.
    Claimed(Box<BacklogItem>),
    /// The child is not to start now, and why: another runner took it, the epic moved, the limit is reached.
    Skipped(String),
}

/// A cause of a re-plan as its row records it: the blocked child and the event that blocked it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplanRow {
    /// The blocked child.
    pub item: i64,
    /// The `backlog-moved` event into its block, when one records it.
    pub event: Option<i64>,
}

/// A re-plan child to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReplan<'a> {
    /// The epic.
    pub epic: i64,
    /// The child as drafted, in the backlog.
    pub draft: &'a BacklogDraft,
    /// Its brief and kind (`explore`).
    pub shape: &'a ItemShape,
    /// The reason, one line.
    pub reason: &'a str,
    /// The causes; empty for "plan complete?".
    pub causes: &'a [ReplanRow],
}

fn is_explore(kind: Option<&str>) -> bool {
    kind == Some(EXPLORE_KIND)
}

fn task_kind(conn: &Connection, id: i64) -> Result<Option<String>, StoreError> {
    Ok(conn.query_row(
        "SELECT task_kind FROM backlog_items WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?)
}

/// The id of the newest `backlog-moved` event that moved `item` into a block.
fn latest_block_event(conn: &Connection, item: i64) -> Result<Option<i64>, StoreError> {
    Ok(conn.query_row(
        "SELECT max(id) FROM events WHERE type = ?1 AND json_extract(data, '$.item') = ?2
         AND json_extract(data, '$.to') = 'blocked'",
        params![EVENT_BACKLOG_MOVED, item],
        |row| row.get(0),
    )?)
}

fn block_replanned(conn: &Connection, item: i64, event: Option<i64>) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM epic_replans WHERE cause_item = ?1 AND cause_event IS ?2)",
        params![item, event],
        |row| row.get(0),
    )?)
}

fn counted_replans(conn: &Connection, epic: i64) -> Result<u32, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT count(DISTINCT child_id) FROM epic_replans WHERE epic_id = ?1 AND cleared_at IS NULL",
        params![epic],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

fn explorer_live(conn: &Connection, epic: &BacklogItem) -> Result<bool, StoreError> {
    let by_role: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM tasks WHERE epic_id = ?1 AND state NOT IN ('closed', 'dropped', 'failed'))",
        params![epic.id],
        |row| row.get(0),
    )?;
    Ok(by_role || epic.task.as_ref().is_some_and(LinkedTask::is_live))
}

/// The children of `epic` with `status`, split into explore children and the rest.
fn children_in(conn: &Connection, epic: i64, status: &str) -> Result<(usize, usize), StoreError> {
    let kinds: Vec<Option<String>> = conn
        .prepare("SELECT task_kind FROM backlog_items WHERE parent_id = ?1 AND status = ?2")?
        .query_map(params![epic, status], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let explore = kinds
        .iter()
        .filter(|kind| is_explore(kind.as_deref()))
        .count();
    Ok((explore, kinds.len() - explore))
}

fn open_explore_children(conn: &Connection, epic: i64) -> Result<usize, StoreError> {
    let mut open = 0;
    for status in ["backlog", "in_progress", "blocked"] {
        open += children_in(conn, epic, status)?.0;
    }
    Ok(open)
}

impl Store {
    /// What the runner decides epic `epic`'s step from: whether an explorer of it is live, each child with its status,
    /// kind, owner, first day and block cause, the counted re-plans and today. `runs` says whether headwork runs a project.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownBacklogItem`] when there is no such item, or a read error.
    pub fn epic_facts(
        &self,
        epic: i64,
        runs: impl Fn(&str) -> bool,
    ) -> Result<EpicFacts, StoreError> {
        let item = load(&self.conn, epic)?;
        let filter = BacklogFilter {
            epic: Some(epic),
            ..BacklogFilter::default()
        };
        let mut children = Vec::new();
        for child in self.backlog_items(&filter)? {
            let blocked = child.draft.status.block().is_some();
            let block_event = if blocked {
                latest_block_event(&self.conn, child.id)?
            } else {
                None
            };
            children.push(EpicChild {
                id: child.id,
                explore: is_explore(task_kind(&self.conn, child.id)?.as_deref()),
                agent: child.draft.owner == BacklogOwner::Agent,
                not_before: child.draft.not_before.clone(),
                has_project: runs(&child.draft.project),
                block_replanned: blocked && block_replanned(&self.conn, child.id, block_event)?,
                block_event,
                text: child.draft.text,
                status: child.draft.status,
            });
        }
        Ok(EpicFacts {
            epic,
            explorer_live: explorer_live(&self.conn, &item)?,
            children,
            replans: counted_replans(&self.conn, epic)?,
            today: today(&self.conn)?,
        })
    }

    /// Claims child `child` for a start: in one immediate transaction it reads the child again and moves it from the
    /// backlog to in progress with no task, as headwork, through the status guard. It skips (and changes nothing) unless
    /// the child is in the backlog, its epic is in progress, and, for an explore child, no other explore child of the
    /// epic is in progress, or, for any other child, fewer than `max_parallel` (0: no limit) non-explore children are.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownBacklogItem`] when there is no such item, or a read or write error.
    pub fn claim_epic_child(
        &mut self,
        child: i64,
        max_parallel: u32,
        by: Actor,
    ) -> Result<EpicClaim, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let item = load(&tx, child)?;
        let Some(epic) = item.parent else {
            return Ok(EpicClaim::Skipped(format!(
                "#{child} is not a child of an epic"
            )));
        };
        if item.draft.status != BacklogStatus::Backlog {
            return Ok(EpicClaim::Skipped(format!(
                "#{child} is {}, not in the backlog",
                item.draft.status.kind()
            )));
        }
        let parent = load(&tx, epic)?;
        if parent.kind != ItemKind::Epic || parent.draft.status != BacklogStatus::InProgress {
            return Ok(EpicClaim::Skipped(format!(
                "epic #{epic} is not in progress"
            )));
        }
        let (explore_running, others_running) = children_in(&tx, epic, "in_progress")?;
        if is_explore(task_kind(&tx, child)?.as_deref()) {
            if explore_running > 0 {
                return Ok(EpicClaim::Skipped(format!(
                    "an explore child of epic #{epic} is in progress"
                )));
            }
        } else if max_parallel != 0
            && others_running >= usize::try_from(max_parallel).unwrap_or(usize::MAX)
        {
            return Ok(EpicClaim::Skipped(format!(
                "epic #{epic} has {others_running} children in progress, max_parallel {max_parallel}"
            )));
        }
        let request = BacklogMove::new(Target::InProgress, BacklogActor::Headwork, by, true);
        let step = match check(&tx, &item, &request) {
            Ok(step) => step,
            Err(StoreError::BacklogRefused { reason, .. }) => {
                return Ok(EpicClaim::Skipped(format!("#{child}: {reason}")));
            }
            Err(error) => return Err(error),
        };
        let claimed = write_move(&tx, &item, &step, &request)?;
        tx.commit()?;
        Ok(EpicClaim::Claimed(Box::new(claimed)))
    }

    /// Adds a re-plan child to an epic and records one `epic_replans` row per cause (one with no cause for "plan
    /// complete?"), logging `backlog-added` and `epic-replan`, in one immediate transaction. Adds nothing and returns
    /// `None` when the epic is not in progress or an explore child of it is already open (another runner added one).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownBacklogItem`] when there is no such epic, [`StoreError::BacklogAddRefused`] or
    /// [`StoreError::InvalidBacklogItem`] for a draft the store refuses, or a read or write error.
    pub fn add_epic_replan(
        &mut self,
        replan: &NewReplan<'_>,
        by: Actor,
    ) -> Result<Option<BacklogItem>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let epic = load(&tx, replan.epic)?;
        if epic.draft.status != BacklogStatus::InProgress
            || open_explore_children(&tx, replan.epic)? > 0
        {
            return Ok(None);
        }
        let child = add_shaped(
            &tx,
            replan.draft,
            by,
            BacklogActor::Headwork,
            None,
            replan.shape,
        )?;
        let rows: Vec<(Option<i64>, Option<i64>)> = if replan.causes.is_empty() {
            vec![(None, None)]
        } else {
            replan
                .causes
                .iter()
                .map(|cause| (Some(cause.item), cause.event))
                .collect()
        };
        for (item, event) in rows {
            tx.execute(
                &format!(
                    "INSERT INTO epic_replans (epic_id, child_id, cause_item, cause_event, reason, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, {NOW})"
                ),
                params![replan.epic, child.id, item, event, replan.reason],
            )?;
        }
        let causes: Vec<i64> = replan.causes.iter().map(|cause| cause.item).collect();
        insert_event(
            &tx,
            by,
            None,
            None,
            EVENT_EPIC_REPLAN,
            &serde_json::json!({
                "epic": replan.epic,
                "child": child.id,
                "reason": replan.reason,
                "causes": causes,
            }),
        )?;
        tx.commit()?;
        Ok(Some(child))
    }

    /// Blocks an epic in progress for the user's decision after `replans` re-plans (note `epic-churn: <n> re-plans`),
    /// clears its counted `epic_replans` rows and logs `epic-churn`, in one immediate transaction. Returns `None` when the
    /// epic is no longer in progress (another runner blocked it first).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownBacklogItem`] when there is no such item, [`StoreError::BacklogRefused`] when the guard
    /// refuses, or a read or write error.
    pub fn block_epic_churn(
        &mut self,
        epic: i64,
        replans: u32,
        by: Actor,
    ) -> Result<Option<BacklogItem>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let item = load(&tx, epic)?;
        if item.draft.status != BacklogStatus::InProgress {
            return Ok(None);
        }
        let note = format!("{CHURN_NOTE}: {replans} re-plans");
        let request = BacklogMove::new(
            Target::Blocked {
                reason: BlockReason::Decision,
                note: Some(note),
            },
            BacklogActor::Headwork,
            by,
            true,
        );
        let step = check(&tx, &item, &request)?;
        let blocked = write_move(&tx, &item, &step, &request)?;
        tx.execute(
            &format!(
                "UPDATE epic_replans SET cleared_at = {NOW} WHERE epic_id = ?1 AND cleared_at IS NULL"
            ),
            params![epic],
        )?;
        insert_event(
            &tx,
            by,
            None,
            None,
            EVENT_EPIC_CHURN,
            &serde_json::json!({ "epic": epic, "replans": replans }),
        )?;
        tx.commit()?;
        Ok(Some(blocked))
    }

    /// Whether an event of `kind` names backlog item `item` (`data.item`).
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn item_event_logged(&self, kind: &str, item: i64) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM events WHERE type = ?1 AND json_extract(data, '$.item') = ?2 LIMIT 1",
                params![kind, item],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    }
}
