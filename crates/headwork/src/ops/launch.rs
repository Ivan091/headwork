//! `launch`: the process a pane runs (design section 3.1).
//!
//! It gives the session its identity through the environment, starts `claude` with the task's fixed session id,
//! name, model and permission mode inside a tree it owns, records both process ids, waits, and records the exit.
//! Until the `SessionStart` hook arrives in milestone 3, the launcher itself reports the session as started once
//! `claude` runs.

use std::process::Command;

use anyhow::{Context, Result};
use headwork_adapters::proc::{self, OwnedChild, Ownership};
use headwork_adapters::runtime::{ClaudeLaunch, ENV_HOME, ENV_ROLE, ENV_SESSION, ENV_TASK};
use headwork_core::lifecycle::{SessionFacts, Trigger};
use headwork_core::{Actor, Config, Role, TaskState};
use headwork_store::{SessionRow, Store, Task, TaskId};

use crate::home::Home;

/// The first prompt of a new child session.
#[must_use]
pub fn first_prompt(task: &Task) -> String {
    format!(
        "You are the child session {}. Read your brief at {} and carry it out.",
        task.name, task.brief_path
    )
}

/// The `claude` command for a task's session, with its identity in the environment.
///
/// # Errors
///
/// Fails when the project left the config or the task's folder is unknown.
pub fn claude_command(
    home: &Home,
    config: &Config,
    task: &Task,
    session: &SessionRow,
    dir: &std::path::Path,
) -> Result<Command> {
    let project = config
        .project(task.project())
        .with_context(|| format!("project `{}` is not configured", task.project()))?;
    let name = task.name.to_string();
    let prompt = first_prompt(task);
    let argv = ClaudeLaunch {
        session_id: &session.id,
        resume: false,
        name: &name,
        remote_control: project
            .remote_control
            .unwrap_or(config.machine.remote_control),
        model: task.model,
        permission_mode: task.permission_mode,
        agent: None,
        prompt: &prompt,
    }
    .argv();
    let (program, rest) = argv.split_first().context("an empty argv")?;
    let mut command = Command::new(program);
    command
        .args(rest)
        .current_dir(dir)
        .env(ENV_SESSION, &session.id)
        .env(ENV_ROLE, Role::Child.as_str())
        .env(ENV_TASK, task.id.to_string())
        .env(ENV_HOME, home.root());
    Ok(command)
}

/// Runs a task's session to its end and returns the exit code of `claude`.
///
/// # Errors
///
/// Fails when the store or config cannot be read; a `claude` that does not start moves the task to `failed` first.
pub fn launch(home: &Home, id: TaskId) -> Result<Option<i32>> {
    let config = home.load_config()?;
    let mut store = home.open_store()?;
    let task = store.task(id)?;
    let session = store.session_of(id)?.with_context(|| {
        format!(
            "task {} has no session; start it with `headwork spawn`",
            task.name
        )
    })?;
    std::env::set_current_dir(home.root())
        .with_context(|| format!("entering {}", home.root().display()))?;
    proc::ignore_interrupts()?;
    let started = start(home, &config, &mut store, &task, &session);
    let child = match started {
        Ok(child) => child,
        Err(error) => {
            let reason = format!("{error:#}");
            store.append_event(
                Actor::Child,
                Some(id),
                Some(&session.id),
                "launch-error",
                &serde_json::json!({ "error": reason }),
            )?;
            if task.state == TaskState::Spawning {
                store.transition(id, &Trigger::LaunchFailed, Actor::Child)?;
            }
            return Err(error);
        }
    };
    drop(store);
    let status = child.wait();
    let mut store = home.open_store()?;
    let code = status
        .as_ref()
        .ok()
        .and_then(std::process::ExitStatus::code);
    store.session_ended(&session.id, code)?;
    after_exit(&mut store, id)?;
    status?;
    Ok(code)
}

fn start(
    home: &Home,
    config: &Config,
    store: &mut Store,
    task: &Task,
    session: &SessionRow,
) -> Result<OwnedChild> {
    let dir = if task.in_main {
        let project = config
            .project(task.project())
            .with_context(|| format!("project `{}` is not configured", task.project()))?;
        home.expand(&project.main)
    } else {
        let worktree = store
            .worktree_of(task.id)?
            .with_context(|| format!("task {} has no worktree", task.name))?;
        std::path::PathBuf::from(worktree.path)
    };
    let mut command = claude_command(home, config, task, session, &dir)?;
    let child = OwnedChild::spawn(&mut command)?;
    let tree = match child.ownership() {
        Ownership::Job => "job".to_owned(),
        Ownership::ProcessGroup => "process-group".to_owned(),
        Ownership::Unowned(reason) => format!("unowned: {reason}"),
    };
    store.session_launched(&session.id, std::process::id(), child.pid(), &tree)?;
    let facts = SessionFacts {
        session_matches: true,
        launcher_alive: true,
    };
    if let Err(error) = store.transition(task.id, &Trigger::SessionStarted(facts), Actor::Child) {
        store.append_event(
            Actor::Child,
            Some(task.id),
            Some(&session.id),
            "note",
            &serde_json::json!({ "start": error.to_string() }),
        )?;
    }
    Ok(child)
}

/// Moves a task whose `claude` exited on its own: a session that never got going fails, a running one is
/// interrupted. Closing and closed tasks are left to `close`.
fn after_exit(store: &mut Store, id: TaskId) -> Result<()> {
    let task = store.task(id)?;
    let trigger = match task.state {
        TaskState::Spawning => Trigger::LaunchFailed,
        TaskState::Working | TaskState::NeedsUser | TaskState::InReview | TaskState::Merged => {
            Trigger::ProcessLost {
                machine: store.machine_mode()?,
            }
        }
        TaskState::Queued
        | TaskState::Closing
        | TaskState::Closed
        | TaskState::Dropped
        | TaskState::Failed
        | TaskState::Interrupted
        | TaskState::Parking
        | TaskState::Parked => return Ok(()),
    };
    match store.transition(id, &trigger, Actor::Child) {
        Ok(_) | Err(headwork_store::StoreError::Refused { .. }) => Ok(()),
        Err(error) => Err(error.into()),
    }
}
