//! The to-do list in the store and its `todo.md` (`docs/todo.md`): the changes the CLI and the orchestrator's tools make,
//! the import of the file, and the export that writes it back.
//!
//! The store is the source; the file is a copy git carries. An export never writes a file that was not imported first
//! and keeps a copy of a file edited by hand since the last export.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use headwork_core::todo::{
    self, ExportFacts, ExportPlan, Todo, TodoDate, TodoDraft, TodoOwner, TodoState, Unparsed,
    clean_note,
};
use headwork_core::{Actor, Config};
use headwork_store::{ImportCounts, Store, Task, TaskId, TodoEdit, TodoFilter};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::spawn::{Pumped, SpawnRequest, spawn};
use super::{Env, slashed, stage_file};
use crate::home::Home;

/// The event an export logs when it replaced a file edited by hand since the last export; it names the copy.
pub const EVENT_EXPORT_OVERWROTE: &str = "todo-export-overwrote";

/// The event a close logs when the export that followed it failed; the close itself stands.
pub const EVENT_EXPORT_FAILED: &str = "todo-export-failed";

/// The folder in the home folder that holds the copies of overwritten files.
pub const TODO_BACKUP_DIR: &str = "todo-backups";

/// How many copies of overwritten files are kept.
pub const TODO_BACKUPS_KEPT: usize = 10;

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
    /// The item text; date groups in it become the fields and a `For you:` prefix the owner, as in an import.
    pub text: String,
    /// A `###` subheading.
    pub section: Option<String>,
    /// Who acts; the user when the text starts with `For you:`, an agent otherwise, when absent.
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

fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with(r"UNC\") => PathBuf::from(rest),
        _ => path,
    }
}

fn absolute(path: &Path) -> PathBuf {
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if let Ok(real) = std::fs::canonicalize(&full) {
        return without_verbatim_prefix(real);
    }
    let parent = full
        .parent()
        .and_then(|dir| std::fs::canonicalize(dir).ok());
    match (parent, full.file_name()) {
        (Some(dir), Some(name)) => without_verbatim_prefix(dir).join(name),
        _ => full,
    }
}

/// The file the to-do list is exported to and imported from: `file` when given, else `[todo] file`.
///
/// The path is made absolute, and resolved when it exists, so that every way of naming one file is the same record in the
/// store.
#[must_use]
pub fn target(home: &Home, config: &Config, file: Option<&Path>) -> Option<PathBuf> {
    file.map(Path::to_path_buf)
        .or_else(|| config.todo.file.as_deref().map(|path| home.expand(path)))
        .map(|path| absolute(&path))
}

fn copy_aside(home: &Home, path: &Path) -> Result<PathBuf> {
    let dir = home.root().join(TODO_BACKUP_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let copy = dir.join(format!("todo-{:015}.md", millis()));
    std::fs::copy(path, &copy)
        .with_context(|| format!("copying {} to {}", path.display(), copy.display()))?;
    let mut copies: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry
            .with_context(|| format!("reading {}", dir.display()))?
            .path();
        let is_copy = path.extension().is_some_and(|extension| extension == "md")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("todo-"));
        if is_copy {
            copies.push(path);
        }
    }
    copies.sort();
    let surplus = copies.len().saturating_sub(TODO_BACKUPS_KEPT);
    for old in copies.into_iter().take(surplus) {
        std::fs::remove_file(&old).with_context(|| format!("removing {}", old.display()))?;
    }
    Ok(copy)
}

/// Writes the to-do list to its file when the content changed since the last export.
///
/// A file that exists but was never imported (and was never exported by headwork) is left alone. A file whose content is
/// not what the last export wrote was edited by hand: it is copied to `<home>/todo-backups` once the new content is
/// written to a temp file, and `todo-export-overwrote` is logged; a failed write makes no copy and no event.
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
    let file_exists = path.exists();
    let on_disk_hash = if file_exists {
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        Some(hash(&String::from_utf8_lossy(&bytes)))
    } else {
        None
    };
    let facts = ExportFacts {
        imported: record
            .as_ref()
            .is_some_and(|record| record.imported_at.is_some() || record.export_hash.is_some()),
        exported_hash: record
            .as_ref()
            .and_then(|record| record.export_hash.as_deref()),
        file_exists,
        on_disk_hash: on_disk_hash.as_deref(),
        new_hash: &new_hash,
    };
    match todo::plan_export(&facts) {
        ExportPlan::Unchanged => Ok(Export::Unchanged),
        ExportPlan::NotImported => Ok(Export::NotImported { path: key }),
        ExportPlan::Write { back_up } => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let staged = stage_file(&path, &text)?;
            let backup = if back_up {
                let copy = slashed(&copy_aside(home, &path)?);
                store.append_event(
                    actor,
                    None,
                    None,
                    EVENT_EXPORT_OVERWROTE,
                    &serde_json::json!({ "path": key, "backup": copy }),
                )?;
                Some(copy)
            } else {
                None
            };
            staged.commit()?;
            store.record_todo_export(&key, &new_hash, actor)?;
            Ok(Export::Written { path: key, backup })
        }
    }
}

/// The export that follows a change: a failure comes back as [`Export::Failed`], never as an error, because the change
/// it follows is already stored.
pub fn export_after(store: &mut Store, env: &Env<'_>) -> Export {
    export(store, env.home, env.config, None, env.actor).unwrap_or_else(|error| Export::Failed {
        error: format!("{error:#}"),
    })
}

/// Called after every close: exports (a no-op when nothing changed, so a close that finished no item writes nothing). A
/// failed export logs `todo-export-failed` with the error and never fails the close.
pub fn after_close(store: &mut Store, env: &Env<'_>) {
    if let Export::Failed { error } = export_after(store, env) {
        drop(store.append_event(
            env.actor,
            None,
            None,
            EVENT_EXPORT_FAILED,
            &serde_json::json!({ "error": error }),
        ));
    }
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
/// Fails when no file is given or set, it cannot be read or is not valid UTF-8, or the store fails.
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
    let text = String::from_utf8(bytes)
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    let parsed = todo::parse(&text);
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

/// Adds an item and exports. The text is read as an import reads a line: date groups in it become the fields (an
/// explicit date wins) and a leading `For you:` becomes the owner (an explicit owner wins).
///
/// # Errors
///
/// Fails when a field cannot be written to the file (empty, several lines, a project that starts with `#`, Done or
/// Dropped) or the store fails.
pub fn add(store: &mut Store, env: &Env<'_>, request: &AddRequest) -> Result<Change> {
    let found = todo::normalize(request.text.trim());
    let today = store.todo_today()?;
    let draft = TodoDraft {
        project: request.project.trim().to_owned(),
        section: request
            .section
            .as_deref()
            .map(|section| section.trim().to_owned()),
        text: found.text,
        notes: request.notes.iter().map(|note| clean_note(note)).collect(),
        state: TodoState::Open,
        owner: request.owner.or(found.owner).unwrap_or(TodoOwner::Agent),
        added: found.added.or(Some(today)),
        not_before: request.not_before.clone().or(found.not_before),
        due: request.due.clone().or(found.due),
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
/// A new text is read as in [`add`]: date groups in it become the fields (an explicit date wins) and a leading `For you:`
/// becomes the owner (an explicit owner wins); the other fields are trimmed.
///
/// # Errors
///
/// Fails when the item is unknown, a field cannot be written to the file, or the store fails.
pub fn edit(store: &mut Store, env: &Env<'_>, id: i64, edit: &TodoEdit) -> Result<Change> {
    let mut edit = edit.clone();
    if let Some(text) = &edit.text {
        let found = todo::normalize(text.trim());
        edit.text = Some(found.text);
        edit.owner = edit.owner.or(found.owner);
        edit.added = found.added;
        edit.not_before = edit.not_before.or_else(|| found.not_before.map(Some));
        edit.due = edit.due.or_else(|| found.due.map(Some));
    }
    edit.project = edit.project.map(|project| project.trim().to_owned());
    edit.section = edit
        .section
        .map(|section| section.map(|section| section.trim().to_owned()));
    edit.append = edit.append.map(|note| clean_note(&note));
    let item = store.edit_todo(id, &edit, env.actor)?;
    Ok(changed(store, env, &[item]))
}

/// What spawning a task for a to-do item did.
#[derive(Debug)]
pub struct LinkedSpawn {
    /// The task.
    pub task: Task,
    /// The queued tasks the spawn started.
    pub pumped: Vec<Pumped>,
    /// Why the item was not linked, when the link was refused after the task started: the task then runs unlinked.
    pub unlinked: Option<String>,
}

/// `spawn` for a task that comes from a to-do item.
///
/// The check that the item is open and that no live task holds it runs before the spawn; the link runs after it. A link
/// refused after the task started leaves the task running unlinked and says so in [`LinkedSpawn::unlinked`], so the
/// caller can tell the user.
///
/// # Errors
///
/// As [`spawn`], and the refusal of the check before it.
pub fn spawn_linked(
    store: &mut Store,
    env: &Env<'_>,
    request: &SpawnRequest<'_>,
    item: Option<i64>,
) -> Result<LinkedSpawn> {
    if let Some(id) = item {
        store.check_todo_linkable(id)?;
    }
    let (task, pumped) = spawn(store, env, request)?;
    let mut unlinked = None;
    if let Some(id) = item {
        match store.link_todo(id, TaskId(task.id.0), env.actor) {
            Ok(_) => drop(export_after(store, env)),
            Err(error) => {
                unlinked = Some(format!(
                    "task {} runs without a link to to-do item {id}: {error}",
                    task.name
                ));
            }
        }
    }
    Ok(LinkedSpawn {
        task,
        pumped,
        unlinked,
    })
}

/// The items that match, in export order. `state` is `open` (the default when absent), `done`, `dropped` or `all`.
///
/// # Errors
///
/// Fails when `state` is none of those or the store fails.
pub fn list(
    store: &Store,
    project: Option<&str>,
    owner: Option<TodoOwner>,
    state: Option<&str>,
) -> Result<Vec<Todo>> {
    let state = match state {
        None | Some("open") => Some(TodoState::Open),
        Some("all") => None,
        Some(other) => Some(other.parse()?),
    };
    Ok(store.todos(&TodoFilter {
        project: project.map(str::to_owned),
        owner,
        state,
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_ignores_line_ending_style() {
        assert_eq!(hash("a\nb\n"), hash("a\r\nb\r\n"));
        assert_ne!(hash("a\nb\n"), hash("a\nc\n"));
    }

    #[test]
    fn a_verbatim_prefix_is_removed_but_a_network_path_is_kept() {
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\C:\x\todo.md")),
            PathBuf::from(r"C:\x\todo.md")
        );
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\UNC\host\share\todo.md")),
            PathBuf::from(r"\\?\UNC\host\share\todo.md")
        );
        assert_eq!(
            without_verbatim_prefix(PathBuf::from("/x/todo.md")),
            PathBuf::from("/x/todo.md")
        );
    }
}
