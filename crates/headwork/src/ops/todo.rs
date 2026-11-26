//! The to-do list in the store and its `todo.md` (`docs/todo.md`): the changes the CLI and the orchestrator's tools make,
//! the import of the file, and the export that writes it back.
//!
//! The store is the source; the file is a copy git carries. An export never writes a file that was not imported first
//! and keeps a copy of a file edited by hand since the last export.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use headwork_core::todo::{self, Todo, TodoDate, TodoDraft, TodoOwner, TodoState, Unparsed};
use headwork_core::{Actor, Config};
use headwork_store::{ImportCounts, Store, TaskId, TodoEdit, TodoFilter};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::spawn::{Pumped, SpawnRequest, spawn};
use super::{Env, slashed};
use crate::home::Home;

/// The event an export logs when it replaced a file edited by hand since the last export; it names the copy.
pub const EVENT_EXPORT_OVERWROTE: &str = "todo-export-overwrote";

/// The folder in the home folder that holds the copies of overwritten files.
pub const BACKUP_DIR: &str = "todo-backups";

/// How many copies of overwritten files are kept.
pub const BACKUPS_KEPT: usize = 10;

/// What an export did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "export", rename_all = "snake_case")]
pub enum Export {
    /// No file is set: neither `[todo] file` nor `--file`.
    Unconfigured,
    /// The content equals the last export; nothing was written.
    Unchanged,
    /// The file was written.
    Written {
        /// The file.
        path: String,
        /// The copy of the file as it was, when someone had edited it since the last export.
        backup: Option<String>,
    },
    /// The file exists and was never imported, so nothing was written.
    NotImported {
        /// The file.
        path: String,
    },
    /// The export failed; the change it followed stands.
    Failed {
        /// Why.
        error: String,
    },
}

impl Export {
    /// One line for the command's output.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Unconfigured => "no to-do file is set ([todo] file), nothing exported".to_owned(),
            Self::Unchanged => "the to-do file is up to date".to_owned(),
            Self::Written { path, backup: None } => format!("exported to {path}"),
            Self::Written {
                path,
                backup: Some(backup),
            } => format!(
                "exported to {path}; the file differed from the last export (edited by hand, or never exported), a copy is in {backup}"
            ),
            Self::NotImported { path } => {
                format!(
                    "{path} exists and was never imported: run `headwork todo import` first; nothing exported"
                )
            }
            Self::Failed { error } => format!("export failed: {error}"),
        }
    }
}

/// What an import found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// The file read.
    pub path: String,
    /// Whether nothing was written.
    pub dry_run: bool,
    /// New, already known and repeated items.
    pub counts: ImportCounts,
    /// The lines left out, with the reason.
    pub unparsed: Vec<Unparsed>,
}

impl ImportReport {
    /// The report as text.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = format!(
            "{} {}: {} added, {} already in the store, {} duplicate in the file, {} lines not parsed\n",
            if self.dry_run {
                "would import"
            } else {
                "imported"
            },
            self.path,
            self.counts.added,
            self.counts.already,
            self.counts.duplicate,
            self.unparsed.len()
        );
        for line in &self.unparsed {
            let _infallible = writeln!(
                out,
                "  line {}: {} ({})",
                line.line_no, line.line, line.reason
            );
        }
        out
    }
}

/// An item to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddRequest {
    /// The area heading.
    pub project: String,
    /// The item text.
    pub text: String,
    /// A `###` subheading.
    pub section: Option<String>,
    /// Who acts; the user when the text starts with `For you`, an agent otherwise, when absent.
    pub owner: Option<TodoOwner>,
    /// The first day it may start.
    pub not_before: Option<TodoDate>,
    /// The day it is due.
    pub due: Option<TodoDate>,
    /// Sub-bullets.
    pub notes: Vec<String>,
}

/// Items a change touched and what the export after it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// The items, one line each.
    pub items: Vec<String>,
    /// The export that followed.
    pub export: Export,
}

impl Change {
    /// The change as text: the item lines, then the export outcome.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        for line in &self.items {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&self.export.summary());
        out.push('\n');
        out
    }
}

fn hash(text: &str) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(text.replace("\r\n", "\n").as_bytes()) {
        let _infallible = write!(hex, "{byte:02x}");
    }
    hex
}

fn millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

/// The file the to-do list is exported to and imported from: `file` when given, else `[todo] file`.
#[must_use]
pub fn target(home: &Home, config: &Config, file: Option<&Path>) -> Option<PathBuf> {
    file.map(Path::to_path_buf)
        .or_else(|| config.todo.file.as_deref().map(|path| home.expand(path)))
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let name = path.file_name().map_or_else(
        || "todo.md".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temp = path.with_file_name(format!(".{name}.headwork-new"));
    std::fs::write(&temp, text).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, path)
        .with_context(|| format!("replacing {} with {}", path.display(), temp.display()))
}

fn back_up(home: &Home, path: &Path) -> Result<PathBuf> {
    let dir = home.root().join(BACKUP_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let copy = dir.join(format!("todo-{:015}.md", millis()));
    std::fs::copy(path, &copy)
        .with_context(|| format!("copying {} to {}", path.display(), copy.display()))?;
    let mut copies: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "md")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("todo-"))
        })
        .collect();
    copies.sort();
    let surplus = copies.len().saturating_sub(BACKUPS_KEPT);
    for old in copies.into_iter().take(surplus) {
        std::fs::remove_file(old).ok();
    }
    Ok(copy)
}

/// Writes the to-do list to its file when the content changed since the last export.
///
/// A file that exists but was never imported is left alone. A file whose content is not what the last export wrote was
/// edited by hand: it is copied to `<home>/todo-backups` first and `todo-export-overwrote` is logged.
///
/// # Errors
///
/// Fails when the store, the file or the copy fails.
pub fn export(
    store: &mut Store,
    home: &Home,
    config: &Config,
    file: Option<&Path>,
    actor: Actor,
) -> Result<Export> {
    let Some(path) = target(home, config, file) else {
        return Ok(Export::Unconfigured);
    };
    let key = slashed(&path);
    let text = todo::render(&store.todos(&TodoFilter::default())?);
    let new_hash = hash(&text);
    let record = store.todo_file(&key)?;
    let exists = path.exists();
    if exists
        && record
            .as_ref()
            .and_then(|record| record.export_hash.as_deref())
            == Some(new_hash.as_str())
    {
        return Ok(Export::Unchanged);
    }
    if exists
        && record
            .as_ref()
            .is_none_or(|record| record.imported_at.is_none())
    {
        return Ok(Export::NotImported { path: key });
    }
    let mut backup = None;
    if exists {
        let on_disk = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let last = record.and_then(|record| record.export_hash);
        if last.as_deref() != Some(hash(&on_disk).as_str()) {
            let copy = back_up(home, &path)?;
            store.append_event(
                actor,
                None,
                None,
                EVENT_EXPORT_OVERWROTE,
                &serde_json::json!({ "path": key, "backup": slashed(&copy) }),
            )?;
            backup = Some(slashed(&copy));
        }
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    write_atomic(&path, &text)?;
    store.record_todo_export(&key, &new_hash, actor)?;
    Ok(Export::Written { path: key, backup })
}

/// The export that follows a change: a failure comes back as [`Export::Failed`], never as an error, because the change
/// it follows is already stored.
pub fn export_after(store: &mut Store, env: &Env<'_>) -> Export {
    export(store, env.home, env.config, None, env.actor).unwrap_or_else(|error| Export::Failed {
        error: format!("{error:#}"),
    })
}

/// Called after a task closed: exports when a to-do item was finished by it. Never fails the close.
pub fn after_close(store: &mut Store, env: &Env<'_>) {
    drop(export_after(store, env));
}

fn changed(store: &mut Store, env: &Env<'_>, items: &[Todo]) -> Change {
    Change {
        items: items.iter().map(Todo::line).collect(),
        export: export_after(store, env),
    }
}

/// Reads the file and adds the items the store lacks; with `dry_run` only counts them. Never changes or deletes a row
/// and never writes the file.
///
/// # Errors
///
/// Fails when no file is given or set, it cannot be read, or the store fails.
pub fn import(
    store: &mut Store,
    home: &Home,
    config: &Config,
    file: Option<&Path>,
    dry_run: bool,
    actor: Actor,
) -> Result<ImportReport> {
    let Some(path) = target(home, config, file) else {
        bail!("no to-do file: set [todo] file in the config or pass --file");
    };
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let parsed = todo::parse(&String::from_utf8_lossy(&bytes));
    let key = slashed(&path);
    let counts = if dry_run {
        store.todo_import_plan(&parsed.items)?
    } else {
        store.import_todos(&key, &parsed.items, actor)?
    };
    Ok(ImportReport {
        path: key,
        dry_run,
        counts,
        unparsed: parsed.unparsed,
    })
}

/// Adds an item and exports.
///
/// # Errors
///
/// Fails when the store fails.
pub fn add(store: &mut Store, env: &Env<'_>, request: &AddRequest) -> Result<Change> {
    let owner = request
        .owner
        .unwrap_or_else(|| todo::owner_of(&request.text));
    let draft = TodoDraft {
        project: request.project.clone(),
        section: request.section.clone(),
        text: request.text.trim().to_owned(),
        notes: request.notes.clone(),
        state: TodoState::Open,
        owner,
        added: Some(store.todo_today()?),
        not_before: request.not_before.clone(),
        due: request.due.clone(),
        done_on: None,
        done_note: None,
    };
    let item = store.add_todo(&draft, env.actor)?;
    Ok(changed(store, env, &[item]))
}

/// Marks an item done, or dropped, and exports.
///
/// # Errors
///
/// Fails when the item is unknown or not open, or the store fails.
pub fn finish(
    store: &mut Store,
    env: &Env<'_>,
    id: i64,
    note: Option<&str>,
    drop: bool,
) -> Result<Change> {
    let item = store.finish_todo(id, note, drop, env.actor)?;
    Ok(changed(store, env, &[item]))
}

/// Takes an item back to open and exports.
///
/// # Errors
///
/// Fails when the item is unknown or already open, or the store fails.
pub fn reopen(store: &mut Store, env: &Env<'_>, id: i64) -> Result<Change> {
    let item = store.reopen_todo(id, env.actor)?;
    Ok(changed(store, env, &[item]))
}

/// Changes an item and exports.
///
/// # Errors
///
/// Fails when the item is unknown or the store fails.
pub fn edit(store: &mut Store, env: &Env<'_>, id: i64, edit: &TodoEdit) -> Result<Change> {
    let item = store.edit_todo(id, edit, env.actor)?;
    Ok(changed(store, env, &[item]))
}

/// `spawn` for a task that comes from a to-do item: refuses an item that is not open or that a live task holds before
/// the task exists, links the item to the task, and exports.
///
/// # Errors
///
/// As [`spawn`], and the refusal of the link.
pub fn spawn_linked(
    store: &mut Store,
    env: &Env<'_>,
    request: &SpawnRequest<'_>,
    item: Option<i64>,
) -> Result<(headwork_store::Task, Vec<Pumped>)> {
    if let Some(id) = item {
        store.check_todo_linkable(id)?;
    }
    let (task, pumped) = spawn(store, env, request)?;
    if let Some(id) = item {
        store.link_todo(id, TaskId(task.id.0), env.actor)?;
        drop(export_after(store, env));
    }
    Ok((task, pumped))
}

/// The items that match, one line each.
///
/// # Errors
///
/// Fails when the store fails.
pub fn list(store: &Store, filter: &TodoFilter) -> Result<Vec<Todo>> {
    Ok(store.todos(filter)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_ignores_line_ending_style() {
        assert_eq!(hash("a\nb\n"), hash("a\r\nb\r\n"));
        assert_ne!(hash("a\nb\n"), hash("a\nc\n"));
    }
}
