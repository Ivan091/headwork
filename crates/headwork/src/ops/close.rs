//! `close`: end a task's session, pane, process tree and worktree, and mark it `closed` only once each is verified
//! gone.
//!
//! The order is the one today's `close-child.sh` learned: everything else first, the pane (or the workspace it leaves
//! empty) next, ending the program last, because a child usually closes itself from inside the pane being closed.
//! Under headwork a self-close ends `claude`'s tree, which takes the calling command with it; the launcher sees
//! `claude` exit with the task `closing`, starts a detached `headwork finish` that outlives the pane, and closes its
//! own pane as its last act. `finish` waits for the launcher to be gone, removes the worktree, verifies, and marks the
//! task `closed`.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use headwork_adapters::host::{SurfaceId, TerminalHost, WorkspaceId};
use headwork_adapters::proc;
use headwork_core::config::TaskVars;
use headwork_core::lifecycle::{CleanupFacts, Trigger, WorktreeEnd};
use headwork_core::{Actor, TaskState};
use headwork_store::{SessionRow, Store, StoreError, TaskId};
use serde::Serialize;

use super::{Env, slashed};

/// How long `close` waits for a launcher to finish after its `claude` was ended.
const LAUNCHER_GRACE: Duration = Duration::from_secs(60);

/// How long the last checks wait for processes to vanish.
const PROCESS_GRACE: Duration = Duration::from_secs(10);

/// Where a close ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloseReport {
    /// `<project>-<slug>`.
    pub name: String,
    /// The task's state afterwards.
    pub state: TaskState,
    /// Why it is not `closed` yet, when it is not.
    pub pending: Option<String>,
}

/// Closes a task. `own_session` is the session the caller runs in, from `HEADWORK_SESSION`: closing that session
/// ends the caller, so the launcher finishes the close.
///
/// # Errors
///
/// Fails when the store fails or the task cannot start closing.
pub fn close(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    own_session: Option<&str>,
) -> Result<CloseReport> {
    begin_closing(store, env, id)?;
    let task = store.task(id)?;
    remove_brief(env, &task.brief_path);
    if let Some(session) = store.session_of(id)?
        && let Some(launcher) = session
            .launcher_pid
            .filter(|pid| proc::is_alive(*pid).unwrap_or(false))
    {
        let target = session.claude_pid.unwrap_or(launcher);
        if own_session == Some(session.id.as_str()) {
            store.append_event(
                env.actor,
                Some(id),
                Some(&session.id),
                "self-close",
                &serde_json::json!({}),
            )?;
            proc::kill_tree(target)?;
            return report(
                store,
                id,
                Some("the launcher finishes the close".to_owned()),
            );
        }
        proc::kill_tree(target)?;
        wait_until(LAUNCHER_GRACE, || {
            !proc::is_alive(launcher).unwrap_or(false)
        });
    }
    finish(store, env, id)
}

fn begin_closing(store: &mut Store, env: &Env<'_>, id: TaskId) -> Result<()> {
    let task = store.task(id)?;
    let trigger = match task.state {
        TaskState::Closing | TaskState::Closed => return Ok(()),
        TaskState::Dropped => Trigger::BeginClosing,
        TaskState::Queued
        | TaskState::Spawning
        | TaskState::Working
        | TaskState::NeedsUser
        | TaskState::InReview
        | TaskState::Merged
        | TaskState::Failed
        | TaskState::Interrupted
        | TaskState::Parking
        | TaskState::Parked => Trigger::ForceClose,
    };
    store.transition(id, &trigger, env.actor)?;
    Ok(())
}

fn remove_brief(env: &Env<'_>, brief: &str) {
    let path = Path::new(brief);
    if path.starts_with(env.home.root()) || slashed(path).starts_with(&slashed(env.home.root())) {
        drop(std::fs::remove_file(path));
    }
}

/// Finishes a `closing` task from outside its pane: closes what is left of the pane, ends leftover processes, removes
/// the worktree, verifies, and marks it `closed`. Safe to run again.
///
/// # Errors
///
/// Fails when the store fails or the task is not closing.
pub fn finish(store: &mut Store, env: &Env<'_>, id: TaskId) -> Result<CloseReport> {
    let task = store.task(id)?;
    match task.state {
        TaskState::Closed => return report(store, id, None),
        TaskState::Closing => {}
        state @ (TaskState::Queued
        | TaskState::Spawning
        | TaskState::Working
        | TaskState::NeedsUser
        | TaskState::InReview
        | TaskState::Merged
        | TaskState::Dropped
        | TaskState::Failed
        | TaskState::Interrupted
        | TaskState::Parking
        | TaskState::Parked) => bail!("task {} is {state}, not closing", task.name),
    }
    let session = store.session_of(id)?;
    let mut pane_gone = true;
    if let Some(session) = &session {
        pane_gone = close_pane(env.host, session)?;
        for pid in [session.claude_pid, session.launcher_pid]
            .into_iter()
            .flatten()
        {
            if proc::is_alive(pid)? {
                proc::kill_tree(pid)?;
            }
        }
    }
    let pids: Vec<u32> = session
        .iter()
        .flat_map(|s| [s.claude_pid, s.launcher_pid])
        .flatten()
        .collect();
    let processes_gone = wait_until(PROCESS_GRACE, || {
        pids.iter().all(|pid| !proc::is_alive(*pid).unwrap_or(true))
    });
    let worktree = if processes_gone {
        remove_worktree(store, env, id, false)?
    } else {
        WorktreeEnd::Present
    };
    let facts = CleanupFacts {
        pane_gone,
        processes_gone,
        worktree,
    };
    match store.transition(id, &Trigger::Cleaned(facts), env.actor) {
        Ok(_) => report(store, id, None),
        Err(StoreError::Refused { refusal, .. }) => report(store, id, Some(refusal.to_string())),
        Err(error) => Err(error.into()),
    }
}

/// Closes the session's pane, or its workspace when the pane is the last thing in it, then ends the host's program.
/// Returns whether the pane is gone.
fn close_pane(host: &dyn TerminalHost, session: &SessionRow) -> Result<bool> {
    let Some(surface) = session
        .host_surface
        .as_deref()
        .map(|s| SurfaceId(s.to_owned()))
    else {
        return Ok(true);
    };
    let workspace = session
        .host_workspace
        .as_deref()
        .map(|w| WorkspaceId(w.to_owned()));
    if surface_present(host, workspace.as_ref(), &surface)? {
        close_surface_or_workspace(host, workspace.as_ref(), &surface)?;
    }
    for pane in host.panes()? {
        if pane.surface == surface && pane.running {
            host.kill_agent(&pane.agent)?;
        }
    }
    Ok(!surface_present(host, workspace.as_ref(), &surface)?)
}

pub(super) fn surface_present(
    host: &dyn TerminalHost,
    workspace: Option<&WorkspaceId>,
    surface: &SurfaceId,
) -> Result<bool> {
    let Some(workspace) = workspace else {
        return Ok(host
            .panes()?
            .iter()
            .any(|pane| &pane.surface == surface && pane.running));
    };
    let exists = host.workspaces()?.iter().any(|w| &w.id == workspace);
    Ok(exists && host.surfaces(workspace)?.contains(surface))
}

/// Closes the workspace when `surface` is all it holds, else just the surface.
///
/// # Errors
///
/// Returns the host's failure.
pub fn close_surface_or_workspace(
    host: &dyn TerminalHost,
    workspace: Option<&WorkspaceId>,
    surface: &SurfaceId,
) -> Result<()> {
    if let Some(workspace) = workspace {
        let surfaces = host.surfaces(workspace)?;
        if surfaces.iter().all(|s| s == surface) {
            host.close_workspace(workspace)?;
            return Ok(());
        }
    }
    host.close_surface(surface)?;
    Ok(())
}

pub(super) fn remove_worktree(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    force: bool,
) -> Result<WorktreeEnd> {
    let task = store.task(id)?;
    let Some(worktree) = store.worktree_of(id)? else {
        return Ok(WorktreeEnd::NoWorktree);
    };
    match worktree.state.as_str() {
        "removed" => return Ok(WorktreeEnd::Removed),
        "kept" => return Ok(WorktreeEnd::Kept),
        _ => {}
    }
    if !Path::new(&worktree.path).exists() {
        store.set_worktree_state(id, "removed", env.actor)?;
        return Ok(WorktreeEnd::Removed);
    }
    let main = slashed(&env.main_path(&task)?);
    let vars = TaskVars {
        slug: task.slug().as_str(),
        branch: &worktree.branch,
        main: &main,
        worktree: &worktree.path,
    };
    let command = &env.project(&task)?.worktree_remove;
    let worktrees = env.worktrees(&task)?;
    let result = if force {
        worktrees.remove_forced(command, &vars)
    } else {
        worktrees.remove(command, &vars)
    };
    match result {
        Ok(()) if !Path::new(&worktree.path).exists() => {
            store.set_worktree_state(id, "removed", env.actor)?;
            Ok(WorktreeEnd::Removed)
        }
        Ok(()) => {
            let error = "the remove command left the folder in place".to_owned();
            store.append_event(
                env.actor,
                Some(id),
                None,
                "worktree-refused",
                &serde_json::json!({ "error": error }),
            )?;
            Ok(WorktreeEnd::Present)
        }
        Err(error) => {
            let error = error.to_string();
            store.append_event(
                env.actor,
                Some(id),
                None,
                "worktree-refused",
                &serde_json::json!({ "error": error }),
            )?;
            Ok(WorktreeEnd::Present)
        }
    }
}

/// Finishes every `closing` task whose launcher is gone, such as one that closed itself.
///
/// # Errors
///
/// Fails when the store fails.
pub fn reap(store: &mut Store, env: &Env<'_>) -> Result<Vec<CloseReport>> {
    let mut out = Vec::new();
    for id in store.tasks_in(TaskState::Closing)? {
        let launcher_alive = store
            .session_of(id)?
            .and_then(|s| s.launcher_pid)
            .is_some_and(|pid| proc::is_alive(pid).unwrap_or(true));
        if !launcher_alive {
            out.push(finish(store, env, id)?);
        }
    }
    Ok(out)
}

/// The launcher's part of a close, once `claude` has exited and the task is `closing`.
///
/// It starts a detached `headwork finish` that waits for this launcher to be gone, then closes this pane, or the
/// workspace it leaves empty, as the very last act.
///
/// # Errors
///
/// Fails when the finisher cannot start; the pane is closed anyway.
pub fn hand_off(store: &mut Store, env: &Env<'_>, id: TaskId, session: &SessionRow) -> Result<()> {
    let mut finisher = Command::new(env.exe);
    finisher
        .args([
            "finish",
            "--task",
            &id.to_string(),
            "--wait-pid",
            &std::process::id().to_string(),
            "--home",
        ])
        .arg(env.home.root())
        .current_dir(env.home.root());
    let started = proc::spawn_detached(&mut finisher);
    let data = match &started {
        Ok((pid, note)) => serde_json::json!({ "finisher": pid, "note": note }),
        Err(error) => serde_json::json!({ "error": error.to_string() }),
    };
    store.append_event(Actor::Child, Some(id), Some(&session.id), "hand-off", &data)?;
    if let Some(surface) = session.host_surface.as_deref() {
        let workspace = session
            .host_workspace
            .as_deref()
            .map(|w| WorkspaceId(w.to_owned()));
        close_surface_or_workspace(env.host, workspace.as_ref(), &SurfaceId(surface.to_owned()))?;
    }
    started?;
    Ok(())
}

/// The detached finisher: waits for `pid` to be gone, then finishes the close and runs the queue.
///
/// # Errors
///
/// Fails when the store fails.
pub fn finish_after(store: &mut Store, env: &Env<'_>, id: TaskId, pid: u32) -> Result<CloseReport> {
    wait_until(LAUNCHER_GRACE, || !proc::is_alive(pid).unwrap_or(false));
    let report = finish(store, env, id)?;
    super::spawn::pump(store, env)?;
    Ok(report)
}

fn report(store: &Store, id: TaskId, pending: Option<String>) -> Result<CloseReport> {
    let task = store.task(id)?;
    Ok(CloseReport {
        name: task.name.to_string(),
        state: task.state,
        pending,
    })
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if done() {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
