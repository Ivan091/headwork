//! `spawn`: create a task in `queued`, then start queued tasks, oldest first, while the start guard lets them.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use headwork_core::config::TaskVars;
use headwork_core::lifecycle::Trigger;
use headwork_core::{PermissionMode, Role, TaskName, TaskRequest};
use headwork_store::{NewSession, NewTask, Store, StoreError, Task, TaskId};
use serde::Serialize;

use super::{Env, label, slashed};

/// A task to create.
#[derive(Debug, Clone)]
pub struct SpawnRequest<'a> {
    /// What the task is.
    pub request: TaskRequest,
    /// The brief to copy into the home folder.
    pub brief: &'a Path,
    /// The task it waits for.
    pub depends_on: Option<TaskId>,
    /// The permission mode its sessions start in.
    pub permission_mode: PermissionMode,
}

/// What happened to one queued task when the queue was run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Outcome {
    /// Its pane runs.
    Started {
        /// The host surface.
        surface: String,
    },
    /// The start guard refused; it stays queued.
    Waiting {
        /// Why.
        reason: String,
    },
    /// The pane could not be opened; the task is `failed`.
    Failed {
        /// Why.
        error: String,
    },
}

/// One queued task and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pumped {
    /// `<project>-<slug>`.
    pub name: String,
    /// What happened.
    pub outcome: Outcome,
}

/// Creates the task, copies its brief, and runs the queue.
///
/// # Errors
///
/// Fails when the brief cannot be read or written, the admission guard refuses, or the store fails.
pub fn spawn(
    store: &mut Store,
    env: &Env<'_>,
    new: &SpawnRequest<'_>,
) -> Result<(Task, Vec<Pumped>)> {
    let brief = std::fs::read_to_string(new.brief)
        .with_context(|| format!("reading the brief {}", new.brief.display()))?;
    let name = TaskName::new(new.request.project.clone(), new.request.slug.clone());
    let brief_path = env.home.brief_path(&name);
    let task = store.create_task(
        env.config,
        &NewTask {
            request: new.request.clone(),
            brief_path: slashed(&brief_path),
            depends_on: new.depends_on,
            permission_mode: new.permission_mode,
        },
        env.actor,
    )?;
    if let Some(parent) = brief_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let brief = if new.request.keep {
        brief
    } else {
        format!("{}{}", brief.trim_end(), closing_section(env, &name))
    };
    std::fs::write(&brief_path, brief)
        .with_context(|| format!("writing {}", brief_path.display()))?;
    let pumped = pump(store, env)?;
    Ok((store.task(task.id)?, pumped))
}

/// The paragraph every brief but a keep task's ends with: how the child closes itself.
#[must_use]
pub fn closing_section(env: &Env<'_>, name: &TaskName) -> String {
    format!(
        "\n\n## Closing yourself\n\nWhen your task is completely finished (its pull requests merged or the task dropped by the \
         user, your report sent to\n`root`, no question or review waiting on the user, and nothing of yours still running \
         in the background), close your\nown session as your very last action: `\"{}\" close {name}`. It ends this pane, \
         so run nothing after it.\nWhile anything is still pending, stay open.\n",
        slashed(env.exe)
    )
}

/// Starts every queued task the start guard lets through, in queue order, and opens its pane. A task whose pane
/// cannot be opened becomes `failed`; the rest of the queue still runs.
///
/// # Errors
///
/// Fails only when the store fails.
pub fn pump(store: &mut Store, env: &Env<'_>) -> Result<Vec<Pumped>> {
    let mut out = Vec::new();
    for id in store.queued()? {
        let task = store.task(id)?;
        let name = task.name.to_string();
        let outcome = match store.start_task(env.config, id, env.actor) {
            Ok(_) => match open_pane(store, env, &task) {
                Ok(surface) => Outcome::Started { surface },
                Err(error) => {
                    let error = format!("{error:#}");
                    store.append_event(
                        env.actor,
                        Some(id),
                        None,
                        "launch-error",
                        &serde_json::json!({ "error": error }),
                    )?;
                    store.transition(id, &Trigger::LaunchFailed, env.actor)?;
                    Outcome::Failed { error }
                }
            },
            Err(StoreError::Refused { refusal, .. }) => Outcome::Waiting {
                reason: refusal.to_string(),
            },
            Err(error) => return Err(error.into()),
        };
        out.push(Pumped { name, outcome });
    }
    Ok(out)
}

/// Makes the worktree when the task needs one, records the session, and starts `headwork launch` in a pane of the
/// project's workspace. Returns the surface.
fn open_pane(store: &mut Store, env: &Env<'_>, task: &Task) -> Result<String> {
    let project = env.project(task)?;
    let main = env.main_path(task)?;
    let dir = env.task_dir(task)?;
    let slug = task.slug().as_str();
    if !task.in_main {
        if !dir.exists() {
            let (main_text, dir_text) = (slashed(&main), slashed(&dir));
            let vars = TaskVars {
                slug,
                branch: slug,
                main: &main_text,
                worktree: &dir_text,
            };
            env.worktrees(task)?.create(&project.worktree_new, &vars)?;
            ensure!(
                dir.is_dir(),
                "the worktree command did not create {}",
                dir.display()
            );
        }
        store.record_worktree(task.id, &slashed(&dir), slug, env.actor)?;
    }
    let (workspace, fresh) = env.host.ensure_workspace(task.project().as_str(), &main)?;
    let session_id = uuid::Uuid::new_v4().to_string();
    store.record_session(
        &NewSession {
            id: session_id.clone(),
            task_id: Some(task.id),
            role: Role::Child,
            rc_name: task.name.to_string(),
            host: env.config.machine.host.as_str().to_owned(),
            host_workspace: Some(workspace.to_string()),
            host_surface: None,
        },
        env.actor,
    )?;
    let argv = vec![
        env.exe.display().to_string(),
        "launch".to_owned(),
        "--task".to_owned(),
        task.id.to_string(),
        "--home".to_owned(),
        env.home.root().display().to_string(),
    ];
    let pane = env
        .host
        .spawn(&workspace, &dir, &label(task), &argv, fresh)?;
    store.set_session_place(
        &session_id,
        workspace.as_str(),
        pane.surface.as_str(),
        env.actor,
    )?;
    Ok(pane.surface.to_string())
}
