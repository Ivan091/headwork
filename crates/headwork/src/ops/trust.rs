//! Folder trust for the main checkouts of configured projects.
//!
//! Each one is trusted in Claude Code's state file before a session starts there, so a worker never stops at "Do you
//! trust this folder?", which only the user may answer.
//!
//! Only the `main` folders of projects in `config.toml` are trusted, each under its exact key, and only the trust
//! field is written (`headwork_adapters::trust`). A task worktree inherits its main repository's trust. A failure
//! to write is logged and never stops a spawn; the tick still raises a trust prompt that shows on a pane as an ask.

use anyhow::Result;
use headwork_adapters::trust::{self, Trust, TrustError};
use headwork_core::{Actor, Config, ProjectName};
use headwork_store::{Store, Task, TaskId};

use super::{Env, slashed};
use crate::home::Home;

/// What became of one project's main checkout.
#[derive(Debug)]
pub struct ProjectTrust {
    /// The project.
    pub project: String,
    /// The key in Claude Code's state file.
    pub key: String,
    /// Whether it was trusted already or is now, or why that failed.
    pub outcome: Result<Trust, TrustError>,
}

/// Trusts the main checkout of one configured project.
#[must_use]
pub fn ensure_project(home: &Home, config: &Config, project: &ProjectName) -> Option<ProjectTrust> {
    let main = config.project(project)?.main.as_str();
    let key = trust::project_key(&slashed(&home.expand(main)));
    let outcome = trust::ensure_trusted(home.claude_json(), &key);
    Some(ProjectTrust {
        project: project.to_string(),
        key,
        outcome,
    })
}

/// Trusts the main checkout of every configured project, in name order.
#[must_use]
pub fn ensure_all(home: &Home, config: &Config) -> Vec<ProjectTrust> {
    config
        .projects
        .keys()
        .filter_map(|project| ensure_project(home, config, project))
        .collect()
}

/// Trusts the main checkout of a task's project before its session starts, and logs `trust-granted` or
/// `trust-error` on the task.
///
/// # Errors
///
/// Fails only when the store fails.
pub fn before_start(store: &Store, env: &Env<'_>, task: &Task) -> Result<()> {
    let Some(found) = ensure_project(env.home, env.config, task.project()) else {
        return Ok(());
    };
    log(store, env.home, env.actor, Some(task.id), &found)
}

/// Logs what [`ensure_project`] did, when it did anything.
///
/// # Errors
///
/// Fails only when the store fails.
pub fn log(
    store: &Store,
    home: &Home,
    actor: Actor,
    task: Option<TaskId>,
    found: &ProjectTrust,
) -> Result<()> {
    let file = slashed(home.claude_json());
    let (kind, data) = match &found.outcome {
        Ok(Trust::Already) => return Ok(()),
        Ok(Trust::Granted) => (
            "trust-granted",
            serde_json::json!({ "project": found.project, "path": found.key, "file": file }),
        ),
        Err(error) => (
            "trust-error",
            serde_json::json!({ "project": found.project, "path": found.key, "error": error.to_string() }),
        ),
    };
    store.append_event(actor, task, None, kind, &data)?;
    Ok(())
}
