//! What the commands do, against a store, a config and a [`TerminalHost`], so it can be tested with the fake host.

pub mod launch;
pub mod spawn;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use headwork_adapters::host::TerminalHost;
use headwork_adapters::worktree::Worktrees;
use headwork_core::config::{ProjectConfig, TaskVars};
use headwork_core::{Actor, Config};
use headwork_store::Task;

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
        let main = self.main_path(task)?;
        if task.in_main {
            return Ok(main);
        }
        let main_text = slashed(&main);
        let vars = TaskVars {
            slug: task.slug().as_str(),
            branch: task.slug().as_str(),
            main: &main_text,
            worktree: "",
        };
        let template = &self.project(task)?.worktree_path;
        Ok(self.home.expand(&vars.expand(template)?))
    }

    /// Runs the project's worktree commands.
    ///
    /// # Errors
    ///
    /// Fails when the project left the config.
    pub fn worktrees(&self, task: &Task) -> Result<Worktrees> {
        let shell = self.config.machine.shell.as_deref().unwrap_or("bash");
        Ok(Worktrees::new(shell, self.main_path(task)?))
    }
}

/// A path with forward slashes, as command templates and Git Bash expect.
#[must_use]
pub fn slashed(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// The host label of a task's pane: `<project>/<slug>`.
#[must_use]
pub fn label(task: &Task) -> String {
    format!("{}/{}", task.project(), task.slug())
}
