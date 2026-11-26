//! `spawn`: create a task in `queued`, then start queued tasks, oldest first, while the start guard lets them.
//!
//! The brief holds the task only; the worker rules reach the session through the plugin's hooks. Only when no session
//! loads the plugin (neither `hooks.plugin_dir` nor the user's `settings.json` names it) are the rendered worker rules
//! appended to the brief instead.
//!
//! A spawn with `--allow-ask` commands appends the worker's first action, the `Allow` ask for them, to the brief. A
//! brief or allow-ask that names a user-only command under a kind that is not main-only is refused before the task
//! exists (`headwork_core::user_go`).

use std::path::Path;

use anyhow::{Context, Result, ensure};
use headwork_core::config::TaskVars;
use headwork_core::lifecycle::Trigger;
use headwork_core::user_go::{self, GoFacts};
use headwork_core::{PermissionMode, Role, TaskName, TaskRequest, TaskShape};
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
    /// User-only commands the user asked a worker to run: the worker's first action is the `Allow` ask for them.
    pub allow_ask: &'a [String],
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
/// Fails when the brief cannot be read or written, the user-only check or the admission guard refuses, or the store
/// fails.
pub fn spawn(
    store: &mut Store,
    env: &Env<'_>,
    new: &SpawnRequest<'_>,
) -> Result<(Task, Vec<Pumped>)> {
    let shape = if env.config.leads.enabled {
        TaskShape::Lead
    } else {
        TaskShape::Plain
    };
    spawn_shaped(store, env, new, shape)
}

/// Creates the task in the given shape, copies its brief, and runs the queue: [`spawn`] for a top-level task, a lead's
/// `part_spawn` for a part.
///
/// # Errors
///
/// As [`spawn`]; the store also refuses a part whose lead is not a live lead of the same project.
pub fn spawn_shaped(
    store: &mut Store,
    env: &Env<'_>,
    new: &SpawnRequest<'_>,
    shape: TaskShape<TaskId>,
) -> Result<(Task, Vec<Pumped>)> {
    let brief = std::fs::read_to_string(new.brief)
        .with_context(|| format!("reading the brief {}", new.brief.display()))?;
    let name = TaskName::new(new.request.project.clone(), new.request.slug.clone());
    let brief_path = env.home.brief_path(&name);
    let lead_allowed = match shape {
        TaskShape::Part { parent } => Some(store.user_only_allowed(parent)?),
        TaskShape::Lead | TaskShape::Plain => None,
    };
    let asks = match env.config.project(&new.request.project) {
        Some(project) => user_go::check_go(&GoFacts {
            project: &new.request.project,
            main_only: &project.main_only,
            user_only: &env.config.all_user_only(),
            kind: &new.request.kind,
            brief: &brief,
            allow_ask: new.allow_ask,
            lead_allowed: lead_allowed.as_deref(),
        })?,
        None => Vec::new(),
    };
    let brief = match user_go::first_action(&name, &asks) {
        section if section.is_empty() => brief,
        section => format!("{}\n\n{section}", brief.trim_end()),
    };
    keeps_brief(&brief_path, &brief)?;
    let mut request = new.request.clone();
    if request.effort.is_none() {
        let model = request
            .model
            .unwrap_or_else(|| env.config.models.for_kind(&request.kind));
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
            shape,
        },
        env.actor,
    )?;
    if !asks.is_empty() {
        let commands: Vec<&str> = asks.iter().map(user_go::AllowAsk::command).collect();
        store.append_event(
            env.actor,
            Some(task.id),
            None,
            "allow-ask",
            &serde_json::json!({ "commands": commands }),
        )?;
    }
    if let Some(parent) = brief_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let brief = if launch::plugin_loaded(env.home, env.config) {
        brief
    } else {
        let dir = env.task_dir(&task)?;
        let parent = shape
            .parent()
            .map(|lead| store.task(lead).map(|lead| lead.name.to_string()))
            .transpose()?;
        let rules = rules::current(store, env.home, env.config, env.host)?.render(
            Role::Worker,
            env.config,
            env.exe,
            Some(&Card {
                task: &task,
                dir: &dir,
                parent: parent.as_deref(),
            }),
        );
        format!("{}\n\n{rules}", brief.trim_end())
    };
    std::fs::write(&brief_path, brief)
        .with_context(|| format!("writing {}", brief_path.display()))?;
    let pumped = pump(store, env)?;
    Ok((store.task(task.id)?, pumped))
}

/// Refuses to replace a brief file that holds another task's text: the file at `path` may be absent, or start with the
/// new brief (an earlier spawn of the same text, with the worker rules appended).
fn keeps_brief(path: &Path, brief: &str) -> Result<()> {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    ensure!(
        existing.trim_end().starts_with(brief.trim_end()),
        "the brief file {} already exists with other content and spawn would overwrite it; spawn under a new slug, or \
         pass that file's own path as the brief to keep its text",
        path.display()
    );
    Ok(())
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
    let (workspace, fresh) = place::workspace_for(store, env, task, &main)?;
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
