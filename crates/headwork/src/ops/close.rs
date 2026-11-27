//! `close`: end a task's session, pane, process tree and worktree, and mark it `closed` only once each is verified
//! gone.
//!
//! The order is the one today's `close-child.sh` learned: everything else first, the pane (or the workspace it leaves
//! empty) next, ending the program last, because a worker usually closes itself from inside the pane being closed.
//! Under headwork a close first climbs the exit ladder ([`super::exit`]) in the session's pane, so `claude` leaves on
//! its own and archives its Remote Control session at claude.ai, and ends `claude`'s tree only when it has not left
//! after the last step. A self-close from the `headwork close` command runs inside the session's own tool call, so it
//! hands the ladder to a detached closer (`headwork self-exit`) that starts once the command is gone. When `claude`
//! exits with the task `closing`, the launcher starts a detached `headwork finish` that outlives the pane, and closes
//! its own pane as its last act. `finish` waits for the launcher to be gone, removes the worktree, verifies, and marks
//! the task `closed`.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use headwork_adapters::git::GitHost;
use headwork_adapters::host::{SurfaceId, TerminalHost, WorkspaceId};
use headwork_adapters::proc;
use headwork_adapters::worktree::BranchEnd;
use headwork_core::config::{REVIEW_KIND, TaskVars, WorktreeCommand};
use headwork_core::exit::ExitMode;
use headwork_core::guard::DEFAULT_BRANCHES;
use headwork_core::lifecycle::{
    CleanupFacts, CloseCause, CloseFacts, CloseGo, FinishFacts, MergeSource, PrEnd, Trigger,
    WorktreeEnd,
};
use headwork_core::removal::{RemovalDecision, RemovalFailure, classify, decide};
use headwork_core::{Actor, Config, JobState, SessionState, TaskShape, TaskState};
use headwork_store::{
    RemovalAttempts, SessionRow, Store, StoreError, Task, TaskId, UsageTotal, WorktreeRow,
};
use serde::Serialize;

use super::hook::{Git, GitProbe};
use super::session_procs;
use super::{Env, slashed};

/// How long `close` waits for a launcher to finish after its `claude` was ended.
const LAUNCHER_GRACE: Duration = Duration::from_secs(60);

/// How long the last checks wait for processes to vanish.
const PROCESS_GRACE: Duration = Duration::from_secs(10);

/// How long a finisher retries a held close claim: a child another thread forked while the claim was held shares the
/// lock until it starts its program, so a claim just released can still look held for a moment.
const CLAIM_WAIT: Duration = Duration::from_secs(2);

/// How long a close from outside the session waits for a finisher that already holds the claim, such as the one the
/// launcher starts once `claude` has left, before it reports the close as another finisher's.
const FINISHER_WAIT: Duration = Duration::from_secs(120);

/// Where a close ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloseReport {
    /// `<project>-<slug>`.
    pub name: String,
    /// The task's state afterwards.
    pub state: TaskState,
    /// Why it is not `closed` yet, when it is not.
    pub pending: Option<String>,
    /// The tokens the task and its parts spent, once it is closed (`docs/usage.md`).
    pub usage: Option<UsageTotal>,
}

/// What a self-close reports: the session's launcher finishes the close once `claude` has left.
pub const LAUNCHER_FINISHES: &str = "the launcher finishes the close";

/// What a self-close from the `headwork close` command reports: a detached closer asks the session to exit once the
/// command is gone, and the launcher finishes the close.
pub const CLOSER_ASKS: &str = "a detached closer asks the session to exit once this command ends; the launcher finishes the close";

/// How long the detached closer waits for the self-closing command to be gone before it asks anyway.
pub const CLOSER_WAIT: Duration = Duration::from_secs(120);

/// How a self-close has its own session asked to exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnExit {
    /// The `headwork close` command runs inside the session's own tool call, and `/exit` waits behind that call: a
    /// detached closer climbs the ladder once the command is gone.
    Detached,
    /// The caller's answer is already out, as the MCP `done`'s is: the ladder runs here.
    Here,
}

impl CloseReport {
    /// Whether the close was left to the session's launcher, which runs in the very session that closed. Its caller is
    /// about to end with `claude` and must start nothing more, such as a queued task, that it could leave half done;
    /// the finisher runs the queue.
    #[must_use]
    pub fn left_to_launcher(&self) -> bool {
        matches!(
            self.pending.as_deref(),
            Some(LAUNCHER_FINISHES | CLOSER_ASKS)
        )
    }
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
    close_now(store, env, id, own_session)
}

/// Closes a part its lead dropped, for the lead's `part_drop`: the one close a worker may run on another task.
///
/// # Errors
///
/// Fails when the task is not a part of `lead`, is not dropped, or the close fails.
pub fn close_dropped_part(
    store: &mut Store,
    env: &Env<'_>,
    lead: TaskId,
    part: TaskId,
) -> Result<CloseReport> {
    let task = store.task(part)?;
    if task.shape.parent() != Some(lead) {
        bail!("{} is not a part of this lead", task.name);
    }
    if task.state != TaskState::Dropped {
        bail!("{} is {}, not dropped", task.name, task.state);
    }
    close_now(store, env, part, None)
}

fn close_now(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    own_session: Option<&str>,
) -> Result<CloseReport> {
    let before = store.task(id)?;
    if before.shape.is_lead() {
        close_parts(store, env, id)?;
    }
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
    end_closing(store, env, id, own_session, OwnExit::Detached)
}

/// Closes every part of a lead that is not closing or closed yet, before the lead itself: the orchestrator's close of
/// a lead, and a lead's drop, end its parts first.
fn close_parts(store: &mut Store, env: &Env<'_>, lead: TaskId) -> Result<()> {
    for part in store.parts(lead)? {
        if !matches!(
            store.task(part)?.state,
            TaskState::Closing | TaskState::Closed
        ) {
            close(store, env, part, None)?;
        }
    }
    Ok(())
}

/// Finishes a task stuck in `closing` whose pull requests are all merged, or that was dropped and has none, and that
/// runs no job.
///
/// The close runs once more, and a worktree the remove command still refuses is recorded `kept` instead of blocking,
/// left on disk for `headwork clean` or the user, never deleted. A pane or a process that is still there keeps
/// refusing. Only the orchestrator or the user forces.
///
/// # Errors
///
/// Fails, changing nothing, when the actor is a worker, the task is not `closing`, a pull request of it is not merged,
/// none is found and the task was not dropped, or a job of it runs; also when the store or the git host fails.
pub fn close_forced(
    store: &mut Store,
    env: &Env<'_>,
    git: &dyn GitHost,
    id: TaskId,
) -> Result<CloseReport> {
    if env.actor == Actor::Worker {
        bail!("a worker does not force-close a task: ask the orchestrator");
    }
    let task = store.task(id)?;
    if task.state != TaskState::Closing {
        bail!(
            "`force` is for a task stuck in closing; {} is {}: close it without `force`",
            task.name,
            task.state
        );
    }
    let found = super::prs::lookup(store, env, git, &task)?;
    if found.seen.is_empty() && !store.was_dropped(id)? {
        bail!(
            "`force` closes only a task whose pull requests are all merged, or a dropped one with none, and {} has none \
             and was not dropped: close it without `force`",
            task.name
        );
    }
    if let Some(pr) = found
        .seen
        .iter()
        .find(|pr| pr.state != headwork_adapters::git::PrState::Merged)
    {
        bail!(
            "`force` closes only a task whose pull requests are all merged; {} is {}",
            pr.url,
            pr.state.as_str()
        );
    }
    if let Some(job) = store
        .open_jobs(Some(id))?
        .iter()
        .find(|job| super::jobs::liveness(job).is_none_or(|now| now.is_alive()))
    {
        bail!(
            "`force` closes only a task with no live job; job {} (`{}`) still runs: stop it first",
            job.id,
            job.label
        );
    }
    store.append_event(
        env.actor,
        Some(id),
        None,
        "force-finish",
        &serde_json::json!({ "prs": found.seen.iter().map(|pr| pr.url.as_str()).collect::<Vec<_>>() }),
    )?;
    let Some(_claim) = claim_close(env, id, CLAIM_WAIT)? else {
        return report(store, id, Some(ANOTHER_FINISHER.to_owned()));
    };
    finish_claimed(store, env, id, true)
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
    end_closing(store, env, id, own_session, OwnExit::Detached)
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
    super::review_loop::cancel_open(store, &before, env.actor)?;
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

/// Whether the user's go to close is missing: the task is a lead, `[leads] ask_before_close` is on, its task is not
/// `silent` and no ask headed `Close` was answered with a go.
///
/// # Errors
///
/// Fails when the store fails.
pub fn close_go(store: &Store, config: &Config, task: &Task) -> Result<CloseGo> {
    if !task.shape.is_lead()
        || !config.leads.asks_before_close(task.close)
        || store.close_go_given(task.id)?
    {
        return Ok(CloseGo::Clear);
    }
    Ok(CloseGo::Awaiting)
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
    if let Some(review) = store.open_review(id)? {
        if review.task_id == id {
            bail!(
                "review-open: review {} of this task is {}; it must finish before you close",
                review.id,
                review.state
            );
        }
        bail!(
            "review-unfinished: review {} is {}; finish it with `review_finish` before you close",
            review.id,
            review.state
        );
    }
    if task.state == TaskState::NeedsUser {
        bail!("{}", headwork_core::Refusal::OpenAsk);
    }
    let (pr, info) = if matches!(
        task.state,
        TaskState::Working | TaskState::InReview | TaskState::Merged
    ) {
        let found = super::prs::lookup(store, env, git, task)?;
        (found.end(), found.deciding().cloned())
    } else {
        (PrEnd::None, None)
    };
    let facts = FinishFacts {
        open_ask: store.has_waiting_ask(id)?,
        running_job: super::jobs::has_running_job(store, id)?,
        report_sent: store.report_sent(id)?,
        live_parts: store.live_parts(id)?,
        go: close_go(store, env.config, task)?,
        pr,
    };
    let close = Trigger::Close(CloseFacts {
        cause: CloseCause::Done,
        open_ask: facts.open_ask,
        running_job: facts.running_job,
        report_sent: facts.report_sent,
        live_parts: facts.live_parts,
        go: facts.go,
    });
    let project = env.project(task)?;
    let merged = Trigger::Merged {
        source: MergeSource::GitHost,
        policy: project.merge,
    };
    let steps = match (task.state, pr) {
        (TaskState::Merged | TaskState::InReview, PrEnd::Open) => {
            bail!("{}", headwork_core::Refusal::PrOpen)
        }
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

/// Ends a `closing` task's session, pane, process tree and worktree, and marks it `closed` once each is verified gone.
///
/// A close from outside the session climbs the [`ExitMode::Outside`] ladder, ends `claude` only when it stays, waits
/// for the launcher and finishes. A self-close (`own_session` is the task's session) leaves the finish to the launcher:
/// `own_exit` says whether the [`ExitMode::Own`] ladder runs here or in a detached closer.
///
/// # Errors
///
/// Fails when the store fails or the task is not closing.
pub fn end_closing(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    own_session: Option<&str>,
    own_exit: OwnExit,
) -> Result<CloseReport> {
    let task = store.task(id)?;
    remove_brief(env, &task.brief_path);
    if let Some(session) = store.session_of(id)?
        && let Some(launcher) = session.launcher().filter(session_procs::runs)
    {
        let target = session.claude().unwrap_or_else(|| launcher.clone());
        if own_session == Some(session.id.as_str()) {
            let exit = match own_exit {
                OwnExit::Detached => "detached",
                OwnExit::Here => "here",
            };
            store.append_event(
                env.actor,
                Some(id),
                Some(&session.id),
                "self-close",
                &serde_json::json!({ "exit": exit }),
            )?;
            if pinned(store, &session.id)? {
                let _logged: Result<()> =
                    start_finisher(store, env, id, &session.id, launcher.pid, "backup-finisher");
            }
            let pending = match own_exit {
                OwnExit::Here => {
                    if !super::exit::ask_session(store, env, ExitMode::Own, Some(id), &session)? {
                        session_procs::end(&[target])?.log(
                            store,
                            env.actor,
                            Some(id),
                            &session.id,
                        )?;
                    }
                    LAUNCHER_FINISHES
                }
                OwnExit::Detached => {
                    if start_closer(store, env, id, &session.id).is_ok() {
                        CLOSER_ASKS
                    } else {
                        session_procs::end(&[target])?.log(
                            store,
                            env.actor,
                            Some(id),
                            &session.id,
                        )?;
                        LAUNCHER_FINISHES
                    }
                }
            };
            return report(store, id, Some(pending.to_owned()));
        }
        if !super::exit::ask_session(store, env, ExitMode::Outside, Some(id), &session)? {
            session_procs::end(&[target])?.log(store, env.actor, Some(id), &session.id)?;
        }
        wait_until(LAUNCHER_GRACE, || !session_procs::runs(&launcher));
    }
    finish_within(store, env, id, FINISHER_WAIT)
}

/// Starts the detached closer of a self-close from the `headwork close` command: `headwork self-exit`, which waits for
/// this process to be gone before it climbs the [`ExitMode::Own`] ladder. Logged as `closer`, with the error when it
/// does not start.
fn start_closer(store: &Store, env: &Env<'_>, id: TaskId, session: &str) -> Result<()> {
    let mut closer = Command::new(env.exe);
    closer
        .args([
            "self-exit",
            "--task",
            &id.to_string(),
            "--session",
            session,
            "--after",
            &std::process::id().to_string(),
            "--home",
        ])
        .arg(env.home.root())
        .current_dir(env.home.root());
    for var in super::resume::SURFACE_VARS {
        closer.env_remove(var);
    }
    let argv: Vec<String> = closer
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let started = proc::spawn_detached(&mut closer);
    let data = match &started {
        Ok((pid, note)) => serde_json::json!({ "closer": pid, "argv": argv, "note": note }),
        Err(error) => serde_json::json!({ "argv": argv, "error": error.to_string() }),
    };
    store.append_event(env.actor, Some(id), Some(session), "closer", &data)?;
    started?;
    Ok(())
}

/// What the detached closer of a self-close did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelfExit {
    /// The session's launcher was already gone, or the session is unknown: nothing to ask.
    Gone,
    /// The task is no longer closing (a later close finished it), so nothing was asked.
    NotClosing,
    /// `claude` left on the ladder.
    Left,
    /// `claude` stayed through the ladder and was ended.
    Ended,
}

impl SelfExit {
    /// What the closer prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gone => "the session was already gone",
            Self::NotClosing => "the task is no longer closing; nothing asked",
            Self::Left => "claude left on its own",
            Self::Ended => "claude stayed through every step and was ended",
        }
    }
}

/// The detached closer of a self-close: once the closing command `after` is gone, the [`ExitMode::Own`] ladder.
///
/// It waits up to [`CLOSER_WAIT`] for `after`, climbs the ladder in the session's pane and ends `claude` only when it
/// stays. It holds no close claim: the launcher's hand-off finishes the close once `claude` has left, as after any
/// self-close.
///
/// # Errors
///
/// Fails when the store fails or a process cannot be ended.
pub fn self_exit_after(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    session: &str,
    after: u32,
) -> Result<SelfExit> {
    wait_until(CLOSER_WAIT, || !proc::is_alive(after).unwrap_or(false));
    let Some((row, _)) = store.session(session)? else {
        return Ok(SelfExit::Gone);
    };
    let Some(launcher) = row.launcher().filter(session_procs::runs) else {
        return Ok(SelfExit::Gone);
    };
    if store.task(id)?.state != TaskState::Closing {
        return Ok(SelfExit::NotClosing);
    }
    let target = row.claude().unwrap_or(launcher);
    if super::exit::ask_session(store, env, ExitMode::Own, Some(id), &row)? {
        return Ok(SelfExit::Left);
    }
    session_procs::end(&[target])?.log(store, env.actor, Some(id), &row.id)?;
    Ok(SelfExit::Ended)
}

fn begin_closing(store: &mut Store, env: &Env<'_>, id: TaskId) -> Result<()> {
    super::review_loop::cancel_open(store, &store.task(id)?, env.actor)?;
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
    finish_within(store, env, id, CLAIM_WAIT)
}

/// [`finish`], waiting up to `wait` for a finisher that holds the claim; once it lets go, the close it ran is
/// reported as it ended.
fn finish_within(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    wait: Duration,
) -> Result<CloseReport> {
    let Some(_claim) = claim_close(env, id, wait)? else {
        store.append_event(
            env.actor,
            Some(id),
            None,
            "close-claimed",
            &serde_json::json!({ "note": "another finisher holds this close" }),
        )?;
        return report(store, id, Some(ANOTHER_FINISHER.to_owned()));
    };
    finish_claimed(store, env, id, false)
}

/// What a finisher reports when another one already runs the same close.
pub const ANOTHER_FINISHER: &str = "another finisher is closing this task";

/// Takes the task's close claim: an exclusive lock on `<home>/closing/<id>.lock`, held until the returned file drops
/// and released by the system if the finisher dies. `None` when another finisher holds it, so two finishers (the
/// launcher's, a backup, the tick, `headwork close`) never end the same task at once.
fn claim_close(env: &Env<'_>, id: TaskId, wait: Duration) -> Result<Option<std::fs::File>> {
    let folder = env.home.root().join("closing");
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    let path = folder.join(format!("{id}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < wait => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("locking {}", path.display()));
            }
        }
    }
}

fn finish_claimed(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    force: bool,
) -> Result<CloseReport> {
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
        super::exit::ask_session(store, env, ExitMode::Outside, Some(id), session)?;
        pane_gone = close_pane(env, task.shape, session)?;
        session_procs::end_all(session)?.log(store, env.actor, Some(id), &session.id)?;
    }
    let records = session
        .as_ref()
        .map(SessionRow::processes)
        .unwrap_or_default();
    let processes_gone = wait_until(PROCESS_GRACE, || !records.iter().any(session_procs::runs));
    finalize_jobs(store, env, id)?;
    let worktree = if processes_gone {
        store.end_task_sessions(id, env.actor)?;
        super::reap::end_task_leftovers(store, env, id, &task.name.to_string())?;
        super::helpers::remove_all(store, env, id)?;
        if !force
            && let Some(waiting) =
                removal_status(store, id)?.and_then(|status| status.waiting_note())
        {
            return report(store, id, Some(waiting));
        }
        let mode = if force {
            RemovalMode::Now
        } else {
            RemovalMode::Normal
        };
        let end = remove_worktree(store, env, id, mode)?;
        if force && end == WorktreeEnd::Present {
            keep_worktree(store, env, id)?
        } else {
            end
        }
    } else {
        WorktreeEnd::Present
    };
    let facts = CleanupFacts {
        pane_gone,
        processes_gone,
        worktree,
    };
    match store.transition(id, &Trigger::Cleaned(facts), env.actor) {
        Ok(_) => {
            store.supersede_asks(id, env.actor)?;
            record_usage_total(store, env, id);
            update_after_close(store, env, &task);
            super::backlog::after_close(store, env);
            report(store, id, None)
        }
        Err(StoreError::Refused { refusal, .. }) => report(store, id, Some(refusal.to_string())),
        Err(error) => Err(error.into()),
    }
}

/// The event a closed task's token total is logged in.
pub const EVENT_USAGE_TOTAL: &str = "usage-total";

/// Logs the closed task's tokens, with its parts', as `usage-total`. It never fails the close.
fn record_usage_total(store: &Store, env: &Env<'_>, id: TaskId) {
    let Ok(total) = super::usage::task_total(store, id) else {
        return;
    };
    let mut data = serde_json::to_value(total.tokens).unwrap_or_default();
    data["responses"] = total.responses.into();
    data["total"] = total.tokens.total().into();
    if let Some(estimate) = super::status::estimate(&env.config.usage, &total.models) {
        data["estimate"] = estimate.into();
    }
    drop(store.append_event(env.actor, Some(id), None, EVENT_USAGE_TOTAL, &data));
}

/// The close trigger of `docs/autoupdate.md`: once a task of `[update] project` is closed, with `[update] enabled`, a
/// detached `headwork update --wait 30` installs the release its merge produces. A start that fails is logged in
/// `update-triggered` and never fails the close.
fn update_after_close(store: &Store, env: &Env<'_>, task: &Task) {
    if !env.config.update.enabled || task.project() != &env.config.update.project {
        return;
    }
    let starter = super::update::Detached {
        home: env.home,
        exe: env.exe,
    };
    drop(super::update::trigger(
        store,
        &starter,
        env.actor,
        Some(task.id),
        super::update::TriggeredBy::Close,
        super::update::close_args(),
    ));
}

/// Records a worktree the remove command refused as `kept`, for a forced close: it stays on disk for `headwork clean`
/// or the user.
fn keep_worktree(store: &mut Store, env: &Env<'_>, id: TaskId) -> Result<WorktreeEnd> {
    store.set_worktree_state(id, "kept", env.actor)?;
    if let Some(ask) = store
        .topic_ask(id, super::tick::WORKTREE_TOPIC)?
        .filter(|ask| ask.state.is_waiting())
    {
        store.supersede_ask(ask.id, id, env.actor)?;
    }
    store.append_event(
        env.actor,
        Some(id),
        None,
        "worktree-kept",
        &serde_json::json!({ "reason": "forced close: left on disk for `headwork clean` or the user" }),
    )?;
    Ok(WorktreeEnd::Kept)
}

/// Ends the task's open jobs: a closed task owns nothing, so a job still running is stopped and every open job is
/// recorded `done`.
fn finalize_jobs(store: &mut Store, env: &Env<'_>, id: TaskId) -> Result<()> {
    for job in store.open_jobs(Some(id))? {
        let note = super::jobs::end_process(&job)?.map_or_else(
            || "the task closed".to_owned(),
            |left| format!("the task closed; {left}"),
        );
        store.set_job_state(job.id, JobState::Done, Some(&note), env.actor)?;
    }
    Ok(())
}

/// Closes the session's pane, or its workspace when the pane is the last thing in it, then ends the host's program.
/// Returns whether the pane is gone.
pub(super) fn close_pane(
    env: &Env<'_>,
    shape: TaskShape<TaskId>,
    session: &SessionRow,
) -> Result<bool> {
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
        close_surface_or_workspace(env, shape, workspace.as_ref(), &surface)?;
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
/// it, and a lead's part closes only its tab (see [`super::place::release`]).
///
/// # Errors
///
/// Returns the host's failure.
pub fn close_surface_or_workspace(
    env: &Env<'_>,
    shape: TaskShape<TaskId>,
    workspace: Option<&WorkspaceId>,
    surface: &SurfaceId,
) -> Result<()> {
    super::place::release(env, shape, workspace, surface)
}

/// How hard a worktree removal tries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemovalMode {
    /// Respects the backoff of an earlier failure and never deletes changes.
    Normal,
    /// Tries now whatever the backoff says, and never deletes changes: a forced close, a user's `headwork clean`.
    Now,
    /// Tries now and deletes the changes: the user's answered `Delete`.
    Delete,
}

pub(super) fn remove_worktree(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    mode: RemovalMode,
) -> Result<WorktreeEnd> {
    let task = store.task(id)?;
    let Some(status) = removal_status(store, id)? else {
        return Ok(WorktreeEnd::NoWorktree);
    };
    let worktree = status.worktree;
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
    if mode == RemovalMode::Normal && status.decision != RemovalDecision::Try {
        return Ok(WorktreeEnd::Present);
    }
    let branches = task_branches(&worktree);
    let main = slashed(&env.main_path(&task)?);
    let vars = TaskVars {
        slug: task.slug().as_str(),
        branch: &worktree.branch,
        main: &main,
        worktree: &worktree.path,
    };
    let command = if task.kind == REVIEW_KIND {
        &WorktreeCommand::Builtin
    } else {
        &env.project(&task)?.worktree_remove
    };
    let worktrees = env.worktrees(&task)?;
    if !worktrees.is_registered(Path::new(&worktree.path))? {
        let end = remove_stray_folder(store, env, id, &worktree.path)?;
        if matches!(end, WorktreeEnd::Removed) {
            delete_empty_branches(store, env, &task, &branches)?;
        }
        return Ok(end);
    }
    let result = if mode == RemovalMode::Delete || task.kind == REVIEW_KIND {
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
        delete_empty_branches(store, env, &task, &branches)?;
        return Ok(WorktreeEnd::Removed);
    };
    if classify(&error) == RemovalFailure::NotAWorktree {
        let end = remove_stray_folder(store, env, id, &worktree.path)?;
        if matches!(end, WorktreeEnd::Removed) {
            delete_empty_branches(store, env, &task, &branches)?;
        }
        return Ok(end);
    }
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
        delete_empty_branches(store, env, &task, &branches)?;
        return Ok(WorktreeEnd::Removed);
    }
    refuse_removal(store, env, id, &error)?;
    Ok(WorktreeEnd::Present)
}

/// Logs a refused removal and counts it, so the next attempt backs off.
fn refuse_removal(store: &mut Store, env: &Env<'_>, id: TaskId, error: &str) -> Result<()> {
    store.record_removal_failure(id, error, env.actor)?;
    Ok(())
}

/// A task's worktree with its failed removals so far and what to do about them.
#[derive(Debug, Clone)]
pub(super) struct RemovalStatus {
    /// The worktree row.
    pub worktree: WorktreeRow,
    /// The failures, the time since the last and its error.
    pub attempts: RemovalAttempts,
    /// Whether a removal runs now, waits or asks the user; `Try` for a worktree that is not present, whose folder is
    /// gone, or that never failed.
    pub decision: RemovalDecision,
}

impl RemovalStatus {
    /// Why the worktree is not tried again yet: an earlier removal failed and is backing off, or needs the user's
    /// keep-or-delete answer. `None` when a removal may run now.
    pub(super) fn waiting_note(&self) -> Option<String> {
        let error = self.attempts.last_error.as_deref().unwrap_or_default();
        let failures = self.attempts.failures;
        match self.decision {
            RemovalDecision::Try => None,
            RemovalDecision::Wait { remaining_s } => Some(format!(
                "removing the worktree failed {failures} times; next try in {remaining_s} s: {error}"
            )),
            RemovalDecision::Ask => Some(format!(
                "removing the worktree failed {failures} times; waiting for the keep-or-delete answer: {error}"
            )),
        }
    }
}

/// The one reading of a task's worktree and its removal backoff, used by close, clean and the tick. `None` without a
/// worktree.
pub(super) fn removal_status(store: &Store, id: TaskId) -> Result<Option<RemovalStatus>> {
    let Some(worktree) = store.worktree_of(id)? else {
        return Ok(None);
    };
    let attempts = store.removal_attempts(id)?;
    let decision = if worktree.state != "present"
        || !Path::new(&worktree.path).exists()
        || attempts.failures == 0
    {
        RemovalDecision::Try
    } else {
        let error = attempts.last_error.as_deref().unwrap_or_default();
        decide(
            attempts.failures,
            attempts.since_last_s.unwrap_or(u64::MAX),
            classify(error) == RemovalFailure::Transient,
        )
    };
    Ok(Some(RemovalStatus {
        worktree,
        attempts,
        decision,
    }))
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
    refuse_removal(store, env, id, &error)?;
    Ok(WorktreeEnd::Present)
}

/// The branches a task's worktree leaves behind: the stored one, and the one checked out in the folder when the worker
/// renamed or switched it.
fn task_branches(worktree: &WorktreeRow) -> Vec<String> {
    let mut branches = vec![worktree.branch.clone()];
    if let Some(current) = Git.branch(&worktree.path)
        && !branches.contains(&current)
        && !DEFAULT_BRANCHES.contains(&current.as_str())
        && current != "HEAD"
    {
        branches.push(current);
    }
    branches
}

fn delete_empty_branches(
    store: &Store,
    env: &Env<'_>,
    task: &Task,
    branches: &[String],
) -> Result<()> {
    for branch in branches {
        delete_empty_branch(store, env, task, branch)?;
    }
    Ok(())
}

/// Deletes a closed task's branch when it holds no commits beyond the main checkout's `HEAD` or its upstream, so the
/// slug can be spawned again, and logs what became of it. A branch that looks unmerged is checked once more after a
/// fetch of the upstream, since its pull request was usually merged on the remote moments ago. A git failure is logged,
/// never fatal: the close is already verified.
fn delete_empty_branch(store: &Store, env: &Env<'_>, task: &Task, branch: &str) -> Result<()> {
    let worktrees = env.worktrees(task)?;
    let mut end = worktrees.delete_empty_branch(branch);
    let mut fetch_error = None;
    if matches!(end, Ok(BranchEnd::Kept)) {
        match worktrees.fetch_upstream() {
            Ok(true) => end = worktrees.delete_empty_branch(branch),
            Ok(false) => {}
            Err(error) => fetch_error = Some(error.to_string()),
        }
    }
    let (kind, data) = match end {
        Ok(BranchEnd::Absent) => return Ok(()),
        Ok(BranchEnd::Deleted) => ("branch-deleted", serde_json::json!({ "branch": branch })),
        Ok(BranchEnd::Kept) => (
            "branch-kept",
            serde_json::json!({
                "branch": branch,
                "reason": "it has commits the main checkout lacks",
                "fetch_error": fetch_error,
            }),
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
            .and_then(|s| s.launcher())
            .is_some_and(|launcher| session_procs::runs(&launcher));
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
        let shape = store.task(id).map_or(TaskShape::Plain, |task| task.shape);
        close_surface_or_workspace(
            env,
            shape,
            workspace.as_ref(),
            &SurfaceId(surface.to_owned()),
        )?;
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
    let usage = if task.state == TaskState::Closed {
        Some(super::usage::task_total(store, id)?)
    } else {
        None
    };
    Ok(CloseReport {
        name: task.name.to_string(),
        state: task.state,
        pending,
        usage,
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
