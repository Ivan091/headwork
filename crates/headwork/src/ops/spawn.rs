//! `spawn`: create a task in `queued`, then start queued tasks, oldest first, while the start guard lets them.
//!
//! The brief holds the task only; the worker rules reach the session through the plugin's hooks. Without the plugin
//! configured, the rendered worker rules are appended to the brief instead.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use headwork_core::config::TaskVars;
use headwork_core::lifecycle::Trigger;
use headwork_core::{PermissionMode, Role, TaskName, TaskRequest};
use headwork_store::{NewSession, NewTask, Store, StoreError, Task, TaskId};
use serde::Serialize;

use super::place::{self, Opening};
use super::rules::{self, Card};
use super::{Env, launch, slashed};

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
    let mut request = new.request.clone();
    if request.effort.is_none() {
        let model = request.model.unwrap_or(env.config.models.default);
        let main = env
            .config
            .project(&request.project)
            .map(|project| env.home.expand(&project.main))
            .unwrap_or_default();
        request.effort = Some(super::effort::resolve(env.home, model, &main));
    }
    let task = store.create_task(
        env.config,
        &NewTask {
            request,
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
    let brief = if launch::plugin_dir(env.home, env.config).is_some() {
        brief
    } else {
        let dir = env.task_dir(&task)?;
        let rules = rules::current(store, env.home, env.config, env.host)?.render(
            Role::Worker,
            env.config,
            env.exe,
            Some(&Card {
                task: &task,
                dir: &dir,
            }),
        );
        format!("{}\n\n{rules}", brief.trim_end())
    };
    std::fs::write(&brief_path, brief)
        .with_context(|| format!("writing {}", brief_path.display()))?;
    let pumped = pump(store, env)?;
    Ok((store.task(task.id)?, pumped))
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

/// Makes the worktree when the task needs one, trusts the project's main checkout, records the session, and starts
/// `headwork launch` in a pane of the project's workspace. Returns the surface.
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
    super::trust::before_start(store, env, task)?;
    let (workspace, fresh) = env.host.ensure_workspace(task.project().as_str(), &main)?;
    let session_id = uuid::Uuid::new_v4().to_string();
    store.record_session(
        &NewSession {
            id: session_id.clone(),
            task_id: Some(task.id),
            role: Role::Worker,
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
    place::open(
        store,
        env,
        &Opening {
            task,
            session: &session_id,
            workspace: &workspace,
            fresh,
            dir: &dir,
            argv: &argv,
        },
    )
}
