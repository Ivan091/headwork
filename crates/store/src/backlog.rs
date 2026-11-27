//! The backlog list (`docs/backlog.md`): items, their link to the task spawned for them, and the record of the file they are
//! imported from and exported to.
//!
//! `backlog_items.position` orders the export: an import keeps file order, a new item goes last. `backlog_items.notes` holds the
//! sub-bullets one per line. `backlog_files` remembers, per path, when the file was imported and the hash and time of the
//! last export, so an export can tell that the file was edited by hand since.

use std::collections::HashSet;

use headwork_core::backlog::{
    BacklogDate, BacklogDraft, BacklogEffect, BacklogItem, BacklogOwner, BacklogState, LinkedTask,
    identity, on_task_end,
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

/// The event of an item done, by hand or by its task closing.
pub const EVENT_BACKLOG_DONE: &str = "backlog-done";

/// The event of an item dropped.
pub const EVENT_BACKLOG_DROPPED: &str = "backlog-dropped";

/// The event of an item changed, reopened, linked or unlinked.
pub const EVENT_BACKLOG_EDITED: &str = "backlog-edited";

/// The event of an import.
pub const EVENT_BACKLOG_IMPORTED: &str = "backlog-imported";

/// The event of an export that wrote the file.
pub const EVENT_BACKLOG_EXPORTED: &str = "backlog-exported";

const TODAY: &str = "date('now', 'localtime')";

const COLUMNS: &str = "t.id, t.project, t.section, t.text, t.notes, t.state, t.owner, t.added, t.not_before, t.due, \
                       t.done_on, t.done_note, t.task_id, k.project, k.slug, k.state, t.position";

/// Which items to list; an absent field matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BacklogFilter {
    /// Only this project.
    pub project: Option<String>,
    /// Only this owner.
    pub owner: Option<BacklogOwner>,
    /// Only this state.
    pub state: Option<BacklogState>,
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
    state: String,
    owner: String,
    dates: [Option<String>; 4],
    done_note: Option<String>,
    task: Option<(i64, String, String, String)>,
    position: i64,
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
            state: row.get(5)?,
            owner: row.get(6)?,
            dates: [row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?],
            done_note: row.get(11)?,
            task,
            position: row.get(16)?,
        })
    }

    fn into_backlog_item(self) -> Result<BacklogItem, StoreError> {
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
            draft: BacklogDraft {
                project: self.project,
                section: self.section,
                text: self.text,
                notes: self
                    .notes
                    .map(|notes| notes.lines().map(str::to_owned).collect())
                    .unwrap_or_default(),
                state: self.state.parse()?,
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

fn load(conn: &Connection, id: i64) -> Result<BacklogItem, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM backlog_items t LEFT JOIN tasks k ON k.id = t.task_id WHERE t.id = ?1"
        ),
        params![id],
        RawBacklogItem::read,
    )
    .optional()?
    .ok_or(StoreError::UnknownBacklogItem(id))?
    .into_backlog_item()
}

fn today(conn: &Connection) -> Result<BacklogDate, StoreError> {
    let text: String = conn.query_row(&format!("SELECT {TODAY}"), [], |row| row.get(0))?;
    Ok(BacklogDate::parse(&text)?)
}

fn insert(tx: &Transaction<'_>, draft: &BacklogDraft) -> Result<i64, StoreError> {
    draft.validate().map_err(StoreError::InvalidBacklogItem)?;
    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM backlog_items",
        [],
        |row| row.get(0),
    )?;
    let notes = (!draft.notes.is_empty()).then(|| draft.notes.join("\n"));
    tx.execute(
        &format!(
            "INSERT INTO backlog_items (project, section, text, notes, state, owner, added, not_before, due, done_on, done_note,
                                position, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, {NOW}, {NOW})"
        ),
        params![
            draft.project,
            draft.section,
            draft.text,
            notes,
            draft.state.as_str(),
            draft.owner.as_str(),
            draft.added.as_ref().map(BacklogDate::as_str),
            draft.not_before.as_ref().map(BacklogDate::as_str),
            draft.due.as_ref().map(BacklogDate::as_str),
            draft.done_on.as_ref().map(BacklogDate::as_str),
            draft.done_note,
            position,
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
        "state": item.draft.state,
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

/// Applies what a task's end means for the item linked to it, inside the transition's transaction: a closed task
/// finishes its item, a dropped or failed one frees it.
pub(crate) fn task_ended(
    tx: &Transaction<'_>,
    task: &Task,
    to: TaskState,
    actor: Actor,
) -> Result<(), StoreError> {
    let Some(id) = tx
        .query_row(
            "SELECT id FROM backlog_items WHERE task_id = ?1 AND state = 'open'",
            params![task.id.0],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(());
    };
    let merged: Vec<String> = if to == TaskState::Closed {
        tx.prepare("SELECT url FROM prs WHERE task_id = ?1 AND state = 'merged' ORDER BY number")?
            .query_map(params![task.id.0], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    } else {
        Vec::new()
    };
    match on_task_end(to, &merged, &task.name.to_string()) {
        Some(BacklogEffect::Finish { note }) => {
            tx.execute(
                &format!(
                    "UPDATE backlog_items SET state = 'done', done_on = {TODAY}, done_note = ?2, task_id = NULL, updated_at = {NOW}
                     WHERE id = ?1"
                ),
                params![id, note],
            )?;
            event(tx, actor, Some(task.id), EVENT_BACKLOG_DONE, &load(tx, id)?)
        }
        Some(BacklogEffect::Unlink) => {
            tx.execute(
                &format!(
                    "UPDATE backlog_items SET task_id = NULL, updated_at = {NOW} WHERE id = ?1"
                ),
                params![id],
            )?;
            event(
                tx,
                actor,
                Some(task.id),
                EVENT_BACKLOG_EDITED,
                &load(tx, id)?,
            )
        }
        None => Ok(()),
    }
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

    /// The items that match, in export order.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn backlog_items(&self, filter: &BacklogFilter) -> Result<Vec<BacklogItem>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM backlog_items t LEFT JOIN tasks k ON k.id = t.task_id
             WHERE (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.owner = ?2) AND (?3 IS NULL OR t.state = ?3)
             ORDER BY t.position, t.id"
        ))?;
        let rows = statement.query_map(
            params![
                filter.project,
                filter.owner.map(BacklogOwner::as_str),
                filter.state.map(BacklogState::as_str)
            ],
            RawBacklogItem::read,
        )?;
        rows.map(|row| row?.into_backlog_item()).collect()
    }

    /// Adds an item last and logs `backlog-added`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn add_backlog_item(
        &mut self,
        draft: &BacklogDraft,
        actor: Actor,
    ) -> Result<BacklogItem, StoreError> {
        self.add_backlog_item_for(draft, actor, None)
    }

    /// Adds an item last and logs `backlog-added` for `task`, the exploration that added it.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn add_backlog_item_for(
        &mut self,
        draft: &BacklogDraft,
        actor: Actor,
        task: Option<TaskId>,
    ) -> Result<BacklogItem, StoreError> {
        let tx = self.conn.transaction()?;
        let id = insert(&tx, draft)?;
        let item = load(&tx, id)?;
        event(&tx, actor, task, EVENT_BACKLOG_ADDED, &item)?;
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

    /// Marks an open item done, or dropped with `drop`, today, with a note, and logs `backlog-done` or `backlog-dropped`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::BacklogRefused`] when the item is not open, or a read or write error.
    pub fn finish_backlog_item(
        &mut self,
        id: i64,
        note: Option<&str>,
        drop: bool,
        actor: Actor,
    ) -> Result<BacklogItem, StoreError> {
        let tx = self.conn.transaction()?;
        let before = load(&tx, id)?;
        if before.draft.state != BacklogState::Open {
            return Err(StoreError::BacklogRefused {
                id,
                reason: format!("it is already {}", before.draft.state),
            });
        }
        let state = if drop {
            BacklogState::Dropped
        } else {
            BacklogState::Done
        };
        tx.execute(
            &format!(
                "UPDATE backlog_items SET state = ?2, done_on = {TODAY}, done_note = ?3, task_id = NULL, updated_at = {NOW}
                 WHERE id = ?1"
            ),
            params![id, state.as_str(), note],
        )?;
        let item = load(&tx, id)?;
        let kind = if drop {
            EVENT_BACKLOG_DROPPED
        } else {
            EVENT_BACKLOG_DONE
        };
        event(
            &tx,
            actor,
            before.task.as_ref().map(|task| TaskId(task.id)),
            kind,
            &item,
        )?;
        tx.commit()?;
        Ok(item)
    }

    /// Takes a done or dropped item back to open and logs `backlog-edited`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::BacklogRefused`] when the item is already open, or a read or write error.
    pub fn reopen_backlog_item(
        &mut self,
        id: i64,
        actor: Actor,
    ) -> Result<BacklogItem, StoreError> {
        let tx = self.conn.transaction()?;
        let before = load(&tx, id)?;
        if before.draft.state == BacklogState::Open {
            return Err(StoreError::BacklogRefused {
                id,
                reason: "it is already open".to_owned(),
            });
        }
        tx.execute(
            &format!(
                "UPDATE backlog_items SET state = 'open', done_on = NULL, done_note = NULL, updated_at = {NOW} WHERE id = ?1"
            ),
            params![id],
        )?;
        let item = load(&tx, id)?;
        event(&tx, actor, None, EVENT_BACKLOG_EDITED, &item)?;
        tx.commit()?;
        Ok(item)
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
        let item = load(&tx, id)?;
        event(&tx, actor, None, EVENT_BACKLOG_EDITED, &item)?;
        tx.commit()?;
        Ok(item)
    }

    /// Whether an item may be given to a new task: it is open and no live task holds it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::BacklogRefused`] when it may not, or a read error.
    pub fn check_backlog_linkable(&self, id: i64) -> Result<(), StoreError> {
        let item = load(&self.conn, id)?;
        if item.draft.state != BacklogState::Open {
            return Err(StoreError::BacklogRefused {
                id,
                reason: format!("it is {}, not open", item.draft.state),
            });
        }
        if let Some(task) = item.task {
            let live: bool = self.conn.query_row(
                "SELECT state NOT IN ('closed', 'dropped', 'failed') FROM tasks WHERE id = ?1",
                params![task.id],
                |row| row.get(0),
            )?;
            if live {
                return Err(StoreError::BacklogRefused {
                    id,
                    reason: format!("task {} already works on it", task.name),
                });
            }
        }
        Ok(())
    }

    /// Gives an item to a task and logs `backlog-edited`.
    ///
    /// # Errors
    ///
    /// As [`Store::check_backlog_linkable`], or a write error.
    pub fn link_backlog_item(
        &mut self,
        id: i64,
        task: TaskId,
        actor: Actor,
    ) -> Result<BacklogItem, StoreError> {
        self.check_backlog_linkable(id)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!("UPDATE backlog_items SET task_id = ?2, updated_at = {NOW} WHERE id = ?1"),
            params![id, task.0],
        )?;
        let item = load(&tx, id)?;
        event(&tx, actor, Some(task), EVENT_BACKLOG_EDITED, &item)?;
        tx.commit()?;
        Ok(item)
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
            insert(&tx, draft)?;
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
