//! The backlog list (`docs/backlog.md`): items, their link to the task spawned for them, and the record of the file they are
//! imported from and exported to.
//!
//! `backlog_items.position` orders the export: an import keeps file order, a new item goes last. `backlog_items.notes` holds the
//! sub-bullets one per line. `backlog_files` remembers, per path, when the file was imported and the hash and time of the
//! last export, so an export can tell that the file was edited by hand since.

use std::collections::HashSet;

use headwork_core::backlog::{
    BacklogActor, BacklogDate, BacklogDraft, BacklogItem, BacklogOwner, BacklogStatus, DateEffect,
    DepNode, ItemKind, ItemMove, ItemStatus, LinkEffect, LinkedEnd, LinkedTask, MoveFacts,
    MoveRefusal, ParentFacts, Target, TaskEndEffect, TaskOutcome, WaitingDependencies,
    check_dependency, check_parent, dependency_block, identity, on_linked_end, on_task_end,
    transition,
};
use headwork_core::{Actor, TaskState};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::Serialize;

use crate::queries::insert_event;
use crate::{NOW, Store, StoreError, Task, TaskId};

/// The event of a new item.
pub const EVENT_BACKLOG_ADDED: &str = "backlog-added";

/// The event an exploration's `Backlog` ask leaves: how many candidates the user picked (`{"count": N, "picks": [...]}`).
pub const EVENT_BACKLOG_PICKED: &str = "backlog-picked";

/// The event of an item done, as written before stored statuses (2026-11-27); a move now logs [`EVENT_BACKLOG_MOVED`].
pub const EVENT_BACKLOG_DONE: &str = "backlog-done";

/// The event of an item dropped, as written before stored statuses (2026-11-27); a cancel now logs
/// [`EVENT_BACKLOG_MOVED`].
pub const EVENT_BACKLOG_DROPPED: &str = "backlog-dropped";

/// The event of an item's status moved (`item`, `from`, `to`, `actor`, `reason`, `kind`, `task`), in the same transaction
/// as the move.
pub const EVENT_BACKLOG_MOVED: &str = "backlog-moved";

/// The event of a move headwork wanted at a task's end and the guard refused (`item`, `reason`); the task's transition
/// stands.
pub const EVENT_BACKLOG_MOVE_REFUSED: &str = "backlog-move-refused";

/// The event of a worker's `done` that said it did not finish the work (`outcome`, `note`, `items`).
pub(crate) const EVENT_TASK_OUTCOME: &str = "task-outcome";

/// The event of a dependency added (`item`, `depends_on`).
pub const EVENT_BACKLOG_DEPENDED: &str = "backlog-depended";

/// The event of a dependency removed (`item`, `depends_on`).
pub const EVENT_BACKLOG_UNDEPENDED: &str = "backlog-undepended";

/// The event of an item's fields changed.
pub const EVENT_BACKLOG_EDITED: &str = "backlog-edited";

/// The event of a task recorded as the explorer of an epic (`epic`).
pub const EVENT_TASK_EPIC: &str = "task-epic";

/// The event of an import.
pub const EVENT_BACKLOG_IMPORTED: &str = "backlog-imported";

/// The event of an export that wrote the file.
pub const EVENT_BACKLOG_EXPORTED: &str = "backlog-exported";

const TODAY: &str = "date('now', 'localtime')";

const COLUMNS: &str = "t.id, t.project, t.section, t.text, t.notes, t.status, t.owner, t.added, t.not_before, t.due, \
                       t.done_on, t.done_note, t.task_id, k.project, k.slug, k.state, t.position, t.blocked_reason, \
                       t.blocked_note, t.status_before_block, t.kind, t.parent_id";

/// Which items to list; an absent field matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BacklogFilter {
    /// Only this project.
    pub project: Option<String>,
    /// Only this owner.
    pub owner: Option<BacklogOwner>,
    /// Only these statuses; empty matches every status.
    pub statuses: Vec<ItemStatus>,
    /// Only the children of this epic.
    pub epic: Option<i64>,
}

/// Where a new item sits: its kind, its epic, the items it waits for, and what its explorer planned for its spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemShape {
    /// An item, or an epic.
    pub kind: Option<ItemKind>,
    /// The epic it belongs to.
    pub parent: Option<i64>,
    /// The items it depends on.
    pub after: Vec<i64>,
    /// The brief, task kind, model and review level of an epic's child.
    pub plan: ItemPlan,
}

/// What a spawn of an epic's child needs, written by the epic's explorer (`docs/backlog.md`, section 12). On an edit, a
/// field that is `None` stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ItemPlan {
    /// The full task brief.
    pub brief: Option<String>,
    /// The task kind, such as `code` or `explore`.
    pub task_kind: Option<String>,
    /// The model; the kind's default when absent.
    pub model: Option<String>,
    /// The planned review level, 0 to 3.
    pub review: Option<u8>,
}

/// A move of an item's status, checked by [`transition`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogMove<'a> {
    /// Where it goes.
    pub target: Target,
    /// Who moves it, for the guard.
    pub actor: BacklogActor,
    /// Who moves it, for the event log.
    pub by: Actor,
    /// The acting worker's or explorer's own task. The worker item tools fill it in, for the guard's worker rows.
    pub actor_task: Option<TaskId>,
    /// Whether a task can work on it; the caller knows the configured projects, or a spawn names one.
    pub has_project: bool,
    /// The note of a finish, the reason of a cancel, or why the move was made; logged with the move.
    pub reason: Option<&'a str>,
    /// The task a start, relink or respawn links it to.
    pub link: Option<TaskId>,
    /// The acting explorer's epic: its children are in its reach, and the epic itself only for `plan_finish`.
    pub actor_epic: Option<i64>,
}

impl<'a> BacklogMove<'a> {
    /// A move by `actor` (logged as `by`) to `target`, with no reason, link or own task; `has_project` says whether a task
    /// can work on the item.
    #[must_use]
    pub const fn new(target: Target, actor: BacklogActor, by: Actor, has_project: bool) -> Self {
        Self {
            target,
            actor,
            by,
            actor_task: None,
            has_project,
            reason: None,
            link: None,
            actor_epic: None,
        }
    }

    /// The same move with a reason.
    #[must_use]
    pub fn with_reason(self, reason: Option<&'a str>) -> Self {
        Self { reason, ..self }
    }
}

/// A change to an item; an absent field stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BacklogEdit {
    /// The new text.
    pub text: Option<String>,
    /// The new project.
    pub project: Option<String>,
    /// The new section; `Some(None)` takes it out of its section.
    pub section: Option<Option<String>>,
    /// The new owner.
    pub owner: Option<BacklogOwner>,
    /// The new first day; `Some(None)` clears it.
    pub not_before: Option<Option<BacklogDate>>,
    /// The new due day; `Some(None)` clears it.
    pub due: Option<Option<BacklogDate>>,
    /// A note line to add.
    pub append: Option<String>,
    /// The day it was added, when the new text says so in an `(added D)` group.
    pub added: Option<BacklogDate>,
    /// The brief, task kind, model and review level to set; a field that is `None` stays.
    pub plan: ItemPlan,
}

/// What an import did, or would do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ImportCounts {
    /// Items new to the store.
    pub added: usize,
    /// Items whose text the store already holds, in any state.
    pub already: usize,
    /// Items whose text an earlier line of the same file already had.
    pub duplicate: usize,
}

/// The record of one `backlog.md` path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BacklogFile {
    /// The path, with forward slashes.
    pub path: String,
    /// When it was imported, UTC.
    pub imported_at: Option<String>,
    /// The hash of the last export's content.
    pub export_hash: Option<String>,
    /// When it was last exported, UTC.
    pub exported_at: Option<String>,
    /// When it was last exported as the configured `[backlog] file`, UTC; an export to `--file` leaves it alone.
    pub configured_at: Option<String>,
}

struct RawBacklogItem {
    id: i64,
    project: String,
    section: Option<String>,
    text: String,
    notes: Option<String>,
    status: String,
    block: (Option<String>, Option<String>, Option<String>),
    owner: String,
    dates: [Option<String>; 4],
    done_note: Option<String>,
    task: Option<(i64, String, String, String)>,
    position: i64,
    kind: String,
    parent: Option<i64>,
}

impl RawBacklogItem {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        let task_id: Option<i64> = row.get(12)?;
        let task = match task_id {
            Some(id) => Some((id, row.get(13)?, row.get(14)?, row.get(15)?)),
            None => None,
        };
        Ok(Self {
            id: row.get(0)?,
            project: row.get(1)?,
            section: row.get(2)?,
            text: row.get(3)?,
            notes: row.get(4)?,
            status: row.get(5)?,
            block: (row.get(17)?, row.get(18)?, row.get(19)?),
            owner: row.get(6)?,
            dates: [row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?],
            done_note: row.get(11)?,
            task,
            position: row.get(16)?,
            kind: row.get(20)?,
            parent: row.get(21)?,
        })
    }

    fn into_backlog_item(self, conn: &Connection) -> Result<BacklogItem, StoreError> {
        let id = self.id;
        let mut item = self.parse().map_err(|source| StoreError::BadBacklogRow {
            id,
            source: Box::new(source),
        })?;
        item.deps = dependencies(conn, id)?;
        Ok(item)
    }

    fn parse(self) -> Result<BacklogItem, StoreError> {
        let [added, not_before, due, done_on] = self.dates;
        let date = |text: Option<String>| text.map(|text| BacklogDate::parse(&text)).transpose();
        let task = self
            .task
            .map(|(id, project, slug, state)| {
                Ok::<_, StoreError>(LinkedTask {
                    id,
                    name: format!("{project}-{slug}"),
                    state: state.parse()?,
                })
            })
            .transpose()?;
        Ok(BacklogItem {
            id: self.id,
            position: self.position,
            task,
            kind: self.kind.parse()?,
            parent: self.parent,
            deps: Vec::new(),
            draft: BacklogDraft {
                project: self.project,
                section: self.section,
                text: self.text,
                notes: self
                    .notes
                    .map(|notes| notes.lines().map(str::to_owned).collect())
                    .unwrap_or_default(),
                status: {
                    let (reason, note, before) = self.block;
                    BacklogStatus::from_columns(
                        &self.status,
                        reason.as_deref(),
                        note,
                        before.as_deref(),
                    )?
                },
                owner: self.owner.parse()?,
                added: date(added)?,
                not_before: date(not_before)?,
                due: date(due)?,
                done_on: date(done_on)?,
                done_note: self.done_note,
            },
        })
    }
}

pub(crate) fn load(conn: &Connection, id: i64) -> Result<BacklogItem, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM backlog_items t LEFT JOIN tasks k ON k.id = t.task_id WHERE t.id = ?1"
        ),
        params![id],
        RawBacklogItem::read,
    )
    .optional()?
    .ok_or(StoreError::UnknownBacklogItem(id))?
    .into_backlog_item(conn)
}

fn dependencies(conn: &Connection, id: i64) -> Result<Vec<i64>, StoreError> {
    Ok(conn
        .prepare(
            "SELECT depends_on_id FROM backlog_deps WHERE item_id = ?1 ORDER BY depends_on_id",
        )?
        .query_map(params![id], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

fn dependants(conn: &Connection, id: i64) -> Result<Vec<i64>, StoreError> {
    Ok(conn
        .prepare("SELECT item_id FROM backlog_deps WHERE depends_on_id = ?1 ORDER BY item_id")?
        .query_map(params![id], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

fn edges(conn: &Connection) -> Result<Vec<(i64, i64)>, StoreError> {
    Ok(conn
        .prepare("SELECT item_id, depends_on_id FROM backlog_deps")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

fn dependency_statuses(conn: &Connection, id: i64) -> Result<Vec<(i64, ItemStatus)>, StoreError> {
    let rows = conn
        .prepare(
            "SELECT t.id, t.status FROM backlog_deps d JOIN backlog_items t ON t.id = d.depends_on_id
             WHERE d.item_id = ?1 ORDER BY t.id",
        )?
        .query_map(params![id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(dep, status)| Ok((dep, status.parse::<ItemStatus>()?)))
        .collect()
}

pub(crate) fn open_children(conn: &Connection, epic: i64) -> Result<Vec<i64>, StoreError> {
    Ok(conn
        .prepare(
            "SELECT id FROM backlog_items WHERE parent_id = ?1 AND status IN ('backlog', 'in_progress', 'blocked')
             ORDER BY position, id",
        )?
        .query_map(params![epic], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

pub(crate) fn today(conn: &Connection) -> Result<BacklogDate, StoreError> {
    let text: String = conn.query_row(&format!("SELECT {TODAY}"), [], |row| row.get(0))?;
    Ok(BacklogDate::parse(&text)?)
}

fn block_columns(
    status: &BacklogStatus,
) -> (Option<&'static str>, Option<&str>, Option<&'static str>) {
    status.block().map_or((None, None, None), |block| {
        (
            Some(block.reason.as_str()),
            block.note.as_deref(),
            Some(block.before.as_str()),
        )
    })
}

fn insert(
    tx: &Transaction<'_>,
    draft: &BacklogDraft,
    kind: ItemKind,
    parent: Option<i64>,
    plan: &ItemPlan,
) -> Result<i64, StoreError> {
    draft.validate().map_err(StoreError::InvalidBacklogItem)?;
    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM backlog_items",
        [],
        |row| row.get(0),
    )?;
    let notes = (!draft.notes.is_empty()).then(|| draft.notes.join("\n"));
    let (reason, note, before) = block_columns(&draft.status);
    tx.execute(
        &format!(
            "INSERT INTO backlog_items (project, section, text, notes, status, owner, added, not_before, due, done_on, done_note,
                                position, blocked_reason, blocked_note, status_before_block, kind, parent_id, brief, task_kind,
                                model, review, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, {NOW}, {NOW})"
        ),
        params![
            draft.project,
            draft.section,
            draft.text,
            notes,
            draft.status.kind().as_str(),
            draft.owner.as_str(),
            draft.added.as_ref().map(BacklogDate::as_str),
            draft.not_before.as_ref().map(BacklogDate::as_str),
            draft.due.as_ref().map(BacklogDate::as_str),
            draft.done_on.as_ref().map(BacklogDate::as_str),
            draft.done_note,
            position,
            reason,
            note,
            before,
            kind.as_str(),
            parent,
            plan.brief,
            plan.task_kind,
            plan.model,
            plan.review,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

fn event(
    tx: &Transaction<'_>,
    actor: Actor,
    task: Option<TaskId>,
    kind: &str,
    item: &BacklogItem,
) -> Result<(), StoreError> {
    let data = serde_json::json!({
        "id": item.id,
        "project": item.draft.project,
        "text": item.draft.text,
        "status": item.draft.status.kind(),
    });
    insert_event(tx, actor, task, None, kind, &data)
}

fn counts(
    conn: &Connection,
    drafts: &[BacklogDraft],
) -> Result<(ImportCounts, Vec<bool>), StoreError> {
    let mut known: HashSet<String> = HashSet::new();
    let mut statement = conn.prepare("SELECT text FROM backlog_items")?;
    for text in statement.query_map([], |row| row.get::<_, String>(0))? {
        known.insert(identity(&text?));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut counts = ImportCounts::default();
    let mut fresh = Vec::with_capacity(drafts.len());
    for draft in drafts {
        let key = identity(&draft.text);
        let is_new = if !seen.insert(key.clone()) {
            counts.duplicate += 1;
            false
        } else if known.contains(&key) {
            counts.already += 1;
            false
        } else {
            counts.added += 1;
            true
        };
        fresh.push(is_new);
    }
    Ok((counts, fresh))
}

fn facts<'a>(
    item: &'a BacklogItem,
    request: &BacklogMove<'a>,
    waiting: &WaitingDependencies,
    open_children: usize,
) -> MoveFacts<'a> {
    let live_task = item
        .task
        .as_ref()
        .filter(|task| task.is_live())
        .map(|task| task.name.as_str());
    MoveFacts {
        live_task,
        linked_to_actor: request
            .actor_task
            .is_some_and(|own| item.task.as_ref().is_some_and(|task| task.id == own.0)),
        in_actor_epic: request
            .actor_epic
            .is_some_and(|epic| item.parent == Some(epic)),
        is_actor_epic: request.actor_epic == Some(item.id),
        open_children,
        is_epic: item.kind == ItemKind::Epic,
        has_project: request.has_project,
        links_another_task: request.link.is_some_and(|link| {
            item.task
                .as_ref()
                .is_some_and(|task| task.is_live() && task.id != link.0)
        }),
        open_dependencies: waiting.open.len(),
        cancelled_dependencies: waiting.cancelled.len(),
        reason: request.reason,
    }
}

pub(crate) fn check(
    conn: &Connection,
    item: &BacklogItem,
    request: &BacklogMove<'_>,
) -> Result<ItemMove, StoreError> {
    let waiting = WaitingDependencies::of(&dependency_statuses(conn, item.id)?);
    let children = if item.kind == ItemKind::Epic {
        open_children(conn, item.id)?.len()
    } else {
        0
    };
    transition(
        Some(&item.draft.status),
        &request.target,
        request.actor,
        &facts(item, request, &waiting, children),
    )
    .map_err(|refusal| {
        let reason = if matches!(refusal, MoveRefusal::Dependencies { .. }) {
            format!(
                "it waits on {}: finish them, or remove the dependencies",
                waiting.describe()
            )
        } else {
            refusal.to_string()
        };
        StoreError::BacklogRefused {
            id: item.id,
            reason,
        }
    })
}

/// Writes an allowed move and its `backlog-moved` event, applying the move's link and date effects ([`ItemMove`]).
pub(crate) fn write_move(
    tx: &Transaction<'_>,
    before: &BacklogItem,
    step: &ItemMove,
    request: &BacklogMove<'_>,
) -> Result<BacklogItem, StoreError> {
    let id = before.id;
    let old_task = before.task.as_ref().map(|task| task.id);
    let task = match step.link {
        LinkEffect::Keep => old_task,
        LinkEffect::Clear => None,
        LinkEffect::Set => request.link.map(|task| task.0),
    };
    let (done_on, done_note) = match step.dates {
        DateEffect::Keep => (before.draft.done_on.clone(), before.draft.done_note.clone()),
        DateEffect::Stamp => (Some(today(tx)?), request.reason.map(str::to_owned)),
        DateEffect::Clear => (None, None),
        DateEffect::Renote => (
            before.draft.done_on.clone(),
            request
                .reason
                .map(str::to_owned)
                .or_else(|| before.draft.done_note.clone()),
        ),
    };
    let (reason, note, before_block) = block_columns(&step.to);
    tx.execute(
        &format!(
            "UPDATE backlog_items SET status = ?2, blocked_reason = ?3, blocked_note = ?4, status_before_block = ?5,
                                      task_id = ?6, done_on = ?7, done_note = ?8, updated_at = {NOW}
             WHERE id = ?1"
        ),
        params![
            id,
            step.to.kind().as_str(),
            reason,
            note,
            before_block,
            task,
            done_on.as_ref().map(BacklogDate::as_str),
            done_note,
        ],
    )?;
    let data = serde_json::json!({
        "item": id,
        "from": step.from,
        "to": step.to.kind(),
        "actor": request.actor,
        "reason": request.reason.or_else(|| step.to.block().and_then(|block| block.note.as_deref())),
        "kind": step.kind,
        "blocked_reason": step.to.block().map(|block| block.reason),
        "task": task.or(old_task),
    });
    insert_event(
        tx,
        request.by,
        task.or(old_task).map(TaskId).or(request.actor_task),
        None,
        EVENT_BACKLOG_MOVED,
        &data,
    )?;
    for dependant in dependants(tx, id)? {
        settle_dependency_block(tx, dependant, request.by)?;
    }
    settle_dependency_block(tx, id, request.by)?;
    load(tx, id)
}

/// Moves a dependant as headwork, in the transaction of the move that caused it; a refusal is logged as
/// `backlog-move-refused` and never fails the move.
fn cascade_move(
    tx: &Transaction<'_>,
    id: i64,
    target: Target,
    by: Actor,
) -> Result<(), StoreError> {
    let item = load(tx, id)?;
    let request = BacklogMove::new(target, BacklogActor::Headwork, by, true);
    match check(tx, &item, &request) {
        Ok(step) => write_move(tx, &item, &step, &request).map(drop),
        Err(error) => insert_event(
            tx,
            by,
            None,
            None,
            EVENT_BACKLOG_MOVE_REFUSED,
            &serde_json::json!({ "item": id, "reason": error.to_string() }),
        ),
    }
}

/// Makes the block of an item match its dependencies, as headwork, in the transaction that changed them: an item in the
/// backlog, or blocked for `depends` or `dep_cancelled`, is blocked `dep_cancelled` while a dependency is cancelled, else
/// blocked `depends` while one is not done, else it goes back to the status it had. Items in progress, done, cancelled or
/// blocked for another reason are left alone. Called after every move of an item (for its dependants and itself), after
/// a dependency is added or removed.
fn settle_dependency_block(tx: &Transaction<'_>, id: i64, by: Actor) -> Result<(), StoreError> {
    let status = load(tx, id)?.draft.status;
    dependency_block(&status, &dependency_statuses(tx, id)?)
        .map_or(Ok(()), |target| cascade_move(tx, id, target, by))
}

fn dep_event(
    tx: &Transaction<'_>,
    by: Actor,
    task: Option<TaskId>,
    kind: &str,
    item: i64,
    dep: i64,
) -> Result<(), StoreError> {
    insert_event(
        tx,
        by,
        task,
        None,
        kind,
        &serde_json::json!({ "item": item, "depends_on": dep }),
    )
}

const fn refused(id: i64, reason: String) -> StoreError {
    StoreError::BacklogRefused { id, reason }
}

/// Records "`item` depends on `dep`" after the dependency guard, and blocks an item in the backlog for `depends` while the
/// dependency is not done. An item already blocked keeps its block; an edge that exists is left as it is.
fn add_dependency(
    tx: &Transaction<'_>,
    item: i64,
    dep: i64,
    by: Actor,
    task: Option<TaskId>,
) -> Result<(), StoreError> {
    let (from, to) = (load(tx, item)?, load(tx, dep)?);
    if from.deps.contains(&dep) {
        return Ok(());
    }
    check_dependency(
        &edges(tx)?,
        DepNode {
            id: item,
            status: from.draft.status.kind(),
            returns_to: from.draft.status.block().map(|block| block.before),
        },
        DepNode {
            id: dep,
            status: to.draft.status.kind(),
            returns_to: to.draft.status.block().map(|block| block.before),
        },
    )
    .map_err(|refusal| refused(item, refusal.to_string()))?;
    tx.execute(
        &format!(
            "INSERT INTO backlog_deps (item_id, depends_on_id, added_by_task, created_at) VALUES (?1, ?2, ?3, {NOW})"
        ),
        params![item, dep, task.map(|task| task.0)],
    )?;
    dep_event(tx, by, task, EVENT_BACKLOG_DEPENDED, item, dep)?;
    settle_dependency_block(tx, item, by)?;
    Ok(())
}

/// Removes "`item` depends on `dep`" and settles the dependency block of the item again.
fn remove_dependency(
    tx: &Transaction<'_>,
    item: i64,
    dep: i64,
    by: Actor,
    task: Option<TaskId>,
) -> Result<(), StoreError> {
    load(tx, item)?;
    load(tx, dep)?;
    let removed = tx.execute(
        "DELETE FROM backlog_deps WHERE item_id = ?1 AND depends_on_id = ?2",
        params![item, dep],
    )?;
    if removed == 0 {
        return Err(refused(item, format!("it does not depend on #{dep}")));
    }
    dep_event(tx, by, task, EVENT_BACKLOG_UNDEPENDED, item, dep)?;
    settle_dependency_block(tx, item, by)?;
    Ok(())
}

fn linked_ids(
    tx: &Transaction<'_>,
    task: TaskId,
    status: ItemStatus,
) -> Result<Vec<i64>, StoreError> {
    Ok(tx
        .prepare("SELECT id FROM backlog_items WHERE task_id = ?1 AND status = ?2 ORDER BY id")?
        .query_map(params![task.0, status.as_str()], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

/// Moves one item for a task's state through the guard as headwork; a refusal is logged as `backlog-move-refused`.
fn headwork_move(
    tx: &Transaction<'_>,
    task: &Task,
    id: i64,
    request: &BacklogMove<'_>,
) -> Result<(), StoreError> {
    let item = load(tx, id)?;
    match check(tx, &item, request) {
        Ok(step) => write_move(tx, &item, &step, request).map(drop),
        Err(error) => insert_event(
            tx,
            request.by,
            Some(task.id),
            None,
            EVENT_BACKLOG_MOVE_REFUSED,
            &serde_json::json!({ "item": id, "reason": error.to_string() }),
        ),
    }
}

/// Applies what a task's new state means for the items linked to it, inside the transition's transaction.
///
/// Every in-progress item follows the task-end table ([`on_task_end`]): a merged task, or a closed one whose item never
/// saw the merge, finishes it (a merged task keeps the link); a dropped or failed one blocks it as `task_lost`. An epic is
/// never finished by its explorer's end: merged or closed, it stays in progress and its link ends. The other
/// linked items follow [`on_linked_end`]: back in review, a done item is in progress again; over, a done item loses its
/// link and gets a note naming every merged pull request, and a blocked one loses its link and keeps its block. A move
/// the guard refuses is logged as `backlog-move-refused` and never fails the transition.
pub(crate) fn task_ended(
    tx: &Transaction<'_>,
    task: &Task,
    to: TaskState,
    actor: Actor,
) -> Result<(), StoreError> {
    let in_progress = linked_ids(tx, task.id, ItemStatus::InProgress)?;
    let done = linked_ids(tx, task.id, ItemStatus::Done)?;
    let blocked = linked_ids(tx, task.id, ItemStatus::Blocked)?;
    if in_progress.is_empty() && done.is_empty() && blocked.is_empty() {
        return Ok(());
    }
    let merged: Vec<String> = tx
        .prepare("SELECT url FROM prs WHERE task_id = ?1 AND state = 'merged' ORDER BY number")?
        .query_map(params![task.id.0], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let name = task.name.to_string();
    for id in in_progress {
        let kind = load(tx, id)?.kind;
        let Some(effect) = on_task_end(to, TaskOutcome::Done, kind, &merged, &name) else {
            continue;
        };
        let note = match &effect {
            TaskEndEffect::Finish { note } | TaskEndEffect::Release { note } => Some(note.as_str()),
            TaskEndEffect::Lost { .. } | TaskEndEffect::Partial { .. } => None,
        };
        let request = BacklogMove {
            actor_task: Some(task.id),
            ..BacklogMove::new(effect.target(), BacklogActor::Headwork, actor, true)
                .with_reason(note)
        };
        headwork_move(tx, task, id, &request)?;
    }
    for (status, ids) in [(ItemStatus::Done, done), (ItemStatus::Blocked, blocked)] {
        match on_linked_end(to, status, &merged, &name) {
            Some(LinkedEnd::Unfinish) => {
                let request =
                    BacklogMove::new(Target::InProgress, BacklogActor::Headwork, actor, true);
                for id in ids {
                    headwork_move(tx, task, id, &request)?;
                }
            }
            Some(LinkedEnd::Unlink { note }) => {
                let request = BacklogMove::new(Target::Unlink, BacklogActor::Headwork, actor, true)
                    .with_reason(note.as_deref());
                for id in ids {
                    headwork_move(tx, task, id, &request)?;
                }
            }
            None => {}
        }
    }
    Ok(())
}

/// Adds an item of `shape` inside `tx`, as [`Store::add_backlog_item_shaped`] describes, and logs `backlog-added`.
pub(crate) fn add_shaped(
    tx: &Transaction<'_>,
    draft: &BacklogDraft,
    by: Actor,
    mover: BacklogActor,
    task: Option<TaskId>,
    shape: &ItemShape,
) -> Result<BacklogItem, StoreError> {
    if draft.status != BacklogStatus::Backlog {
        return Err(StoreError::BacklogAddRefused(format!(
            "a new item is added to the backlog, not as {}",
            draft.status.kind()
        )));
    }
    transition(None, &Target::Backlog, mover, &MoveFacts::default())
        .map_err(|refusal| StoreError::BacklogAddRefused(refusal.to_string()))?;
    let kind = shape.kind.unwrap_or(ItemKind::Item);
    let parent = shape
        .parent
        .map(|parent| {
            load(tx, parent).map(|row| ParentFacts {
                id: row.id,
                kind: row.kind,
                parent: row.parent,
            })
        })
        .transpose()?;
    check_parent(kind, parent)
        .map_err(|refusal| StoreError::BacklogAddRefused(refusal.to_string()))?;
    let id = insert(tx, draft, kind, shape.parent, &shape.plan)?;
    for dep in &shape.after {
        add_dependency(tx, id, *dep, by, task)?;
    }
    let item = load(tx, id)?;
    event(tx, by, task, EVENT_BACKLOG_ADDED, &item)?;
    Ok(item)
}

impl Store {
    /// Today's date on this machine's clock, the day an item is added or done.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn backlog_today(&self) -> Result<BacklogDate, StoreError> {
        today(&self.conn)
    }

    /// One item.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownBacklogItem`] when there is none, or a read error.
    pub fn backlog_item(&self, id: i64) -> Result<BacklogItem, StoreError> {
        load(&self.conn, id)
    }

    /// The items linked to a task, in export order, whatever their status.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn backlog_items_of_task(&self, task: TaskId) -> Result<Vec<BacklogItem>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM backlog_items t LEFT JOIN tasks k ON k.id = t.task_id
             WHERE t.task_id = ?1 ORDER BY t.position, t.id"
        ))?;
        let rows = statement.query_map(params![task.0], RawBacklogItem::read)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?.into_backlog_item(&self.conn)?);
        }
        Ok(items)
    }

    /// Records that a task's `done` said it did not finish the work, and blocks each in-progress item linked to it as
    /// `partial` ([`on_task_end`] with [`TaskOutcome::Partial`]); the note is the worker's, cut to one line. An item
    /// already done (its pull request merged first) stays done, and an epic is never blocked by its explorer's outcome.
    /// Logs `task-outcome`; returns the ids blocked.
    ///
    /// # Errors
    ///
    /// Returns a read or write error; a move the guard refuses is logged as `backlog-move-refused` and does not fail.
    pub fn record_partial_outcome(
        &mut self,
        task: &Task,
        session: &str,
        note: Option<&str>,
        actor: Actor,
    ) -> Result<Vec<i64>, StoreError> {
        let tx = self.conn.transaction()?;
        let mut blocked = Vec::new();
        for id in linked_ids(&tx, task.id, ItemStatus::InProgress)? {
            if load(&tx, id)?.kind != ItemKind::Epic {
                blocked.push(id);
            }
        }
        let name = task.name.to_string();
        let effect = on_task_end(
            TaskState::Closed,
            TaskOutcome::Partial,
            ItemKind::Item,
            &[],
            &name,
        );
        if let Some(TaskEndEffect::Partial { note: head }) = effect {
            let said = note
                .map(|note| note.replace(['\n', '\r', '[', ']'], " "))
                .map(|note| note.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|note| !note.is_empty());
            let text = said.map_or_else(|| head.clone(), |said| format!("{head}: {said}"));
            let request = BacklogMove::new(
                TaskEndEffect::Partial { note: text }.target(),
                BacklogActor::Headwork,
                actor,
                true,
            );
            for id in &blocked {
                headwork_move(&tx, task, *id, &request)?;
            }
        }
        insert_event(
            &tx,
            actor,
            Some(task.id),
            Some(session),
            EVENT_TASK_OUTCOME,
            &serde_json::json!({ "outcome": TaskOutcome::Partial.as_str(), "note": note, "items": blocked }),
        )?;
        tx.commit()?;
        Ok(blocked)
    }

    /// The items that match, in export order.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn backlog_items(&self, filter: &BacklogFilter) -> Result<Vec<BacklogItem>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM backlog_items t LEFT JOIN tasks k ON k.id = t.task_id
             WHERE (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.owner = ?2) AND (?3 IS NULL OR t.parent_id = ?3)
             ORDER BY t.position, t.id"
        ))?;
        let rows = statement.query_map(
            params![
                filter.project,
                filter.owner.map(BacklogOwner::as_str),
                filter.epic
            ],
            RawBacklogItem::read,
        )?;
        let mut items = Vec::new();
        for row in rows {
            let item = row?.into_backlog_item(&self.conn)?;
            if filter.statuses.is_empty() || filter.statuses.contains(&item.draft.status.kind()) {
                items.push(item);
            }
        }
        Ok(items)
    }

    /// Adds an item last and logs `backlog-added`; `actor` moves it as [`BacklogActor::from_actor`] says, never as an
    /// explorer.
    ///
    /// # Errors
    ///
    /// As [`Store::add_backlog_item_for`].
    pub fn add_backlog_item(
        &mut self,
        draft: &BacklogDraft,
        actor: Actor,
    ) -> Result<BacklogItem, StoreError> {
        self.add_backlog_item_for(draft, actor, BacklogActor::from_actor(actor, false), None)
    }

    /// Adds an item last, in the backlog, and logs `backlog-added` by `by` for `task`, the exploration that added it.
    ///
    /// The add passes the status guard as `mover` (`transition(None, backlog, ...)`): a worker is refused, an explorer
    /// adds. The draft's status must be `backlog`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::BacklogAddRefused`] when the draft's status is not `backlog` or the guard refuses the add,
    /// [`StoreError::InvalidBacklogItem`] when a field cannot be written to the file, or the SQLite error.
    pub fn add_backlog_item_for(
        &mut self,
        draft: &BacklogDraft,
        by: Actor,
        mover: BacklogActor,
        task: Option<TaskId>,
    ) -> Result<BacklogItem, StoreError> {
        self.add_backlog_item_shaped(draft, by, mover, task, &ItemShape::default())
    }

    /// As [`Store::add_backlog_item_for`], for an item of `shape`: an epic or an item, the epic it belongs to
    /// ([`check_parent`]) and the items it waits for (each through [`check_dependency`], so the item starts blocked for
    /// `depends` while one is not done). Nothing is written when any part is refused.
    ///
    /// # Errors
    ///
    /// As [`Store::add_backlog_item_for`], and [`StoreError::BacklogRefused`] naming a dependency the guards refuse.
    pub fn add_backlog_item_shaped(
        &mut self,
        draft: &BacklogDraft,
        by: Actor,
        mover: BacklogActor,
        task: Option<TaskId>,
        shape: &ItemShape,
    ) -> Result<BacklogItem, StoreError> {
        let tx = self.conn.transaction()?;
        let item = add_shaped(&tx, draft, by, mover, task, shape)?;
        tx.commit()?;
        Ok(item)
    }

    /// The `count` of the newest `backlog-picked` event of `task` and the number of `backlog-added` events it logged
    /// after that event; `(0, 0)` when it has no pick.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn backlog_pick_counts(&self, task: TaskId) -> Result<(u64, u64), StoreError> {
        let picked: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, data FROM events WHERE task_id = ?1 AND type = ?2 ORDER BY id DESC LIMIT 1",
                params![task.0, EVENT_BACKLOG_PICKED],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((id, data)) = picked else {
            return Ok((0, 0));
        };
        let count = serde_json::from_str::<serde_json::Value>(&data)?
            .get("count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let added: i64 = self.conn.query_row(
            "SELECT count(*) FROM events WHERE task_id = ?1 AND type = ?2 AND id > ?3",
            params![task.0, EVENT_BACKLOG_ADDED, id],
            |row| row.get(0),
        )?;
        Ok((count, u64::try_from(added).unwrap_or(0)))
    }

    /// Moves an item's status through the guard ([`transition`]) and logs `backlog-moved` in the same transaction.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::BacklogRefused`] with the guard's reason when the move is not allowed,
    /// [`StoreError::UnknownBacklogItem`] when there is no such item, or a read or write error.
    pub fn move_backlog_item(
        &mut self,
        id: i64,
        request: &BacklogMove<'_>,
    ) -> Result<BacklogItem, StoreError> {
        let tx = self.conn.transaction()?;
        let before = load(&tx, id)?;
        let step = check(&tx, &before, request)?;
        let item = write_move(&tx, &before, &step, request)?;
        tx.commit()?;
        Ok(item)
    }

    /// Makes `item` depend on `dep` and logs `backlog-depended`; an item in the backlog is blocked for `depends` in the same
    /// transaction while `dep` is not done.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::BacklogRefused`] with the dependency guard reason, [`StoreError::UnknownBacklogItem`], or a
    /// write error.
    pub fn add_backlog_dependency(
        &mut self,
        item: i64,
        dep: i64,
        by: Actor,
        task: Option<TaskId>,
    ) -> Result<BacklogItem, StoreError> {
        let tx = self.conn.transaction()?;
        add_dependency(&tx, item, dep, by, task)?;
        let item = load(&tx, item)?;
        tx.commit()?;
        Ok(item)
    }

    /// As [`Store::add_backlog_dependency`] for each of `deps`, in one transaction: when one is refused nothing changes.
    ///
    /// # Errors
    ///
    /// As [`Store::add_backlog_dependency`].
    pub fn add_backlog_dependencies(
        &mut self,
        item: i64,
        deps: &[i64],
        by: Actor,
        task: Option<TaskId>,
    ) -> Result<BacklogItem, StoreError> {
        let tx = self.conn.transaction()?;
        for dep in deps {
            add_dependency(&tx, item, *dep, by, task)?;
        }
        let item = load(&tx, item)?;
        tx.commit()?;
        Ok(item)
    }

    /// Removes the dependency and logs `backlog-undepended`; the dependency block of the item is settled again in the same
    /// transaction: lifted when nothing is left to wait for, else `depends` or `dep_cancelled` for what still waits.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::BacklogRefused`] when `item` does not depend on `dep`, [`StoreError::UnknownBacklogItem`], or
    /// a write error.
    pub fn remove_backlog_dependency(
        &mut self,
        item: i64,
        dep: i64,
        by: Actor,
        task: Option<TaskId>,
    ) -> Result<BacklogItem, StoreError> {
        let tx = self.conn.transaction()?;
        remove_dependency(&tx, item, dep, by, task)?;
        let item = load(&tx, item)?;
        tx.commit()?;
        Ok(item)
    }

    /// Whether the guard would allow a move, without making it.
    ///
    /// # Errors
    ///
    /// As [`Store::move_backlog_item`], and nothing is written.
    pub fn check_backlog_move(&self, id: i64, request: &BacklogMove<'_>) -> Result<(), StoreError> {
        check(&self.conn, &load(&self.conn, id)?, request).map(drop)
    }

    /// Changes an item and logs `backlog-edited`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownBacklogItem`] when there is none, or a write error.
    pub fn edit_backlog_item(
        &mut self,
        id: i64,
        edit: &BacklogEdit,
        actor: Actor,
    ) -> Result<BacklogItem, StoreError> {
        self.edit_backlog_item_for(id, edit, actor, None)
    }

    /// As [`Store::edit_backlog_item`], logged with `task`: the explorer that changed its epic's child.
    ///
    /// # Errors
    ///
    /// As [`Store::edit_backlog_item`].
    pub fn edit_backlog_item_for(
        &mut self,
        id: i64,
        edit: &BacklogEdit,
        actor: Actor,
        task: Option<TaskId>,
    ) -> Result<BacklogItem, StoreError> {
        let tx = self.conn.transaction()?;
        let mut item = load(&tx, id)?.draft;
        if let Some(text) = &edit.text {
            text.clone_into(&mut item.text);
        }
        if let Some(project) = &edit.project {
            project.clone_into(&mut item.project);
        }
        if let Some(section) = &edit.section {
            item.section.clone_from(section);
        }
        if let Some(owner) = edit.owner {
            item.owner = owner;
        }
        if let Some(date) = &edit.not_before {
            item.not_before.clone_from(date);
        }
        if let Some(date) = &edit.due {
            item.due.clone_from(date);
        }
        if let Some(date) = &edit.added {
            item.added = Some(date.clone());
        }
        if let Some(line) = &edit.append {
            item.notes.push(line.clone());
        }
        item.validate().map_err(StoreError::InvalidBacklogItem)?;
        let notes = (!item.notes.is_empty()).then(|| item.notes.join("\n"));
        tx.execute(
            &format!(
                "UPDATE backlog_items SET text = ?2, project = ?3, section = ?4, owner = ?5, not_before = ?6, due = ?7, notes = ?8,
                                  added = ?9, updated_at = {NOW}
                 WHERE id = ?1"
            ),
            params![
                id,
                item.text,
                item.project,
                item.section,
                item.owner.as_str(),
                item.not_before.as_ref().map(BacklogDate::as_str),
                item.due.as_ref().map(BacklogDate::as_str),
                notes,
                item.added.as_ref().map(BacklogDate::as_str),
            ],
        )?;
        let plan = &edit.plan;
        tx.execute(
            &format!(
                "UPDATE backlog_items SET brief = coalesce(?2, brief), task_kind = coalesce(?3, task_kind),
                                          model = coalesce(?4, model), review = coalesce(?5, review), updated_at = {NOW}
                 WHERE id = ?1"
            ),
            params![id, plan.brief, plan.task_kind, plan.model, plan.review],
        )?;
        let item = load(&tx, id)?;
        event(&tx, actor, task, EVENT_BACKLOG_EDITED, &item)?;
        tx.commit()?;
        Ok(item)
    }

    /// The brief, task kind, model and review level stored for an item.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownBacklogItem`] when there is none, or a read error.
    pub fn backlog_plan(&self, id: i64) -> Result<ItemPlan, StoreError> {
        self.conn
            .query_row(
                "SELECT brief, task_kind, model, review FROM backlog_items WHERE id = ?1",
                params![id],
                |row| {
                    Ok(ItemPlan {
                        brief: row.get(0)?,
                        task_kind: row.get(1)?,
                        model: row.get(2)?,
                        review: row.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::UnknownBacklogItem(id))
    }

    /// The open children of an epic (backlog, in progress, blocked), in export order.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn backlog_open_children(&self, epic: i64) -> Result<Vec<i64>, StoreError> {
        open_children(&self.conn, epic)
    }

    /// Records that `task` was spun up to explore epic `epic` (`docs/backlog.md`, section 12) and logs `task-epic`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::BacklogRefused`] when `epic` is not an epic, [`StoreError::UnknownBacklogItem`] when there is
    /// no such item, or a write error.
    pub fn set_task_epic(
        &mut self,
        task: TaskId,
        epic: i64,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        if load(&tx, epic)?.kind != ItemKind::Epic {
            return Err(refused(epic, "it is not an epic".to_owned()));
        }
        tx.execute(
            &format!("UPDATE tasks SET epic_id = ?2, updated_at = {NOW} WHERE id = ?1"),
            params![task.0, epic],
        )?;
        insert_event(
            &tx,
            actor,
            Some(task),
            None,
            EVENT_TASK_EPIC,
            &serde_json::json!({ "epic": epic }),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// What importing `drafts` would do: new items, items the store already holds by text, and repeats within the list.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn backlog_import_plan(&self, drafts: &[BacklogDraft]) -> Result<ImportCounts, StoreError> {
        Ok(counts(&self.conn, drafts)?.0)
    }

    /// Adds the items whose text the store lacks, in order, after the ones it has; never changes or deletes a row.
    /// Records `path` as imported and logs `backlog-imported`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error; nothing is added then.
    pub fn import_backlog_items(
        &mut self,
        path: &str,
        drafts: &[BacklogDraft],
        actor: Actor,
    ) -> Result<ImportCounts, StoreError> {
        let tx = self.conn.transaction()?;
        let (counts, fresh) = counts(&tx, drafts)?;
        for (draft, _) in drafts.iter().zip(&fresh).filter(|(_, fresh)| **fresh) {
            insert(&tx, draft, ItemKind::Item, None, &ItemPlan::default())?;
        }
        tx.execute(
            &format!(
                "INSERT INTO backlog_files (path, imported_at) VALUES (?1, {NOW})
                 ON CONFLICT (path) DO UPDATE SET imported_at = excluded.imported_at"
            ),
            params![path],
        )?;
        let data = serde_json::json!({ "path": path, "counts": counts });
        insert_event(&tx, actor, None, None, EVENT_BACKLOG_IMPORTED, &data)?;
        tx.commit()?;
        Ok(counts)
    }

    /// The record of a `backlog.md` path, when it has one.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn backlog_file(&self, path: &str) -> Result<Option<BacklogFile>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT imported_at, export_hash, exported_at, configured_at FROM backlog_files WHERE path = ?1",
                params![path],
                |row| {
                    Ok(BacklogFile {
                        path: path.to_owned(),
                        imported_at: row.get(0)?,
                        export_hash: row.get(1)?,
                        exported_at: row.get(2)?,
                        configured_at: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Every recorded `backlog.md` path, the one used last (exported, else imported) first.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn backlog_files(&self) -> Result<Vec<BacklogFile>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT path, imported_at, export_hash, exported_at, configured_at FROM backlog_files
             ORDER BY coalesce(exported_at, imported_at) DESC, path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(BacklogFile {
                path: row.get(0)?,
                imported_at: row.get(1)?,
                export_hash: row.get(2)?,
                exported_at: row.get(3)?,
                configured_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Records that `path` now holds the pointer line whose hash is `hash`, so pointing `[backlog] file` back at it later
    /// overwrites the pointer without taking it for a hand edit.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn record_backlog_pointer(&self, path: &str, hash: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE backlog_files SET export_hash = ?2 WHERE path = ?1",
            params![path, hash],
        )?;
        Ok(())
    }

    /// Records an export of `hash` to `path` and logs `backlog-exported`; `configured` marks it an export of the
    /// configured `[backlog] file` (`configured_at`).
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn record_backlog_export(
        &mut self,
        path: &str,
        hash: &str,
        configured: bool,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO backlog_files (path, export_hash, exported_at, configured_at)
                 VALUES (?1, ?2, {NOW}, CASE WHEN ?3 THEN {NOW} END)
                 ON CONFLICT (path) DO UPDATE SET export_hash = excluded.export_hash, exported_at = excluded.exported_at,
                 configured_at = coalesce(excluded.configured_at, backlog_files.configured_at)"
            ),
            params![path, hash, configured],
        )?;
        let data = serde_json::json!({ "path": path, "hash": hash });
        insert_event(&tx, actor, None, None, EVENT_BACKLOG_EXPORTED, &data)?;
        tx.commit()?;
        Ok(())
    }
}
