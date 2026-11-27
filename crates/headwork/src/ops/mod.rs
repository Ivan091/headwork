//! What the commands do, against a store, a config and a [`TerminalHost`], so it can be tested with the fake host.

pub mod answer;
pub mod backlog;
pub mod clean;
pub mod close;
pub mod config_sync;
pub mod dash;
pub mod digest;
pub mod doctor;
pub mod effort;
pub mod flags;
pub mod flows;
pub mod gate;
pub mod graph;
pub mod helpers;
pub mod hook;
pub mod import;
pub mod install;
pub mod jobs;
pub mod launch;
pub mod launchd;
pub mod leftovers;
pub mod lost;
pub mod mcp;
pub mod migrate;
pub mod park;
pub mod place;
pub mod prs;
pub mod reap;
pub mod recap;
pub mod relay;
pub mod reload;
pub mod resume;
pub mod review;
pub mod review_loop;
pub mod rules;
pub mod schedule;
pub mod session_procs;
pub mod shadow;
pub mod spawn;
pub mod status;
pub mod store_backup;
pub mod tick;
pub mod tick_lock;
pub mod title;
pub mod trust;
pub mod update;
pub mod usage;
pub mod waiting;
pub mod windowless;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use headwork_adapters::host::TerminalHost;
use headwork_adapters::host::cmux::{Cli as CmuxCli, Cmux};
use headwork_adapters::host::wmux::{Cli as WmuxCli, Wmux};
use headwork_adapters::worktree::Worktrees;
use headwork_core::config::{HostKind, ProjectConfig, TaskVars};
use headwork_core::{Actor, Config};
use headwork_store::{Store, Task};

use crate::home::Home;

/// Everything a command acts with, besides the store.
#[derive(Debug, Clone, Copy)]
pub struct Env<'a> {
    /// The home folder.
    pub home: &'a Home,
    /// The machine config.
    pub config: &'a Config,
    /// The terminal host.
    pub host: &'a dyn TerminalHost,
    /// The `headwork` program that panes run as their launcher.
    pub exe: &'a Path,
    /// Who acts.
    pub actor: Actor,
}

impl Env<'_> {
    /// The project config of a task.
    ///
    /// # Errors
    ///
    /// Fails when the project left the config.
    pub fn project(&self, task: &Task) -> Result<&ProjectConfig> {
        self.config
            .project(task.project())
            .with_context(|| format!("project `{}` is not configured", task.project()))
    }

    /// The project's main checkout.
    ///
    /// # Errors
    ///
    /// Fails when the project left the config.
    pub fn main_path(&self, task: &Task) -> Result<PathBuf> {
        Ok(self.home.expand(&self.project(task)?.main))
    }

    /// Where the task's session runs: its worktree, or the main checkout for a main-only task.
    ///
    /// # Errors
    ///
    /// Fails when the project left the config or its `worktree_path` template is bad.
    pub fn task_dir(&self, task: &Task) -> Result<PathBuf> {
        if task.in_main {
            return self.main_path(task);
        }
        self.worktree_dir(task, task.slug().as_str())
    }

    /// The folder the project's `worktree_path` gives a worktree whose slug and branch are `slug`: the task's own, or a
    /// helper worktree it makes for a part of its work.
    ///
    /// # Errors
    ///
    /// Fails when the project is not configured or the template is bad.
    pub fn worktree_dir(&self, task: &Task, slug: &str) -> Result<PathBuf> {
        let main_text = slashed(&self.main_path(task)?);
        let vars = TaskVars {
            slug,
            branch: slug,
            main: &main_text,
            worktree: "",
        };
        let template = &self.project(task)?.worktree_path;
        Ok(self.home.expand(&vars.expand(template)?))
    }

    /// The shell that runs commands: `[machine] shell`, else Git Bash.
    #[must_use]
    pub fn shell(&self) -> PathBuf {
        headwork_adapters::shell::resolve(self.config.machine.shell.as_deref())
    }

    /// Runs the project's worktree commands.
    ///
    /// # Errors
    ///
    /// Fails when the project left the config.
    pub fn worktrees(&self, task: &Task) -> Result<Worktrees> {
        Ok(Worktrees::new(self.shell(), self.main_path(task)?))
    }
}

/// The terminal host the config names.
///
/// # Errors
///
/// Never fails today; a host this build cannot drive would.
pub fn host_for(config: &Config) -> Result<Box<dyn TerminalHost>> {
    match config.machine.host {
        HostKind::Wmux => Ok(Box::new(Wmux::new(WmuxCli::locate(
            config.machine.host_bin.as_deref().map(Path::new),
        )))),
        HostKind::Cmux => Ok(Box::new(Cmux::new(CmuxCli::locate(
            config.machine.host_bin.as_deref().map(Path::new),
        )))),
    }
}

/// A path with forward slashes, as command templates and Git Bash expect.
#[must_use]
pub fn slashed(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// A file written in full next to its target and not yet in its place: [`StagedFile::commit`] renames it over the target,
/// and dropping it without a commit removes it.
#[derive(Debug)]
pub struct StagedFile {
    temp: Option<PathBuf>,
    target: PathBuf,
}

impl StagedFile {
    /// Puts the staged file in place of the target.
    ///
    /// # Errors
    ///
    /// Fails when the rename fails; the temp file is removed then.
    pub fn commit(mut self) -> Result<()> {
        let Some(temp) = self.temp.take() else {
            return Ok(());
        };
        std::fs::rename(&temp, &self.target).map_err(|error| {
            drop(std::fs::remove_file(&temp));
            anyhow::Error::new(error).context(format!(
                "replacing {} with {}",
                self.target.display(),
                temp.display()
            ))
        })
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Some(temp) = self.temp.take() {
            drop(std::fs::remove_file(temp));
        }
    }
}

/// Writes `text` to a temp file next to `path`, named for this process and call so two writers never share one, and
/// returns it ready to be committed.
///
/// # Errors
///
/// Fails when no temp file can be created or written.
pub fn stage_file(path: &Path, text: &str) -> Result<StagedFile> {
    use std::io::Write as _;

    let name = path.file_name().map_or_else(
        || "file".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    for attempt in 0_u32..1000 {
        let temp = path.with_file_name(format!(
            ".{name}.{}-{nanos}-{attempt}.tmp",
            std::process::id()
        ));
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp);
        match opened {
            Ok(mut file) => {
                let staged = StagedFile {
                    temp: Some(temp.clone()),
                    target: path.to_path_buf(),
                };
                file.write_all(text.as_bytes())
                    .with_context(|| format!("writing {}", temp.display()))?;
                return Ok(staged);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", temp.display()));
            }
        }
    }
    bail!("no free temp file name next to {}", path.display())
}

/// Replaces `path` with `text` through a temp file and a rename, so a reader never sees half a file.
///
/// # Errors
///
/// Fails when the temp file cannot be written or the rename fails.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    stage_file(path, text)?.commit()
}

/// The host label of a task's pane: `<project>/<slug>`.
#[must_use]
pub fn label(task: &Task) -> String {
    format!("{}/{}", task.project(), task.slug())
}

/// Turns a time the user typed (`--since`, `--at`) into an instant in UTC: a span back from now (`90m`, `24h`, `7d`),
/// or an RFC 3339 date and time or a plain date.
pub(crate) fn instant(store: &Store, text: &str) -> Result<String> {
    let text = text.trim();
    let span = text.char_indices().last().and_then(|(at, unit)| {
        let seconds: u64 = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        text.get(..at)?
            .parse::<u64>()
            .ok()
            .and_then(|amount| amount.checked_mul(seconds))
    });
    if let Some(seconds) = span {
        return Ok(store.instant_ago(seconds)?);
    }
    match store.instant_of(text)? {
        Some(instant) => Ok(instant),
        None => bail!(
            "cannot read `{text}` as a time: use YYYY-MM-DD, an RFC 3339 date and time, or a span such as 24h or 7d"
        ),
    }
}
