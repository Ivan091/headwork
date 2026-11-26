//! The to-do list (`docs/todo.md`): items, their link to the task spawned for them, and the record of the file they are
//! imported from and exported to.
//!
//! `todos.position` orders the export: an import keeps file order, a new item goes last. `todos.notes` holds the
//! sub-bullets one per line. `todo_files` remembers, per path, when the file was imported and the hash and time of the
//! last export, so an export can tell that the file was edited by hand since.

use std::collections::HashSet;

use headwork_core::todo::{
    LinkedTask, Todo, TodoDate, TodoDraft, TodoEffect, TodoOwner, TodoState, identity, on_task_end,
};
use headwork_core::{Actor, TaskState};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::Serialize;

use crate::queries::insert_event;
use crate::{NOW, Store, StoreError, Task, TaskId};

/// The event of a new item.
pub const EVENT_TODO_ADDED: &str = "todo-added";

/// The event of an item done, by hand or by its task closing.
pub const EVENT_TODO_DONE: &str = "todo-done";

/// The event of an item dropped.
pub const EVENT_TODO_DROPPED: &str = "todo-dropped";

/// The event of an item changed, reopened, linked or unlinked.
pub const EVENT_TODO_EDITED: &str = "todo-edited";

/// The event of an import.
pub const EVENT_TODO_IMPORTED: &str = "todo-imported";

/// The event of an export that wrote the file.
pub const EVENT_TODO_EXPORTED: &str = "todo-exported";

const TODAY: &str = "date('now', 'localtime')";

const COLUMNS: &str = "t.id, t.project, t.section, t.text, t.notes, t.state, t.owner, t.added, t.not_before, t.due, \
                       t.done_on, t.done_note, t.task_id, k.project, k.slug, k.state, t.position";

/// Which items to list; an absent field matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoFilter {
    /// Only this project.
    pub project: Option<String>,
    /// Only this owner.
    pub owner: Option<TodoOwner>,
    /// Only this state.
    pub state: Option<TodoState>,
}

/// A change to an item; an absent field stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoEdit {
    /// The new text.
    pub text: Option<String>,
    /// The new project.
    pub project: Option<String>,
    /// The new section; `Some(None)` takes it out of its section.
    pub section: Option<Option<String>>,
    /// The new owner.
    pub owner: Option<TodoOwner>,
    /// The new first day; `Some(None)` clears it.
    pub not_before: Option<Option<TodoDate>>,
    /// The new due day; `Some(None)` clears it.
    pub due: Option<Option<TodoDate>>,
    /// A note line to add.
    pub append: Option<String>,
    /// The day it was added, when the new text says so in an `(added D)` group.
    pub added: Option<TodoDate>,
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

/// The record of one `todo.md` path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TodoFile {
    /// The path, with forward slashes.
    pub path: String,
    /// When it was imported, UTC.
    pub imported_at: Option<String>,
    /// The hash of the last export's content.
    pub export_hash: Option<String>,
    /// When it was last exported, UTC.
    pub exported_at: Option<String>,
}

struct RawTodo {
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

impl RawTodo {
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

    fn into_todo(self) -> Result<Todo, StoreError> {
        let [added, not_before, due, done_on] = self.dates;
        let date = |text: Option<String>| text.map(|text| TodoDate::parse(&text)).transpose();
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
        Ok(Todo {
            id: self.id,
            position: self.position,
            task,
            draft: TodoDraft {
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

fn load(conn: &Connection, id: i64) -> Result<Todo, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM todos t LEFT JOIN tasks k ON k.id = t.task_id WHERE t.id = ?1"
        ),
        params![id],
        RawTodo::read,
    )
    .optional()?
    .ok_or(StoreError::UnknownTodo(id))?
    .into_todo()
}

fn today(conn: &Connection) -> Result<TodoDate, StoreError> {
    let text: String = conn.query_row(&format!("SELECT {TODAY}"), [], |row| row.get(0))?;
    Ok(TodoDate::parse(&text)?)
}

fn insert(tx: &Transaction<'_>, draft: &TodoDraft) -> Result<i64, StoreError> {
    draft.validate().map_err(StoreError::InvalidTodo)?;
    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM todos",
        [],
        |row| row.get(0),
    )?;
    let notes = (!draft.notes.is_empty()).then(|| draft.notes.join("\n"));
    tx.execute(
        &format!(
            "INSERT INTO todos (project, section, text, notes, state, owner, added, not_before, due, done_on, done_note,
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
            draft.added.as_ref().map(TodoDate::as_str),
            draft.not_before.as_ref().map(TodoDate::as_str),
            draft.due.as_ref().map(TodoDate::as_str),
            draft.done_on.as_ref().map(TodoDate::as_str),
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
    todo: &Todo,
) -> Result<(), StoreError> {
    let data = serde_json::json!({
        "id": todo.id,
        "project": todo.draft.project,
        "text": todo.draft.text,
        "state": todo.draft.state,
    });
    insert_event(tx, actor, task, None, kind, &data)
}

fn counts(
    conn: &Connection,
    drafts: &[TodoDraft],
) -> Result<(ImportCounts, Vec<bool>), StoreError> {
    let mut known: HashSet<String> = HashSet::new();
    let mut statement = conn.prepare("SELECT text FROM todos")?;
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
            "SELECT id FROM todos WHERE task_id = ?1 AND state = 'open'",
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
        Some(TodoEffect::Finish { note }) => {
            tx.execute(
                &format!(
                    "UPDATE todos SET state = 'done', done_on = {TODAY}, done_note = ?2, task_id = NULL, updated_at = {NOW}
                     WHERE id = ?1"
                ),
                params![id, note],
            )?;
            event(tx, actor, Some(task.id), EVENT_TODO_DONE, &load(tx, id)?)
        }
        Some(TodoEffect::Unlink) => {
            tx.execute(
                &format!("UPDATE todos SET task_id = NULL, updated_at = {NOW} WHERE id = ?1"),
                params![id],
            )?;
            event(tx, actor, Some(task.id), EVENT_TODO_EDITED, &load(tx, id)?)
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
    pub fn todo_today(&self) -> Result<TodoDate, StoreError> {
        today(&self.conn)
    }

    /// One item.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownTodo`] when there is none, or a read error.
    pub fn todo(&self, id: i64) -> Result<Todo, StoreError> {
        load(&self.conn, id)
    }

    /// The items that match, in export order.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn todos(&self, filter: &TodoFilter) -> Result<Vec<Todo>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM todos t LEFT JOIN tasks k ON k.id = t.task_id
             WHERE (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.owner = ?2) AND (?3 IS NULL OR t.state = ?3)
             ORDER BY t.position, t.id"
        ))?;
        let rows = statement.query_map(
            params![
                filter.project,
                filter.owner.map(TodoOwner::as_str),
                filter.state.map(TodoState::as_str)
            ],
            RawTodo::read,
        )?;
        rows.map(|row| row?.into_todo()).collect()
    }

    /// Adds an item last and logs `todo-added`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn add_todo(&mut self, draft: &TodoDraft, actor: Actor) -> Result<Todo, StoreError> {
        let tx = self.conn.transaction()?;
        let id = insert(&tx, draft)?;
        let todo = load(&tx, id)?;
        event(&tx, actor, None, EVENT_TODO_ADDED, &todo)?;
        tx.commit()?;
        Ok(todo)
    }

    /// Marks an open item done, or dropped with `drop`, today, with a note, and logs `todo-done` or `todo-dropped`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::TodoRefused`] when the item is not open, or a read or write error.
    pub fn finish_todo(
        &mut self,
        id: i64,
        note: Option<&str>,
        drop: bool,
        actor: Actor,
    ) -> Result<Todo, StoreError> {
        let tx = self.conn.transaction()?;
        let before = load(&tx, id)?;
        if before.draft.state != TodoState::Open {
            return Err(StoreError::TodoRefused {
                id,
                reason: format!("it is already {}", before.draft.state),
            });
        }
        let state = if drop {
            TodoState::Dropped
        } else {
            TodoState::Done
        };
        tx.execute(
            &format!(
                "UPDATE todos SET state = ?2, done_on = {TODAY}, done_note = ?3, task_id = NULL, updated_at = {NOW}
                 WHERE id = ?1"
            ),
            params![id, state.as_str(), note],
        )?;
        let todo = load(&tx, id)?;
        let kind = if drop {
            EVENT_TODO_DROPPED
        } else {
            EVENT_TODO_DONE
        };
        event(
            &tx,
            actor,
            before.task.as_ref().map(|task| TaskId(task.id)),
            kind,
            &todo,
        )?;
        tx.commit()?;
        Ok(todo)
    }

    /// Takes a done or dropped item back to open and logs `todo-edited`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::TodoRefused`] when the item is already open, or a read or write error.
    pub fn reopen_todo(&mut self, id: i64, actor: Actor) -> Result<Todo, StoreError> {
        let tx = self.conn.transaction()?;
        let before = load(&tx, id)?;
        if before.draft.state == TodoState::Open {
            return Err(StoreError::TodoRefused {
                id,
                reason: "it is already open".to_owned(),
            });
        }
        tx.execute(
            &format!(
                "UPDATE todos SET state = 'open', done_on = NULL, done_note = NULL, updated_at = {NOW} WHERE id = ?1"
            ),
            params![id],
        )?;
        let todo = load(&tx, id)?;
        event(&tx, actor, None, EVENT_TODO_EDITED, &todo)?;
        tx.commit()?;
        Ok(todo)
    }

    /// Changes an item and logs `todo-edited`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::UnknownTodo`] when there is none, or a write error.
    pub fn edit_todo(
        &mut self,
        id: i64,
        edit: &TodoEdit,
        actor: Actor,
    ) -> Result<Todo, StoreError> {
        let tx = self.conn.transaction()?;
        let mut todo = load(&tx, id)?.draft;
        if let Some(text) = &edit.text {
            text.clone_into(&mut todo.text);
        }
        if let Some(project) = &edit.project {
            project.clone_into(&mut todo.project);
        }
        if let Some(section) = &edit.section {
            todo.section.clone_from(section);
        }
        if let Some(owner) = edit.owner {
            todo.owner = owner;
        }
        if let Some(date) = &edit.not_before {
            todo.not_before.clone_from(date);
        }
        if let Some(date) = &edit.due {
            todo.due.clone_from(date);
        }
        if let Some(date) = &edit.added {
            todo.added = Some(date.clone());
        }
        if let Some(line) = &edit.append {
            todo.notes.push(line.clone());
        }
        todo.validate().map_err(StoreError::InvalidTodo)?;
        let notes = (!todo.notes.is_empty()).then(|| todo.notes.join("\n"));
        tx.execute(
            &format!(
                "UPDATE todos SET text = ?2, project = ?3, section = ?4, owner = ?5, not_before = ?6, due = ?7, notes = ?8,
                                  added = ?9, updated_at = {NOW}
                 WHERE id = ?1"
            ),
            params![
                id,
                todo.text,
                todo.project,
                todo.section,
                todo.owner.as_str(),
                todo.not_before.as_ref().map(TodoDate::as_str),
                todo.due.as_ref().map(TodoDate::as_str),
                notes,
                todo.added.as_ref().map(TodoDate::as_str),
            ],
        )?;
        let todo = load(&tx, id)?;
        event(&tx, actor, None, EVENT_TODO_EDITED, &todo)?;
        tx.commit()?;
        Ok(todo)
    }

    /// Whether an item may be given to a new task: it is open and no live task holds it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::TodoRefused`] when it may not, or a read error.
    pub fn check_todo_linkable(&self, id: i64) -> Result<(), StoreError> {
        let todo = load(&self.conn, id)?;
        if todo.draft.state != TodoState::Open {
            return Err(StoreError::TodoRefused {
                id,
                reason: format!("it is {}, not open", todo.draft.state),
            });
        }
        if let Some(task) = todo.task {
            let live: bool = self.conn.query_row(
                "SELECT state NOT IN ('closed', 'dropped', 'failed') FROM tasks WHERE id = ?1",
                params![task.id],
                |row| row.get(0),
            )?;
            if live {
                return Err(StoreError::TodoRefused {
                    id,
                    reason: format!("task {} already works on it", task.name),
                });
            }
        }
        Ok(())
    }

    /// Gives an item to a task and logs `todo-edited`.
    ///
    /// # Errors
    ///
    /// As [`Store::check_todo_linkable`], or a write error.
    pub fn link_todo(&mut self, id: i64, task: TaskId, actor: Actor) -> Result<Todo, StoreError> {
        self.check_todo_linkable(id)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!("UPDATE todos SET task_id = ?2, updated_at = {NOW} WHERE id = ?1"),
            params![id, task.0],
        )?;
        let todo = load(&tx, id)?;
        event(&tx, actor, Some(task), EVENT_TODO_EDITED, &todo)?;
        tx.commit()?;
        Ok(todo)
    }

    /// What importing `drafts` would do: new items, items the store already holds by text, and repeats within the list.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn todo_import_plan(&self, drafts: &[TodoDraft]) -> Result<ImportCounts, StoreError> {
        Ok(counts(&self.conn, drafts)?.0)
    }

    /// Adds the items whose text the store lacks, in order, after the ones it has; never changes or deletes a row.
    /// Records `path` as imported and logs `todo-imported`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error; nothing is added then.
    pub fn import_todos(
        &mut self,
        path: &str,
        drafts: &[TodoDraft],
        actor: Actor,
    ) -> Result<ImportCounts, StoreError> {
        let tx = self.conn.transaction()?;
        let (counts, fresh) = counts(&tx, drafts)?;
        for (draft, _) in drafts.iter().zip(&fresh).filter(|(_, fresh)| **fresh) {
            insert(&tx, draft)?;
        }
        tx.execute(
            &format!(
                "INSERT INTO todo_files (path, imported_at) VALUES (?1, {NOW})
                 ON CONFLICT (path) DO UPDATE SET imported_at = excluded.imported_at"
            ),
            params![path],
        )?;
        let data = serde_json::json!({ "path": path, "counts": counts });
        insert_event(&tx, actor, None, None, EVENT_TODO_IMPORTED, &data)?;
        tx.commit()?;
        Ok(counts)
    }

    /// The record of a `todo.md` path, when it has one.
    ///
    /// # Errors
    ///
    /// Returns a read error.
    pub fn todo_file(&self, path: &str) -> Result<Option<TodoFile>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT imported_at, export_hash, exported_at FROM todo_files WHERE path = ?1",
                params![path],
                |row| {
                    Ok(TodoFile {
                        path: path.to_owned(),
                        imported_at: row.get(0)?,
                        export_hash: row.get(1)?,
                        exported_at: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Records an export of `hash` to `path` and logs `todo-exported`.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error.
    pub fn record_todo_export(
        &mut self,
        path: &str,
        hash: &str,
        actor: Actor,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO todo_files (path, export_hash, exported_at) VALUES (?1, ?2, {NOW})
                 ON CONFLICT (path) DO UPDATE SET export_hash = excluded.export_hash, exported_at = excluded.exported_at"
            ),
            params![path, hash],
        )?;
        let data = serde_json::json!({ "path": path, "hash": hash });
        insert_event(&tx, actor, None, None, EVENT_TODO_EXPORTED, &data)?;
        tx.commit()?;
        Ok(())
    }
}
