//! `close`: end a task's session, pane, process tree and worktree, and mark it `closed` only once each is verified
//! gone.
//!
//! The order is the one today's `close-child.sh` learned: everything else first, the pane (or the workspace it leaves
//! empty) next, ending the program last, because a worker usually closes itself from inside the pane being closed.
//! Under headwork a self-close ends `claude`'s tree, which takes the calling command with it; the launcher sees
//! `claude` exit with the task `closing`, starts a detached `headwork finish` that outlives the pane, and closes its
//! own pane as its last act. `finish` waits for the launcher to be gone, removes the worktree, verifies, and marks the
//! task `closed`.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use headwork_adapters::git::{GitHost, PrInfo, PrState};
use headwork_adapters::host::{SurfaceId, TerminalHost, WorkspaceId};
use headwork_adapters::proc;
use headwork_adapters::worktree::BranchEnd;
use headwork_core::config::{TaskVars, WorktreeCommand};
use headwork_core::guard::DEFAULT_BRANCHES;
use headwork_core::lifecycle::{
    CleanupFacts, CloseCause, CloseFacts, FinishFacts, MergeSource, PrEnd, Trigger, WorktreeEnd,
};
use headwork_core::{Actor, JobState, SessionState, TaskState};
use headwork_store::{PrSeen, SessionRow, Store, StoreError, Task, TaskId};
use serde::Serialize;

use super::hook::{Git, GitProbe};
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

/// Force-closes a task, as the user or the orchestrator.
///
/// Every state that is not over goes to `closing` at once. `own_session` is the session the caller runs in, from
/// `HEADWORK_SESSION`: closing that session ends the caller, so the launcher finishes the close. A worker never
/// force-closes; it finishes its own task with [`close_own`].
///
/// # Errors
///
/// Fails when the store fails, the task cannot start closing, or the actor is a worker.
pub fn close(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    own_session: Option<&str>,
) -> Result<CloseReport> {
    if env.actor == Actor::Worker {
        bail!(
            "a worker does not force-close a task: finish your own task with `done` or `headwork close <your name>`, \
             and ask the orchestrator to close anything else"
        );
    }
    let before = store.task(id)?;
    if !matches!(before.state, TaskState::Closing | TaskState::Closed) {
        super::recap::note(
            store,
            env,
            &before,
            headwork_core::RecapKind::Close,
            "closing",
        );
    }
    begin_closing(store, env, id)?;
    end_closing(store, env, id, own_session)
}

/// Closes a worker's own task through the checked path.
///
/// A merged pull request moves it through `in_review` and
/// `merged` to `closing`, a task without one (or whose pull request was closed unmerged) finishes from `working`, and
/// a dropped task begins closing. Every step is checked before any is applied: an open pull request, an ask waiting
/// on the user, a running job or a missing report to the orchestrator refuses the close and leaves the task as it was.
///
/// # Errors
///
/// Fails with the refusal when the task may not close yet, or when the store or the git host fails.
pub fn close_own(
    store: &mut Store,
    env: &Env<'_>,
    git: &dyn GitHost,
    id: TaskId,
    own_session: Option<&str>,
) -> Result<CloseReport> {
    begin_own_close(store, env, git, id)?;
    end_closing(store, env, id, own_session)
}

/// The checked half of [`close_own`]: moves the worker's own task to `closing`, or refuses and leaves it as it was.
/// A task already closing or closed is left alone.
///
/// # Errors
///
/// Fails with the refusal when the task may not close yet, or when the store or the git host fails.
pub fn begin_own_close(
    store: &mut Store,
    env: &Env<'_>,
    git: &dyn GitHost,
    id: TaskId,
) -> Result<()> {
    let before = store.task(id)?;
    if matches!(before.state, TaskState::Closing | TaskState::Closed) {
        return Ok(());
    }
    let steps = match own_steps(store, env, git, &before) {
        Ok(steps) => steps,
        Err(error) => {
            store.append_event(
                env.actor,
                Some(id),
                None,
                "close-refused",
                &serde_json::json!({ "state": before.state, "reason": format!("{error:#}") }),
            )?;
            return Err(error);
        }
    };
    for trigger in &steps {
        store.transition(id, trigger, env.actor)?;
    }
    super::recap::note(
        store,
        env,
        &before,
        headwork_core::RecapKind::Close,
        "closing",
    );
    Ok(())
}

/// The transitions that take a worker's own task to `closing`, all checked against the lifecycle before any is
/// applied.
fn own_steps(
    store: &mut Store,
    env: &Env<'_>,
    git: &dyn GitHost,
    task: &Task,
) -> Result<Vec<Trigger>> {
    let id = task.id;
    if task.state == TaskState::Dropped {
        return Ok(vec![Trigger::BeginClosing]);
    }
    if task.state == TaskState::NeedsUser {
        bail!("{}", headwork_core::Refusal::OpenAsk);
    }
    let (pr, info) = if matches!(task.state, TaskState::Working | TaskState::InReview) {
        pr_end(store, env, git, task)?
    } else {
        (PrEnd::None, None)
    };
    let facts = FinishFacts {
        open_ask: store.has_waiting_ask(id)?,
        running_job: super::jobs::has_running_job(store, id)?,
        report_sent: store.report_sent(id)?,
        pr,
    };
    let close = Trigger::Close(CloseFacts {
        cause: CloseCause::Done,
        open_ask: facts.open_ask,
        running_job: facts.running_job,
        report_sent: facts.report_sent,
    });
    let project = env.project(task)?;
    let merged = Trigger::Merged {
        source: MergeSource::GitHost,
        policy: project.merge,
    };
    let steps = match (task.state, pr) {
        (TaskState::Merged, _) => vec![close],
        (TaskState::Working, PrEnd::Merged) => {
            let repo_allowed = info.as_ref().is_some_and(|info| {
                project
                    .allowed_repos
                    .iter()
                    .any(|repo| repo.as_str().eq_ignore_ascii_case(&info.repo))
            });
            vec![Trigger::ReviewOpened { repo_allowed }, merged, close]
        }
        (TaskState::InReview, PrEnd::Merged) => vec![merged, close],
        (TaskState::InReview, PrEnd::Closed | PrEnd::None) => vec![
            Trigger::ChangesRequested,
            Trigger::Finish(FinishFacts {
                pr: PrEnd::Closed,
                ..facts
            }),
        ],
        (TaskState::InReview, PrEnd::Open) => bail!("{}", headwork_core::Refusal::PrOpen),
        _ => vec![Trigger::Finish(facts)],
    };
    let mut snapshot = task.snapshot();
    for trigger in &steps {
        let step =
            headwork_core::apply(&snapshot, trigger).map_err(|refusal| anyhow!("{refusal}"))?;
        snapshot.state = step.to;
        snapshot.state_before_park = step.state_before_park;
    }
    Ok(steps)
}

/// Where the task's pull requests stand, asked of the git host now: those recorded for the task, the one of its
/// worktree's branch and the one of the branch checked out in its folder. Any open one makes it [`PrEnd::Open`], else
/// any merged one [`PrEnd::Merged`], else any closed one [`PrEnd::Closed`]. A task in the main checkout has none.
fn pr_end(
    store: &mut Store,
    env: &Env<'_>,
    git: &dyn GitHost,
    task: &Task,
) -> Result<(PrEnd, Option<PrInfo>)> {
    if task.in_main {
        return Ok((PrEnd::None, None));
    }
    let mut seen: Vec<PrInfo> = Vec::new();
    for url in store.pr_urls_of(task.id)? {
        if let Some(pr) = git.pr_by_url(&url)? {
            seen.push(pr);
        }
    }
    let dir = env.task_dir(task)?;
    let dir = if dir.is_dir() {
        dir
    } else {
        env.main_path(task)?
    };
    let mut branches = vec![
        store
            .worktree_of(task.id)?
            .map_or_else(|| task.slug().to_string(), |w| w.branch),
    ];
    if let Some(current) = Git.branch(&slashed(&dir))
        && !branches.contains(&current)
        && !DEFAULT_BRANCHES.contains(&current.as_str())
    {
        branches.push(current);
    }
    for branch in &branches {
        if let Some(pr) = git.pr_for_branch(&dir, branch)?
            && !seen.iter().any(|known| known.url == pr.url)
        {
            seen.push(pr);
        }
    }
    for pr in &seen {
        store.record_pr(
            task.id,
            &PrSeen {
                repo: &pr.repo,
                number: pr.number,
                url: &pr.url,
                state: pr.state.as_str(),
            },
            env.actor,
        )?;
    }
    let pick = [
        (PrState::Open, PrEnd::Open),
        (PrState::Merged, PrEnd::Merged),
        (PrState::Closed, PrEnd::Closed),
    ];
    Ok(pick
        .into_iter()
        .find_map(|(state, end)| {
            seen.iter()
                .find(|pr| pr.state == state)
                .map(|pr| (end, Some(pr.clone())))
        })
        .unwrap_or((PrEnd::None, None)))
}

/// Ends a `closing` task's session, pane, process tree and worktree, and marks it `closed` once each is verified gone.
///
/// # Errors
///
/// Fails when the store fails or the task is not closing.
pub fn end_closing(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    own_session: Option<&str>,
) -> Result<CloseReport> {
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
            if pinned(store, &session.id)? {
                let _logged: Result<()> =
                    start_finisher(store, env, id, &session.id, launcher, "backup-finisher");
            }
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
        pane_gone = close_pane(env, session)?;
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
    finalize_jobs(store, env, id)?;
    let worktree = if processes_gone {
        store.end_task_sessions(id, env.actor)?;
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

/// Ends the task's open jobs: a closed task owns nothing, so a job still running is stopped and every open job is
/// recorded `done`.
fn finalize_jobs(store: &mut Store, env: &Env<'_>, id: TaskId) -> Result<()> {
    for job in store.open_jobs(Some(id))? {
        if let Some(pid) = job.pid
            && proc::is_alive(pid)?
        {
            proc::kill_tree(pid)?;
        }
        store.set_job_state(job.id, JobState::Done, Some("the task closed"), env.actor)?;
    }
    Ok(())
}

/// Closes the session's pane, or its workspace when the pane is the last thing in it, then ends the host's program.
/// Returns whether the pane is gone.
pub(super) fn close_pane(env: &Env<'_>, session: &SessionRow) -> Result<bool> {
    let host = env.host;
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
        close_surface_or_workspace(env, workspace.as_ref(), &surface)?;
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

/// Closes a session's surface, or its workspace when nothing else there holds a session; a pane left empty goes with
/// it (see [`super::place::release`]).
///
/// # Errors
///
/// Returns the host's failure.
pub fn close_surface_or_workspace(
    env: &Env<'_>,
    workspace: Option<&WorkspaceId>,
    surface: &SurfaceId,
) -> Result<()> {
    super::place::release(env, workspace, surface)
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
        delete_empty_branch(store, env, &task, &worktree.branch)?;
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
    if !worktrees.is_registered(Path::new(&worktree.path))? {
        let end = remove_stray_folder(store, env, id, &worktree.path)?;
        if matches!(end, WorktreeEnd::Removed) {
            delete_empty_branch(store, env, &task, &worktree.branch)?;
        }
        return Ok(end);
    }
    let result = if force {
        worktrees.remove_forced(command, &vars)
    } else {
        worktrees.remove(command, &vars)
    };
    let refusal = match result {
        Ok(()) if !Path::new(&worktree.path).exists() => None,
        Ok(()) => Some("the remove command left the folder in place".to_owned()),
        Err(error) => Some(error.to_string()),
    };
    let Some(error) = refusal else {
        store.set_worktree_state(id, "removed", env.actor)?;
        delete_empty_branch(store, env, &task, &worktree.branch)?;
        return Ok(WorktreeEnd::Removed);
    };
    let script = matches!(command, WorktreeCommand::Command(_));
    if script && worktrees.remove_if_pristine(&vars)? && !Path::new(&worktree.path).exists() {
        store.append_event(
            env.actor,
            Some(id),
            None,
            "worktree-removed-pristine",
            &serde_json::json!({ "script_error": error }),
        )?;
        store.set_worktree_state(id, "removed", env.actor)?;
        delete_empty_branch(store, env, &task, &worktree.branch)?;
        return Ok(WorktreeEnd::Removed);
    }
    store.append_event(
        env.actor,
        Some(id),
        None,
        "worktree-refused",
        &serde_json::json!({ "error": error }),
    )?;
    Ok(WorktreeEnd::Present)
}

/// Ends a task's folder that git no longer counts as a worktree: an empty one is deleted and recorded `removed`, one
/// with files is left for the user.
fn remove_stray_folder(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    path: &str,
) -> Result<WorktreeEnd> {
    let folder = Path::new(path);
    let empty = std::fs::read_dir(folder)?.next().is_none();
    if empty {
        std::fs::remove_dir(folder)?;
        store.set_worktree_state(id, "removed", env.actor)?;
        return Ok(WorktreeEnd::Removed);
    }
    let error = format!(
        "{path} is not a worktree of the repository and still holds files; remove it by hand"
    );
    store.append_event(
        env.actor,
        Some(id),
        None,
        "worktree-refused",
        &serde_json::json!({ "error": error }),
    )?;
    Ok(WorktreeEnd::Present)
}

/// Deletes a closed task's branch when it holds no commits beyond the main checkout's `HEAD`, so the slug can be
/// spawned again, and logs what became of it. A git failure is logged, never fatal: the close is already verified.
fn delete_empty_branch(store: &Store, env: &Env<'_>, task: &Task, branch: &str) -> Result<()> {
    let (kind, data) = match env.worktrees(task)?.delete_empty_branch(branch) {
        Ok(BranchEnd::Absent) => return Ok(()),
        Ok(BranchEnd::Deleted) => ("branch-deleted", serde_json::json!({ "branch": branch })),
        Ok(BranchEnd::Kept) => (
            "branch-kept",
            serde_json::json!({ "branch": branch, "reason": "it has commits the main checkout lacks" }),
        ),
        Err(error) => (
            "branch-refused",
            serde_json::json!({ "branch": branch, "error": error.to_string() }),
        ),
    };
    store.append_event(env.actor, Some(task.id), None, kind, &data)?;
    Ok(())
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
/// It starts a detached `headwork finish` that waits for the launcher (`launcher_pid`) to be gone, then closes this
/// pane, or the workspace it leaves empty, as the very last act.
///
/// # Errors
///
/// Fails when the finisher cannot start; the pane is closed anyway.
pub fn hand_off(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    session: &SessionRow,
    launcher_pid: u32,
) -> Result<()> {
    let started = start_finisher(store, env, id, &session.id, launcher_pid, "hand-off");
    if let Some(surface) = session.host_surface.as_deref() {
        let workspace = session
            .host_workspace
            .as_deref()
            .map(|w| WorkspaceId(w.to_owned()));
        close_surface_or_workspace(env, workspace.as_ref(), &SurfaceId(surface.to_owned()))?;
    }
    started?;
    Ok(())
}

/// Starts a detached `headwork finish` that waits for `launcher_pid` to be gone, and logs it as `kind`.
fn start_finisher(
    store: &Store,
    env: &Env<'_>,
    id: TaskId,
    session: &str,
    launcher_pid: u32,
    kind: &str,
) -> Result<()> {
    let mut finisher = Command::new(env.exe);
    finisher
        .args([
            "finish",
            "--task",
            &id.to_string(),
            "--wait-pid",
            &launcher_pid.to_string(),
            "--home",
        ])
        .arg(env.home.root())
        .current_dir(env.home.root());
    let started = proc::spawn_detached(&mut finisher);
    let data = match &started {
        Ok((pid, note)) => serde_json::json!({ "finisher": pid, "note": note }),
        Err(error) => serde_json::json!({ "error": error.to_string() }),
    };
    store.append_event(Actor::Worker, Some(id), Some(session), kind, &data)?;
    started?;
    Ok(())
}

/// Whether a session's launcher predates hot reload: it recorded no build, so it may not know the store's schema and
/// fail before it hands its close off.
fn pinned(store: &Store, session: &str) -> Result<bool> {
    Ok(store
        .session_event(session, super::reload::EVENT_BUILD)?
        .is_none())
}

/// The detached finisher: waits for `pid` to be gone, records the end of a session its launcher could not record,
/// then finishes the close and runs the queue.
///
/// # Errors
///
/// Fails when the store fails.
pub fn finish_after(store: &mut Store, env: &Env<'_>, id: TaskId, pid: u32) -> Result<CloseReport> {
    wait_until(LAUNCHER_GRACE, || !proc::is_alive(pid).unwrap_or(false));
    if let Some(session) = store.session_of(id)?
        && matches!(session.state, SessionState::Starting | SessionState::Live)
    {
        store.session_ended(&session.id, None)?;
    }
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

pub(super) fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
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
