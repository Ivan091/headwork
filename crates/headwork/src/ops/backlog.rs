//! The backlog list in the store and its `backlog.md` (`docs/backlog.md`): the changes the CLI and the orchestrator's tools make,
//! the import of the file, and the export that writes it back.
//!
//! The store is the source; the file is a copy git carries. An export never writes a file that was not imported first
//! and keeps a copy of a file edited by hand since the last export.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use headwork_core::backlog::{
    self, BacklogActor, BacklogDate, BacklogDraft, BacklogItem, BacklogOwner, BacklogStatus,
    BlockReason, ExportFacts, ExportPlan, ItemStatus, Target, Unparsed, clean_note,
};
use headwork_core::{Actor, Config};
use headwork_store::{BacklogEdit, BacklogFilter, BacklogMove, ImportCounts, Store, Task, TaskId};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::spawn::{Pumped, SpawnRequest, spawn};
use super::{Env, slashed, stage_file};
use crate::home::Home;

/// The event an export logs when it replaced a file edited by hand since the last export; it names the copy.
pub const EVENT_EXPORT_OVERWROTE: &str = "backlog-export-overwrote";

/// The event an export logs when it wrote a file used for the first time while another file was used before.
///
/// It names both and whether the old one got the pointer line. Before stored statuses (2026-11-27) it was `backlog-moved`,
/// which now names an item's move.
pub const EVENT_BACKLOG_FILE_MOVED: &str = "backlog-file-moved";

/// The event a close logs when the export that followed it failed; the close itself stands.
pub const EVENT_EXPORT_FAILED: &str = "backlog-export-failed";

/// The folder in the home folder that holds the copies of overwritten files. The folder `todo-backups` of earlier builds is
/// left where it is: never listed, pruned, moved or deleted.
pub const BACKLOG_BACKUP_DIR: &str = "backlog-backups";

/// How many copies of overwritten files are kept.
pub const BACKLOG_BACKUPS_KEPT: usize = 10;

/// What an export did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "export", rename_all = "snake_case")]
pub enum Export {
    /// No file is set: neither `[backlog] file` nor `--file`.
    Unconfigured,
    /// The content equals the last export; nothing was written.
    Unchanged,
    /// The file was written.
    Written {
        /// The file.
        path: String,
        /// The copy of the file as it was, when someone had edited it since the last export.
        backup: Option<String>,
        /// The file used before, when this export was the first to this file.
        moved: Option<Moved>,
    },
    /// The file exists and was never imported, so nothing was written.
    NotImported {
        /// The file.
        path: String,
    },
    /// Another export held the lock for longer than this one waited; nothing was written.
    Busy,
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
            Self::Unconfigured => {
                "no backlog file is set ([backlog] file), nothing exported".to_owned()
            }
            Self::Unchanged => "the backlog file is up to date".to_owned(),
            Self::Written {
                path,
                backup,
                moved,
            } => {
                let mut line = format!("exported to {path}");
                if let Some(backup) = backup {
                    line.push_str(
                        "; the file differed from the last export (edited by hand, or never exported), a copy is in ",
                    );
                    line.push_str(backup);
                }
                if let Some(moved) = moved {
                    line.push_str(&moved.summary());
                }
                line
            }
            Self::NotImported { path } => {
                format!(
                    "{path} exists and was never imported: run `headwork backlog import` first; nothing exported"
                )
            }
            Self::Busy => "another export is running; nothing exported".to_owned(),
            Self::Failed { error } => format!("export failed: {error}"),
        }
    }
}

/// The file an export moved away from: the one used before, left in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Moved {
    /// The file used before.
    pub from: String,
    /// Whether it now holds the pointer line (`[backlog] leave_pointer`).
    pub pointer: bool,
    /// The copy of its text taken before the pointer replaced it.
    pub backup: Option<String>,
}

impl Moved {
    fn summary(&self) -> String {
        match (&self.backup, self.pointer) {
            (Some(backup), true) => format!(
                "; moved from {}, which now points here (its text is in {backup})",
                self.from
            ),
            (None, true) => format!("; moved from {}, which now points here", self.from),
            (_, false) => format!("; moved from {}, left as it was", self.from),
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
    pub owner: Option<BacklogOwner>,
    /// The first day it may start.
    pub not_before: Option<BacklogDate>,
    /// The day it is due.
    pub due: Option<BacklogDate>,
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

/// The file the backlog list is exported to and imported from: `file` when given, else `[backlog] file`.
///
/// The path is made absolute, and resolved when it exists, so that every way of naming one file is the same record in the
/// store.
#[must_use]
pub fn target(home: &Home, config: &Config, file: Option<&Path>) -> Option<PathBuf> {
    file.map(Path::to_path_buf)
        .or_else(|| config.backlog.file.as_deref().map(|path| home.expand(path)))
        .map(|path| absolute(&path))
}

fn backup_copies(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut copies: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry
            .with_context(|| format!("reading {}", dir.display()))?
            .path();
        let is_copy = path.extension().is_some_and(|extension| extension == "md")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("backlog-"));
        if is_copy {
            copies.push(path);
        }
    }
    copies.sort();
    Ok(copies)
}

/// A copy of a file edited by hand, and whether this call made it.
struct Aside {
    copy: PathBuf,
    fresh: bool,
}

/// Copies `path` to the backup folder unless a kept copy already holds the same content (`content_hash`), so a write that
/// keeps failing makes one copy of a hand edit, not one per attempt.
fn copy_aside(home: &Home, path: &Path, content_hash: &str) -> Result<Aside> {
    let dir = home.root().join(BACKLOG_BACKUP_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut copies = backup_copies(&dir)?;
    for kept in copies.iter().rev() {
        let held = std::fs::read(kept)
            .ok()
            .map(|bytes| hash(&String::from_utf8_lossy(&bytes)));
        if held.as_deref() == Some(content_hash) {
            return Ok(Aside {
                copy: kept.clone(),
                fresh: false,
            });
        }
    }
    let copy = dir.join(format!("backlog-{:015}.md", millis()));
    std::fs::copy(path, &copy)
        .with_context(|| format!("copying {} to {}", path.display(), copy.display()))?;
    copies.push(copy.clone());
    copies.sort();
    let surplus = copies.len().saturating_sub(BACKLOG_BACKUPS_KEPT);
    for old in copies.into_iter().take(surplus) {
        std::fs::remove_file(&old).with_context(|| format!("removing {}", old.display()))?;
    }
    Ok(Aside { copy, fresh: true })
}

/// The export lock file in the home folder.
pub const EXPORT_LOCK_FILE: &str = "backlog-export.lock";

/// How long an export waits for the lock another export holds.
pub const EXPORT_LOCK_WAIT: Duration = Duration::from_secs(5);

/// How old a lock file must be to count as left behind by a dead export.
pub const EXPORT_LOCK_STALE: Duration = Duration::from_secs(600);

/// The export lock, held until this value is dropped.
#[derive(Debug)]
struct ExportLock {
    path: PathBuf,
}

impl Drop for ExportLock {
    fn drop(&mut self) {
        drop(std::fs::remove_file(&self.path));
    }
}

fn lock_is_stale(path: &Path, stale: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= stale)
}

/// Takes the export lock: a file created exclusively, so a second export waits up to `wait` and then gets `None`. A lock
/// file older than `stale` was left by an export that died and is taken over.
fn take_export_lock(home: &Home, wait: Duration, stale: Duration) -> Result<Option<ExportLock>> {
    let path = home.root().join(EXPORT_LOCK_FILE);
    let start = Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_file) => return Ok(Some(ExportLock { path })),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(&path, stale) {
                    drop(std::fs::remove_file(&path));
                } else if start.elapsed() >= wait {
                    return Ok(None);
                } else {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", path.display()));
            }
        }
    }
}

/// Writes the backlog list to its file when the content changed since the last export.
///
/// A file that exists but was never imported (and was never exported by headwork) is left alone. A file whose content is
/// not what the last export wrote was edited by hand: it is copied to `<home>/backlog-backups` once the new content is
/// written to a temp file, and `backlog-export-overwrote` is logged; a failed write makes no copy and no event.
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
    let wait = EXPORT_LOCK_WAIT;
    let stale = EXPORT_LOCK_STALE;
    export_locked(store, home, config, file, actor, (wait, stale))
}

fn export_locked(
    store: &mut Store,
    home: &Home,
    config: &Config,
    file: Option<&Path>,
    actor: Actor,
    (wait, stale): (Duration, Duration),
) -> Result<Export> {
    let Some(path) = target(home, config, file) else {
        return Ok(Export::Unconfigured);
    };
    let Some(_lock) = take_export_lock(home, wait, stale)? else {
        return Ok(Export::Busy);
    };
    let key = slashed(&path);
    let text = backlog::render(&store.backlog_items(&BacklogFilter::default())?);
    let new_hash = hash(&text);
    let record = store.backlog_file(&key)?;
    let configured = file.is_none();
    let source = if configured {
        move_source(store, &key)?
    } else {
        None
    };
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
    match backlog::plan_export(&facts) {
        ExportPlan::Unchanged => {
            let Some(from) = source else {
                return Ok(Export::Unchanged);
            };
            let moved = point_away(home, config, from, &key)?;
            store.record_backlog_export(&key, &new_hash, configured, actor)?;
            finish_move(store, &moved, &key, actor)?;
            Ok(Export::Written {
                path: key,
                backup: None,
                moved: Some(moved),
            })
        }
        ExportPlan::NotImported => Ok(Export::NotImported { path: key }),
        ExportPlan::Write { back_up } => {
            let moved = source
                .map(|from| point_away(home, config, from, &key))
                .transpose()?;
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let staged = stage_file(&path, &text)?;
            let backup = if back_up {
                let aside = copy_aside(home, &path, on_disk_hash.as_deref().unwrap_or_default())?;
                let copy = slashed(&aside.copy);
                if aside.fresh {
                    store.append_event(
                        actor,
                        None,
                        None,
                        EVENT_EXPORT_OVERWROTE,
                        &serde_json::json!({ "path": key, "backup": copy }),
                    )?;
                }
                Some(copy)
            } else {
                None
            };
            staged.commit()?;
            store.record_backlog_export(&key, &new_hash, configured, actor)?;
            if let Some(moved) = &moved {
                finish_move(store, moved, &key, actor)?;
            }
            Ok(Export::Written {
                path: key,
                backup,
                moved,
            })
        }
    }
}

/// The file an export of the configured file to `key` moves away from ([`backlog::move_source`]).
fn move_source(store: &Store, key: &str) -> Result<Option<String>> {
    let files = store.backlog_files()?;
    let configured: Vec<(&str, &str)> = files
        .iter()
        .filter_map(|file| Some((file.path.as_str(), file.configured_at.as_deref()?)))
        .collect();
    Ok(backlog::move_source(key, &configured).map(str::to_owned))
}

/// The first half of a move to `key`, done before the new file is written or recorded: the file used before is left in
/// place, and given the pointer line when `[backlog] leave_pointer` is on and it exists, after a copy of its text goes to
/// the backup folder. A file that already holds the pointer is left as it is, so a move that failed later is retried
/// without a second copy.
fn point_away(home: &Home, config: &Config, from: String, key: &str) -> Result<Moved> {
    let old_path = PathBuf::from(&from);
    let mut moved = Moved {
        from,
        pointer: false,
        backup: None,
    };
    if config.backlog.leave_pointer && old_path.is_file() {
        let line = backlog::pointer_line(key);
        let bytes =
            std::fs::read(&old_path).with_context(|| format!("reading {}", old_path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        if text != line {
            let staged = stage_file(&old_path, &line)?;
            let aside = copy_aside(home, &old_path, &hash(&text))?;
            staged.commit()?;
            moved.backup = Some(slashed(&aside.copy));
        }
        moved.pointer = true;
    }
    Ok(moved)
}

/// The second half of a move, once the new file is recorded: the pointer's hash becomes the old file's last export, so
/// pointing `[backlog] file` back at it overwrites the pointer without a copy, and `backlog-file-moved` is logged.
fn finish_move(store: &Store, moved: &Moved, key: &str, actor: Actor) -> Result<()> {
    if moved.pointer {
        store.record_backlog_pointer(&moved.from, &hash(&backlog::pointer_line(key)))?;
    }
    store.append_event(
        actor,
        None,
        None,
        EVENT_BACKLOG_FILE_MOVED,
        &serde_json::json!({ "from": moved.from, "to": key, "pointer": moved.pointer, "backup": moved.backup }),
    )?;
    Ok(())
}

/// The export that follows a change: a failure comes back as [`Export::Failed`], never as an error, because the change
/// it follows is already stored.
pub fn export_after(store: &mut Store, env: &Env<'_>) -> Export {
    export(store, env.home, env.config, None, env.actor).unwrap_or_else(|error| Export::Failed {
        error: format!("{error:#}"),
    })
}

/// Called after every close: exports (a no-op when nothing changed, so a close that finished no item writes nothing). A
/// failed export logs `backlog-export-failed` with the error and never fails the close.
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

fn changed(store: &mut Store, env: &Env<'_>, items: &[BacklogItem]) -> Change {
    Change {
        items: items.iter().map(BacklogItem::line).collect(),
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
        bail!("no backlog file: set [backlog] file in the config or pass --file");
    };
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let text = String::from_utf8(bytes)
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    let parsed = backlog::parse(&text);
    let key = slashed(&path);
    let counts = if dry_run {
        store.backlog_import_plan(&parsed.items)?
    } else {
        store.import_backlog_items(&key, &parsed.items, actor)?
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
    add_for(store, env, request, None)
}

/// [`add`] for an exploration: the `backlog-added` event carries `task`.
///
/// # Errors
///
/// As [`add`].
pub fn add_for(
    store: &mut Store,
    env: &Env<'_>,
    request: &AddRequest,
    task: Option<TaskId>,
) -> Result<Change> {
    let found = backlog::normalize(request.text.trim());
    let today = store.backlog_today()?;
    let draft = BacklogDraft {
        project: request.project.trim().to_owned(),
        section: request
            .section
            .as_deref()
            .map(|section| section.trim().to_owned()),
        text: found.text,
        notes: request.notes.iter().map(|note| clean_note(note)).collect(),
        status: BacklogStatus::Backlog,
        owner: request.owner.or(found.owner).unwrap_or(BacklogOwner::Agent),
        added: found.added.or(Some(today)),
        not_before: request.not_before.clone().or(found.not_before),
        due: request.due.clone().or(found.due),
        done_on: None,
        done_note: None,
    };
    let item = store.add_backlog_item_for(&draft, env.actor, task)?;
    Ok(changed(store, env, &[item]))
}

/// Who moves an item for the guard, from who acts.
///
/// The CLI with no `HEADWORK_ROLE` is the user, the orchestrator's tools and the CLI it runs are the orchestrator, a worker
/// is a worker, and the tick and hooks are headwork.
#[must_use]
pub const fn backlog_actor(actor: Actor) -> BacklogActor {
    match actor {
        Actor::User => BacklogActor::User,
        Actor::Orchestrator => BacklogActor::Orchestrator,
        Actor::Worker => BacklogActor::Worker,
        Actor::Tick | Actor::Hook => BacklogActor::Headwork,
    }
}

fn project_known(config: &Config, project: &str) -> bool {
    config
        .projects
        .keys()
        .any(|name| name.to_string() == project)
}

/// Moves an item through the status guard and exports: the actor is [`backlog_actor`] of who acts, and a start needs the
/// item's project to be one headwork runs.
///
/// # Errors
///
/// Fails when the item is unknown, the guard refuses the move (its reason is the message), or the store fails.
pub fn move_item(
    store: &mut Store,
    env: &Env<'_>,
    id: i64,
    target: Target,
    reason: Option<&str>,
) -> Result<Change> {
    let has_project = project_known(env.config, &store.backlog_item(id)?.draft.project);
    let request = BacklogMove {
        has_project,
        ..BacklogMove::new(target, backlog_actor(env.actor), env.actor)
            .with_reason(reason.map(str::trim).filter(|reason| !reason.is_empty()))
    };
    let item = store.move_backlog_item(id, &request)?;
    Ok(changed(store, env, &[item]))
}

/// Marks an item done today with a note, and exports; refused while a live task works on it.
///
/// # Errors
///
/// As [`move_item`].
pub fn finish(store: &mut Store, env: &Env<'_>, id: i64, note: Option<&str>) -> Result<Change> {
    move_item(store, env, id, Target::Done, note)
}

/// Cancels an item today with a reason, and exports; refused while a live task works on it, and for the orchestrator
/// without a reason.
///
/// # Errors
///
/// As [`move_item`].
pub fn cancel(store: &mut Store, env: &Env<'_>, id: i64, reason: Option<&str>) -> Result<Change> {
    move_item(store, env, id, Target::Cancelled, reason)
}

/// Spins an item up: backlog to in progress. It spawns nothing; `spawn --backlog` links a task.
///
/// # Errors
///
/// As [`move_item`].
pub fn start(store: &mut Store, env: &Env<'_>, id: i64) -> Result<Change> {
    move_item(store, env, id, Target::InProgress, None)
}

/// Blocks an item for `reason` (the user and the orchestrator give `decision` or `event`) with a note.
///
/// # Errors
///
/// As [`move_item`].
pub fn block(
    store: &mut Store,
    env: &Env<'_>,
    id: i64,
    reason: BlockReason,
    note: Option<&str>,
) -> Result<Change> {
    let note = note.map(str::trim).filter(|note| !note.is_empty());
    move_item(
        store,
        env,
        id,
        Target::Blocked {
            reason,
            note: note.map(str::to_owned),
        },
        None,
    )
}

/// Takes a blocked item back to the status it had before its block.
///
/// # Errors
///
/// As [`move_item`].
pub fn unblock(store: &mut Store, env: &Env<'_>, id: i64) -> Result<Change> {
    move_item(store, env, id, Target::Unblock, None)
}

/// Takes a done or cancelled item back to the backlog and exports.
///
/// # Errors
///
/// As [`move_item`].
pub fn reopen(store: &mut Store, env: &Env<'_>, id: i64) -> Result<Change> {
    move_item(store, env, id, Target::Backlog, None)
}

/// Changes an item and exports.
///
/// A new text is read as in [`add`]: date groups in it become the fields (an explicit date wins) and a leading `For you:`
/// becomes the owner (an explicit owner wins); the other fields are trimmed.
///
/// # Errors
///
/// Fails when the item is unknown, a field cannot be written to the file, or the store fails.
pub fn edit(store: &mut Store, env: &Env<'_>, id: i64, edit: &BacklogEdit) -> Result<Change> {
    let mut edit = edit.clone();
    if let Some(text) = &edit.text {
        let found = backlog::normalize(text.trim());
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
    let item = store.edit_backlog_item(id, &edit, env.actor)?;
    Ok(changed(store, env, &[item]))
}

/// What spawning a task for a backlog item did.
#[derive(Debug)]
pub struct LinkedSpawn {
    /// The task.
    pub task: Task,
    /// The queued tasks the spawn started.
    pub pumped: Vec<Pumped>,
    /// Why the item was not linked, when the link was refused after the task started: the task then runs unlinked.
    pub unlinked: Option<String>,
}

/// `spawn` for a task that comes from a backlog item: the item moves to in progress, linked to the task.
///
/// The guard runs before the spawn (from the backlog, from in progress with no live task, or a respawn of an item blocked
/// as `task_lost` or `partial`); the move runs after it. A move refused after the task started leaves the task running
/// unlinked and says so in [`LinkedSpawn::unlinked`], so the caller can tell the user.
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
    let link = |task: Option<TaskId>| BacklogMove {
        link: task,
        ..BacklogMove::new(Target::InProgress, backlog_actor(env.actor), env.actor)
    };
    if let Some(id) = item {
        store.check_backlog_move(id, &link(None))?;
    }
    let (task, pumped) = spawn(store, env, request)?;
    let mut unlinked = None;
    if let Some(id) = item {
        match store.move_backlog_item(id, &link(Some(TaskId(task.id.0)))) {
            Ok(_) => drop(export_after(store, env)),
            Err(error) => {
                unlinked = Some(format!(
                    "task {} runs without a link to backlog item {id}: {error}",
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

/// The statuses a `--status` word names: one status, `open` (backlog, in progress and blocked) or `all`.
///
/// # Errors
///
/// Fails when the word is none of those.
pub fn statuses(word: &str) -> Result<Vec<ItemStatus>> {
    Ok(match word {
        "open" => ItemStatus::OPEN.to_vec(),
        "all" => Vec::new(),
        other => vec![other.parse()?],
    })
}

/// The statuses an old `--state` word names, accepted for one release: `open` is backlog, in progress and blocked,
/// `dropped` is cancelled, and `done` and `all` stay.
///
/// # Errors
///
/// Fails when the word is none of `open`, `done`, `dropped` and `all`.
pub fn old_state(word: &str) -> Result<Vec<ItemStatus>> {
    match word {
        "open" | "done" | "all" => statuses(word),
        "dropped" => Ok(vec![ItemStatus::Cancelled]),
        other => bail!("unknown state `{other}`: open, done, dropped or all (use --status)"),
    }
}

/// The items that match, in export order. `status` is a status, `open` (the default when absent: backlog, in progress
/// and blocked) or `all`; `state` is the old word, read by [`old_state`] when `status` is absent.
///
/// # Errors
///
/// Fails when a word is unknown or the store fails.
pub fn list(
    store: &Store,
    project: Option<&str>,
    owner: Option<BacklogOwner>,
    status: Option<&str>,
    state: Option<&str>,
) -> Result<Vec<BacklogItem>> {
    let statuses = match (status, state) {
        (Some(word), _) => statuses(word)?,
        (None, Some(word)) => old_state(word)?,
        (None, None) => ItemStatus::OPEN.to_vec(),
    };
    Ok(store.backlog_items(&BacklogFilter {
        project: project.map(str::to_owned),
        owner,
        statuses,
    })?)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test")]
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
            without_verbatim_prefix(PathBuf::from(r"\\?\C:\x\backlog.md")),
            PathBuf::from(r"C:\x\backlog.md")
        );
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\UNC\host\share\backlog.md")),
            PathBuf::from(r"\\?\UNC\host\share\backlog.md")
        );
        assert_eq!(
            without_verbatim_prefix(PathBuf::from("/x/backlog.md")),
            PathBuf::from("/x/backlog.md")
        );
    }

    fn home_at(dir: &Path) -> Home {
        std::fs::create_dir_all(dir.join("home")).unwrap();
        Home::at(Some(dir.join("home"))).unwrap()
    }

    #[test]
    fn a_hand_edit_is_copied_once_however_often_the_copy_is_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let home = home_at(dir.path());
        let file = dir.path().join("backlog.md");
        std::fs::write(&file, "edited by hand\n").unwrap();
        let content = hash("edited by hand\n");
        let first = copy_aside(&home, &file, &content).unwrap();
        let second = copy_aside(&home, &file, &content).unwrap();
        assert!(first.fresh, "the first call copies");
        assert!(!second.fresh, "the second finds the copy");
        assert_eq!(first.copy, second.copy);
        std::fs::write(&file, "edited again\n").unwrap();
        assert!(
            copy_aside(&home, &file, &hash("edited again\n"))
                .unwrap()
                .fresh
        );
    }

    #[test]
    fn a_held_lock_gives_up_and_a_stale_one_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let home = home_at(dir.path());
        let held = take_export_lock(&home, Duration::ZERO, EXPORT_LOCK_STALE)
            .unwrap()
            .unwrap();
        assert!(
            take_export_lock(&home, Duration::from_millis(120), EXPORT_LOCK_STALE)
                .unwrap()
                .is_none(),
            "a live holder makes the second give up"
        );
        assert!(
            take_export_lock(&home, Duration::ZERO, Duration::ZERO)
                .unwrap()
                .is_some(),
            "a lock older than the stale age is taken"
        );
        drop(held);
        assert!(
            take_export_lock(&home, Duration::ZERO, EXPORT_LOCK_STALE)
                .unwrap()
                .is_some(),
            "a released lock is free"
        );
    }
}
