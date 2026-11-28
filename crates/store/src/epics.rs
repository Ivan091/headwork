//! The epic runner's store side (`docs/backlog.md`, section 13): the facts one epic's step is decided from, the claim of
//! a child before its spawn, the re-plan child with its `epic_replans` rows, and the churn block.
//!
//! Each write runs in an immediate transaction that reads what it decides on again, so two runners in two processes (a
//! tick and a close's finish) never claim one child twice, add two re-plans or block an epic twice.

use headwork_core::backlog::{
    BacklogActor, BacklogDraft, BacklogItem, BacklogOwner, BacklogStatus, BlockReason, EpicChild,
    EpicFacts, ItemKind, ItemStatus, LinkedTask, Target, holds_triggers,
};
use headwork_core::{Actor, EXPLORE_KIND};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::backlog::{
    BacklogFilter, BacklogMove, EVENT_BACKLOG_MOVED, ItemShape, add_shaped, check, load, today,
    write_move,
};
use crate::queries::insert_event;
use crate::{NOW, Store, StoreError, TaskId};

/// The event of a re-plan child the runner added (`epic`, `child`, `reason`, `causes`).
pub const EVENT_EPIC_REPLAN: &str = "epic-replan";

/// The event of an epic the runner blocked for the user's decision after too many re-plans (`epic`, `replans`).
pub const EVENT_EPIC_CHURN: &str = "epic-churn";

/// The event of a runner step that failed (`epic`, `child`, `error`); one is logged again only after a pass of that
/// epic went through without an error ([`EVENT_EPIC_RUNNER_OK`]).
pub const EVENT_EPIC_RUNNER_ERROR: &str = "epic-runner-error";

/// The event of an epic's first pass without an error after one that failed (`epic`): it ends the tick's
/// `epic-runner-error` finding and lets the same error be logged again.
pub const EVENT_EPIC_RUNNER_OK: &str = "epic-runner-ok";

/// The start of the block note of an epic blocked for churn: `epic-churn: <n> re-plans`. Only text for the user: the
/// churn block is recorded in `epic_churn_blocks`, never read back from the note.
const CHURN_NOTE: &str = "epic-churn";

/// The reason recorded for an explore child an explorer planned itself, which counts toward `max_replans` like a
/// re-plan a trigger added.
const PLANNED_EXPLORE: &str = "explore child planned by an explorer";

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

/// How many explore children of `epic` hold its re-plan triggers ([`holds_triggers`]).
fn trigger_holders(conn: &Connection, epic: i64) -> Result<usize, StoreError> {
    let rows: Vec<(Option<String>, String)> = conn
        .prepare("SELECT task_kind, status FROM backlog_items WHERE parent_id = ?1")?
        .query_map(params![epic], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut held = 0;
    for (kind, status) in rows {
        if holds_triggers(is_explore(kind.as_deref()), status.parse::<ItemStatus>()?) {
            held += 1;
        }
    }
    Ok(held)
}

impl Store {
    /// What the runner decides epic `epic`'s step from: whether an explorer of it is live, each child with its status,
    /// kind, owner, first day and block cause, the counted re-plans and today. `skip` says why the runner may not start a
    /// child of a project and task kind (as stored, none when unset), or nothing when it may.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownBacklogItem`] when there is no such item, or a read error.
    pub fn epic_facts(
        &self,
        epic: i64,
        skip: impl Fn(&str, Option<&str>) -> Option<String>,
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
            let kind = task_kind(&self.conn, child.id)?;
            children.push(EpicChild {
                id: child.id,
                explore: is_explore(kind.as_deref()),
                agent: child.draft.owner == BacklogOwner::Agent,
                not_before: child.draft.not_before.clone(),
                skip: skip(&child.draft.project, kind.as_deref()),
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
    /// the child is in the backlog, its epic is in progress, no explore child of the epic is in progress (it holds the
    /// epic, its task perhaps not yet created), and, for a child that is not `explore`, fewer than `max_parallel` (0: no
    /// limit) non-explore children are. An explore child no trigger added (an explorer planned it) records an
    /// `epic_replans` row with no cause, so a chain of explorers counts toward `max_replans`.
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
        if explore_running > 0 {
            return Ok(EpicClaim::Skipped(format!(
                "an explore child of epic #{epic} is in progress: it holds the epic"
            )));
        }
        let explore = is_explore(task_kind(&tx, child)?.as_deref());
        if !explore
            && max_parallel != 0
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
        if explore {
            tx.execute(
                &format!(
                    "INSERT INTO epic_replans (epic_id, child_id, cause_item, cause_event, reason, created_at)
                     SELECT ?1, ?2, NULL, NULL, ?3, {NOW}
                     WHERE NOT EXISTS (SELECT 1 FROM epic_replans WHERE child_id = ?2)"
                ),
                params![epic, child, PLANNED_EXPLORE],
            )?;
        }
        tx.commit()?;
        Ok(EpicClaim::Claimed(Box::new(claimed)))
    }

    /// Adds a re-plan child to an epic and records one `epic_replans` row per cause (one with no cause for "plan
    /// complete?"), logging `backlog-added` and `epic-replan`, in one immediate transaction. Adds nothing and returns
    /// `None` when the epic is not in progress or an explore child of it holds the triggers (in the backlog or in
    /// progress, [`holds_triggers`]: another runner added one).
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
        if epic.draft.status != BacklogStatus::InProgress || trigger_holders(&tx, replan.epic)? > 0
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
        if let Some(event) = latest_block_event(&tx, epic)? {
            tx.execute(
                &format!("INSERT INTO epic_churn_blocks (epic_id, event_id, created_at) VALUES (?1, ?2, {NOW})"),
                params![epic, event],
            )?;
        }
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

    /// Whether epic `epic` is blocked by the runner's churn cap: blocked, and the `backlog-moved` event into its current
    /// block is one [`Store::block_epic_churn`] recorded in `epic_churn_blocks`. A block by anyone else, whatever its note,
    /// is not.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownBacklogItem`] when there is no such item, or a read error.
    pub fn epic_churn_blocked(&self, epic: i64) -> Result<bool, StoreError> {
        if load(&self.conn, epic)?.draft.status.block().is_none() {
            return Ok(false);
        }
        let Some(event) = latest_block_event(&self.conn, epic)? else {
            return Ok(false);
        };
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM epic_churn_blocks WHERE epic_id = ?1 AND event_id = ?2)",
            params![epic, event],
            |row| row.get(0),
        )?)
    }

    /// The children of `epic` headwork claimed more than `grace_s` seconds ago that are still in progress with no task:
    /// the newest move of each into `in_progress` was headwork's, and no link followed. A child a user moved there by
    /// hand is not one.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn stale_epic_claims(&self, epic: i64, grace_s: u32) -> Result<Vec<i64>, StoreError> {
        Ok(self
            .conn
            .prepare(
                "SELECT t.id FROM backlog_items t
                 JOIN events e ON e.id = (
                     SELECT max(id) FROM events WHERE type = ?2 AND json_extract(data, '$.item') = t.id
                     AND json_extract(data, '$.to') = 'in_progress')
                 WHERE t.parent_id = ?1 AND t.status = 'in_progress' AND t.task_id IS NULL
                 AND json_extract(e.data, '$.actor') = 'headwork'
                 AND e.at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-' || ?3 || ' seconds')
                 ORDER BY t.id",
            )?
            .query_map(params![epic, EVENT_BACKLOG_MOVED, grace_s], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    }

    /// The newest live task of `project` whose slug is `prefix` or starts with `<prefix>-`: the task the runner named
    /// for a child (`e<epic>-<child>`).
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn live_task_with_slug_prefix(
        &self,
        project: &str,
        prefix: &str,
    ) -> Result<Option<TaskId>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM tasks WHERE project = ?1 AND (slug = ?2 OR slug LIKE ?2 || '-%')
                 AND state NOT IN ('closed', 'dropped', 'failed') ORDER BY id DESC LIMIT 1",
                params![project, prefix],
                |row| row.get(0),
            )
            .optional()?
            .map(TaskId))
    }

    /// Whether the runner already logged `error` for `epic` and `child` since the epic's last error-free pass, so it is
    /// not logged again.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn runner_error_logged(
        &self,
        epic: Option<i64>,
        child: Option<i64>,
        error: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE type = ?1 AND json_extract(data, '$.epic') IS ?3
             AND json_extract(data, '$.child') IS ?4 AND json_extract(data, '$.error') = ?5
             AND id > coalesce((SELECT max(id) FROM events WHERE type = ?2 AND json_extract(data, '$.epic') IS ?3), 0))",
            params![EVENT_EPIC_RUNNER_ERROR, EVENT_EPIC_RUNNER_OK, epic, child, error],
            |row| row.get(0),
        )?)
    }

    /// The error of `epic`'s newest runner status event when that is an [`EVENT_EPIC_RUNNER_ERROR`] (not followed by an
    /// [`EVENT_EPIC_RUNNER_OK`]): the epic's runner is failing.
    ///
    /// # Errors
    ///
    /// Returns a read error, or the JSON error of a bad event.
    pub fn runner_failing(&self, epic: i64) -> Result<Option<String>, StoreError> {
        let latest: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT type, data FROM events WHERE type IN (?1, ?2) AND json_extract(data, '$.epic') = ?3
                 ORDER BY id DESC LIMIT 1",
                params![EVENT_EPIC_RUNNER_ERROR, EVENT_EPIC_RUNNER_OK, epic],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((kind, data)) = latest.filter(|(kind, _)| kind == EVENT_EPIC_RUNNER_ERROR) else {
            return Ok(None);
        };
        drop(kind);
        let data: serde_json::Value = serde_json::from_str(&data)?;
        Ok(Some(
            data.get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ))
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
