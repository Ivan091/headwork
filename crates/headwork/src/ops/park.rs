//! `park`: the graceful shutdown before a restart (design section 6.1).
//!
//! Every running session is asked for a recap and brought to a safe point; registered jobs are stopped at a
//! checkpoint, stopped for a rerun, or recorded as left running; the store records everything, and the panes close.
//! A session that has not written its recap by the deadline gets a mechanical one.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use headwork_adapters::host::{Key, SurfaceId};
use headwork_core::config::ParkPolicy;
use headwork_core::lifecycle::{ParkedFacts, Trigger};
use headwork_core::{JobState, MachineMode, RecapKind, SessionState, TaskShape, TaskState};
use headwork_store::{JobRow, NewRecap, SessionRow, Store, Task, TaskId};
use serde::Serialize;

use super::close::{close_pane, type_exit, wait_until};
use super::{Env, jobs, recap};

/// What a session is told when a park begins.
pub const PARK_PROMPT: &str = "headwork: the machine is about to restart. Reach a safe point now and call the \
    headwork `recap` tool once. After the restart a new session with an empty context takes over from your recap and \
    your brief alone, so write it to continue from without this conversation: what was done and what is left; the \
    decisions you took and why; open questions and anything waiting on the user; the exact next step; the files, \
    branch, worktree and pull requests involved; what runs, with the command to start it again; and the user's \
    standing constraints and wishes from this conversation. Register any long process with `job_register`. Do not \
    start new work.";

/// What the orchestrator is told when a park begins. The park closes it as its last step (unless `--keep-orchestrator`),
/// and the resume opens a new orchestrator session.
pub const ORCHESTRATOR_PARK_PROMPT: &str = "headwork: the machine is about to restart. Call the headwork `recap` tool once. \
    After the restart a new orchestrator session with an empty context takes over from your recap, the board and the \
    log alone, so write it to continue from without this conversation: what the user asked for and what became of each \
    request; what each task is doing and why it was shaped that way; the decisions you took and why; what waits on the \
    user; your exact next step; and the user's standing constraints and preferences. Do not start new work.";

/// How a park waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParkOptions {
    /// How long to wait for recaps and checkpoints before forcing what is left.
    pub deadline: Duration,
    /// How often to look.
    pub poll: Duration,
    /// Leave the orchestrator running for the restart to end, instead of closing it as the park's last step.
    pub keep_orchestrator: bool,
}

impl Default for ParkOptions {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(300),
            poll: Duration::from_secs(2),
            keep_orchestrator: false,
        }
    }
}

/// Whose recap a parked session left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecapSource {
    /// The session wrote it.
    Session,
    /// The tool wrote it at the deadline.
    Mechanical,
}

/// One parked session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParkedSession {
    /// `<project>-<slug>`.
    pub name: String,
    /// The state it returns to.
    pub was: TaskState,
    /// Whose recap it left.
    pub recap: RecapSource,
    /// The handoff file.
    pub handoff: String,
    /// The safety copy of uncommitted changes, when there were any.
    pub snapshot: Option<String>,
    /// The question it had open, to be asked again after the resume.
    pub open_question: Option<String>,
    /// Why the pane or process could not be closed, when it could not.
    pub problem: Option<String>,
}

/// What happened to one job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParkedJob {
    /// Its label.
    pub label: String,
    /// What happened.
    pub outcome: String,
    /// It runs on and a restart will end it.
    pub left_running: bool,
}

/// The orchestrator session of a park.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParkedOrchestrator {
    /// Its Claude session id, which the resume brings back.
    pub session: String,
    /// Whose recap it left.
    pub recap: RecapSource,
    /// Why it could not be asked for its recap, when it could not.
    pub problem: Option<String>,
    /// What became of it at the end of the park.
    pub end: OrchestratorEnd,
}

/// What became of the orchestrator at the end of a park.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "how")]
pub enum OrchestratorEnd {
    /// `--keep-orchestrator`: it runs on until the restart ends it.
    Kept,
    /// It no longer ran.
    Gone,
    /// It left on `/exit`, so Claude Code archived its Remote Control session.
    Exited,
    /// It did not leave on `/exit` within the grace, so its processes or its pane were ended.
    Ended,
    /// The park runs inside the orchestrator (its pane, or a process it started), so a detached `headwork park
    /// --close-orchestrator` closes it once the park has exited.
    Detached {
        /// The closer's process id.
        pid: u32,
    },
    /// It could not be closed.
    Failed {
        /// Why.
        problem: String,
    },
}

/// What a park did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParkReport {
    /// The park's id.
    pub park: i64,
    /// The sessions, in the order they were parked.
    pub sessions: Vec<ParkedSession>,
    /// The orchestrator session, when one was running; closed last unless kept.
    pub orchestrator: Option<ParkedOrchestrator>,
    /// The jobs.
    pub jobs: Vec<ParkedJob>,
    /// Everything stopped cleanly: it is safe to restart.
    pub safe_to_restart: bool,
}

struct Target {
    task: Task,
    session: Option<SessionRow>,
    asked: Vec<(String, String)>,
    expects_recap: bool,
}

/// Parks the machine.
///
/// # Errors
///
/// Fails when the machine is not in `normal` (or `parking`, to carry on a park that was cut short), or the store
/// fails.
pub fn park(store: &mut Store, env: &Env<'_>, opts: ParkOptions) -> Result<ParkReport> {
    let record = match store.machine_mode()? {
        MachineMode::Normal => store.begin_park(opts.deadline.as_secs(), env.actor)?,
        MachineMode::Parking => store
            .current_park()?
            .context("the machine is parking but no park is recorded")?,
        MachineMode::Parked => {
            bail!("the machine is already parked; run `headwork resume` after the restart")
        }
        MachineMode::Resuming => bail!("a resume is running; wait for it to finish"),
    };
    let began = SystemTime::now();
    let limit = Instant::now() + opts.deadline;
    let targets = begin_sessions(store, env)?;
    let orchestrator = begin_orchestrator(store, env, record.id)?;
    let mut parked_jobs = stop_fast_jobs(store, env)?;

    loop {
        let mut waiting = settle_jobs(store, env, began, &mut parked_jobs)?;
        for target in &targets {
            if target.expects_recap
                && store
                    .session_park_recap(record.id, target.task.id)?
                    .is_none()
                && target_alive(target)
            {
                waiting = true;
            }
        }
        if let Some(orchestrator) = &orchestrator
            && orchestrator.expects_recap
            && session_alive(&orchestrator.session)
            && store
                .orchestrator_park_recap(record.id, &orchestrator.session.id)?
                .is_none()
        {
            waiting = true;
        }
        if !waiting || Instant::now() >= limit {
            break;
        }
        std::thread::sleep(opts.poll);
    }

    force_jobs(store, env, &mut parked_jobs)?;
    let mut sessions = Vec::new();
    for target in &targets {
        sessions.push(finish_session(store, env, record.id, target)?);
    }
    let mut parked_orchestrator = orchestrator
        .as_ref()
        .map(|orchestrator| finish_orchestrator(store, env, record.id, orchestrator))
        .transpose()?;
    let safe =
        sessions.iter().all(|s| s.problem.is_none()) && parked_jobs.iter().all(|j| !j.left_running);
    store.finish_park(record.id, env.actor)?;
    if let (Some(target), Some(parked)) = (&orchestrator, &mut parked_orchestrator) {
        parked.end = end_orchestrator(store, env, target, opts.keep_orchestrator)?;
    }
    let orchestrator = parked_orchestrator;
    Ok(ParkReport {
        park: record.id,
        sessions,
        orchestrator,
        jobs: parked_jobs,
        safe_to_restart: safe,
    })
}

fn target_alive(target: &Target) -> bool {
    target.session.as_ref().is_some_and(session_alive)
}

/// Whether a session has not ended and its `claude` (or launcher) still runs.
pub(super) fn session_alive(session: &SessionRow) -> bool {
    matches!(session.state, SessionState::Live | SessionState::Starting)
        && session
            .claude()
            .or_else(|| session.launcher())
            .is_some_and(|process| super::session_procs::runs(&process))
}

fn begin_sessions(store: &mut Store, env: &Env<'_>) -> Result<Vec<Target>> {
    let mut targets = Vec::new();
    for state in [
        TaskState::Parking,
        TaskState::Working,
        TaskState::NeedsUser,
        TaskState::InReview,
    ] {
        for id in store.tasks_in(state)? {
            let task = store.task(id)?;
            let session = store.session_of(id)?;
            let blocked = state == TaskState::NeedsUser;
            let asked = if blocked {
                store.supersede_asks(id, env.actor)?
            } else {
                Vec::new()
            };
            if state != TaskState::Parking {
                store.transition(id, &Trigger::Park, env.actor)?;
            }
            if let Some(surface) = session.as_ref().and_then(|s| s.host_surface.as_deref()) {
                let surface = SurfaceId(surface.to_owned());
                if blocked {
                    drop(env.host.clear_blocked(&surface));
                } else if let Err(error) = ask_for_recap(env, &surface) {
                    store.append_event(
                        env.actor,
                        Some(id),
                        None,
                        "park-notice-failed",
                        &serde_json::json!({ "error": format!("{error:#}") }),
                    )?;
                }
            }
            let mut target = Target {
                task,
                session,
                asked,
                expects_recap: !blocked,
            };
            target.expects_recap &= target_alive(&target);
            targets.push(target);
        }
    }
    Ok(targets)
}

fn ask_for_recap(env: &Env<'_>, surface: &SurfaceId) -> Result<()> {
    env.host.send_text(surface, PARK_PROMPT)?;
    env.host.send_key(surface, Key::Enter)?;
    Ok(())
}

fn stop_fast_jobs(store: &mut Store, env: &Env<'_>) -> Result<Vec<ParkedJob>> {
    let mut out = Vec::new();
    for job in jobs::refresh(store, env.actor)? {
        if job.state == JobState::Running && job.park_policy == ParkPolicy::Rerun {
            jobs::stop(
                store,
                &job,
                "stopped for the park; it runs again after the resume",
                env.actor,
            )?;
            out.push(ParkedJob {
                label: job.label.clone(),
                outcome: if job.restart {
                    "stopped, will rerun".to_owned()
                } else {
                    "stopped, not restarted".to_owned()
                },
                left_running: false,
            });
        }
    }
    Ok(out)
}

fn checkpoint_fresh(job: &JobRow, since: SystemTime) -> bool {
    job.checkpoint_path
        .as_deref()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
        .is_some_and(|modified| modified >= since)
}

fn settle_jobs(
    store: &mut Store,
    env: &Env<'_>,
    began: SystemTime,
    done: &mut Vec<ParkedJob>,
) -> Result<bool> {
    let mut waiting = false;
    for job in jobs::refresh(store, env.actor)? {
        if job.state != JobState::Running {
            continue;
        }
        match job.park_policy {
            ParkPolicy::Checkpoint if checkpoint_fresh(&job, began) => {
                jobs::stop(store, &job, "stopped after a fresh checkpoint", env.actor)?;
                done.push(ParkedJob {
                    label: job.label.clone(),
                    outcome: "stopped at a fresh checkpoint".to_owned(),
                    left_running: false,
                });
            }
            ParkPolicy::Checkpoint | ParkPolicy::Finish => waiting = true,
            ParkPolicy::Rerun => {}
        }
    }
    Ok(waiting)
}

fn force_jobs(store: &mut Store, env: &Env<'_>, done: &mut Vec<ParkedJob>) -> Result<()> {
    for job in jobs::refresh(store, env.actor)? {
        if job.state != JobState::Running {
            continue;
        }
        match job.park_policy {
            ParkPolicy::Checkpoint => {
                jobs::stop(
                    store,
                    &job,
                    "stopped at the deadline without a fresh checkpoint; work since the last one is lost",
                    env.actor,
                )?;
                done.push(ParkedJob {
                    label: job.label.clone(),
                    outcome: "stopped at the deadline, work since its last checkpoint is lost"
                        .to_owned(),
                    left_running: false,
                });
            }
            ParkPolicy::Finish | ParkPolicy::Rerun => {
                let note = "left running at the park; a restart ends it";
                store.set_job_state(job.id, JobState::Running, Some(note), env.actor)?;
                done.push(ParkedJob {
                    label: job.label.clone(),
                    outcome: note.to_owned(),
                    left_running: true,
                });
            }
        }
    }
    Ok(())
}

fn finish_session(
    store: &mut Store,
    env: &Env<'_>,
    park: i64,
    target: &Target,
) -> Result<ParkedSession> {
    let task = store.task(target.task.id)?;
    let written = store.session_park_recap(park, task.id)?;
    let reason = if written.is_some() {
        "park"
    } else if target.expects_recap {
        "park, the session wrote no recap before the deadline"
    } else {
        "park, the session was waiting on the user"
    };
    let mut facts = recap::mechanical(store, env, &task, reason)?;
    for (question, options) in &target.asked {
        let _ = writeln!(
            facts,
            "- ask this again after the resume: {question} (options: {options})"
        );
    }
    let source = if written.is_some() {
        RecapSource::Session
    } else {
        store.add_park_recap(
            park,
            &NewRecap {
                task_id: Some(task.id),
                session_id: target.session.as_ref().map(|s| s.id.as_str()),
                kind: RecapKind::Mechanical,
                body: &facts,
            },
            env.actor,
        )?;
        RecapSource::Mechanical
    };
    let handoff = recap::write_handoff(
        env,
        &task,
        written.as_ref().map(|r| r.body.as_str()),
        &facts,
    )?;
    let snapshot = snapshot_worktree(store, env, &task)?;
    let problem = stop_session(store, env, task.id, task.shape, target.session.as_ref())?;
    store.transition(
        task.id,
        &Trigger::Parked(ParkedFacts {
            handoff_written: source == RecapSource::Session,
            deadline_forced: source == RecapSource::Mechanical,
        }),
        env.actor,
    )?;
    Ok(ParkedSession {
        name: task.name.to_string(),
        was: task.state_before_park.unwrap_or(task.state),
        recap: source,
        handoff: super::slashed(&handoff),
        snapshot,
        open_question: target.asked.first().map(|(question, _)| question.clone()),
        problem,
    })
}

fn snapshot_worktree(store: &mut Store, env: &Env<'_>, task: &Task) -> Result<Option<String>> {
    let dir = env.task_dir(task)?;
    if !dir.is_dir() {
        return Ok(None);
    }
    let Some(sha) = recap::git_text(&dir, &["stash", "create"]) else {
        return Ok(None);
    };
    let reference = format!("refs/headwork/park/{}", task.slug());
    drop(recap::git_text(&dir, &["update-ref", &reference, &sha]));
    if !ref_exists(&dir, &reference) {
        return Ok(None);
    }
    store.set_park_ref(task.id, &reference)?;
    Ok(Some(reference))
}

fn ref_exists(dir: &Path, reference: &str) -> bool {
    recap::git_text(dir, &["rev-parse", "--verify", "--quiet", reference]).is_some()
}

fn stop_session(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    shape: TaskShape<TaskId>,
    session: Option<&SessionRow>,
) -> Result<Option<String>> {
    let Some(session) = session else {
        return Ok(None);
    };
    let mut problem =
        (!close_pane(env, shape, session)?).then(|| "the pane is still open".to_owned());
    let records = session.processes();
    super::session_procs::end(&records)?.log(store, env.actor, Some(id), &session.id)?;
    let gone = wait_until(Duration::from_secs(10), || {
        !records.iter().any(super::session_procs::runs)
    });
    if !gone {
        problem = Some("processes of the session are still alive".to_owned());
    }
    store.set_session_state(&session.id, SessionState::Ended, env.actor)?;
    Ok(problem)
}

struct OrchestratorTarget {
    session: SessionRow,
    expects_recap: bool,
    problem: Option<String>,
}

/// Records the running orchestrator session on the park and asks it for its recap. It is not stopped here: it is
/// closed as the park's very last step ([`end_orchestrator`]), after every worker.
///
/// A recap it wrote in the turn that started the park (the `park` tool asks for one first) is that recap, and it is
/// not asked again. Otherwise it gets the park notice in its pane; when that cannot be done, the reason is logged
/// (`orchestrator-unasked`) and it gets a mechanical recap at the end.
fn begin_orchestrator(
    store: &mut Store,
    env: &Env<'_>,
    park: i64,
) -> Result<Option<OrchestratorTarget>> {
    let Some(session) = store.live_orchestrator_session()?.filter(session_alive) else {
        return begin_unmanaged_orchestrator(store, env, park);
    };
    store.set_park_orchestrator(park, &session.id, env.actor)?;
    if store
        .adopt_orchestrator_recap(park, &session.id, env.actor)?
        .is_some()
    {
        return Ok(Some(OrchestratorTarget {
            session,
            expects_recap: false,
            problem: None,
        }));
    }
    let surface = orchestrator_surface(env, &session);
    let problem = surface.as_ref().map_or_else(
        || Some("its pane is unknown, so it was not asked for a recap".to_owned()),
        |surface| {
            env.host
                .send_text(surface, ORCHESTRATOR_PARK_PROMPT)
                .and_then(|()| env.host.send_key(surface, Key::Enter))
                .err()
                .map(|error| format!("the park notice did not reach it: {error}"))
        },
    );
    let data = serde_json::json!({ "park": park, "surface": surface, "problem": problem });
    let kind = if problem.is_none() {
        "orchestrator-asked"
    } else {
        "orchestrator-unasked"
    };
    store.append_event(env.actor, None, Some(&session.id), kind, &data)?;
    Ok(Some(OrchestratorTarget {
        expects_recap: problem.is_none(),
        session,
        problem,
    }))
}

/// Records an orchestrator that runs outside headwork on the park, so the resume opens a new session for it from a mechanical recap.
/// It has no headwork tools, so it is not asked for a recap and gets a mechanical one.
fn begin_unmanaged_orchestrator(
    store: &mut Store,
    env: &Env<'_>,
    park: i64,
) -> Result<Option<OrchestratorTarget>> {
    let Some((session, surface)) = unmanaged_orchestrator(store, env)? else {
        return Ok(None);
    };
    store.set_park_orchestrator(park, &session.id, env.actor)?;
    store.append_event(
        env.actor,
        None,
        Some(&session.id),
        "orchestrator-unmanaged",
        &serde_json::json!({ "surface": surface, "during": "park" }),
    )?;
    Ok(Some(OrchestratorTarget {
        session,
        expects_recap: false,
        problem: Some(UNMANAGED_PROBLEM.to_owned()),
    }))
}

/// Why an orchestrator found running outside headwork leaves no recap of its own.
const UNMANAGED_PROBLEM: &str = "it runs outside headwork (restored by the host or started without `headwork \
    orchestrator`), so it has no `recap` tool; the resume opens a new session from a mechanical recap";

/// An orchestrator session the store knows that runs outside headwork: the host sees its Claude session in a surface,
/// but the store does not count it as running (the host reopened it after a restart with a plain `claude --resume`,
/// or it was started without `headwork orchestrator`). A host that does not answer counts as showing none.
///
/// # Errors
///
/// Fails when the store fails.
pub(super) fn unmanaged_orchestrator(
    store: &Store,
    env: &Env<'_>,
) -> Result<Option<(SessionRow, SurfaceId)>> {
    let Ok(states) = env.host.surface_states() else {
        return Ok(None);
    };
    for state in states {
        let Some(id) = state.session_id else {
            continue;
        };
        if let Some(session) = store.orchestrator_session(&id)?
            && !session_alive(&session)
        {
            return Ok(Some((session, state.surface)));
        }
    }
    Ok(None)
}

/// The surface the orchestrator runs in: where headwork placed it, else where the host sees its Claude session.
pub(super) fn orchestrator_surface(env: &Env<'_>, session: &SessionRow) -> Option<SurfaceId> {
    if let Some(surface) = &session.host_surface {
        return Some(SurfaceId(surface.clone()));
    }
    env.host
        .surface_states()
        .ok()?
        .into_iter()
        .find(|state| state.session_id.as_deref() == Some(session.id.as_str()))
        .map(|state| state.surface)
}

fn finish_orchestrator(
    store: &mut Store,
    env: &Env<'_>,
    park: i64,
    orchestrator: &OrchestratorTarget,
) -> Result<ParkedOrchestrator> {
    let recap = if store
        .orchestrator_park_recap(park, &orchestrator.session.id)?
        .is_some()
    {
        RecapSource::Session
    } else {
        let reason = match orchestrator.problem.as_deref() {
            _ if orchestrator.expects_recap => {
                "the orchestrator wrote no recap before the deadline".to_owned()
            }
            Some(UNMANAGED_PROBLEM) => {
                "the orchestrator ran outside headwork, so it could not write a recap".to_owned()
            }
            Some(problem) => format!("the orchestrator could not be asked for a recap: {problem}"),
            None => "the orchestrator could not be asked for a recap".to_owned(),
        };
        let body = recap::orchestrator_mechanical(store, &reason).unwrap_or_else(|error| {
            format!("the recap could not be built from the store: {error:#}")
        });
        let body = format!("{}{body}", super::digest::section(store, env.config));
        store.add_park_recap(
            park,
            &NewRecap {
                task_id: None,
                session_id: Some(&orchestrator.session.id),
                kind: RecapKind::Mechanical,
                body: &body,
            },
            env.actor,
        )?;
        RecapSource::Mechanical
    };
    Ok(ParkedOrchestrator {
        session: orchestrator.session.id.clone(),
        recap,
        problem: orchestrator.problem.clone(),
        end: OrchestratorEnd::Kept,
    })
}

/// The park's very last step: closes the orchestrator so Claude Code archives its Remote Control session, unless
/// `keep` says to leave it for the restart. When the park runs inside the orchestrator (in its pane, or as a process
/// its `claude` started, such as the `park` tool's detached run), closing it here would end the park halfway, so a
/// detached `headwork park --close-orchestrator` does it once this process has exited.
///
/// # Errors
///
/// Fails when the store fails.
fn end_orchestrator(
    store: &mut Store,
    env: &Env<'_>,
    target: &OrchestratorTarget,
    keep: bool,
) -> Result<OrchestratorEnd> {
    let id = &target.session.id;
    if keep {
        store.append_event(
            env.actor,
            None,
            Some(id),
            "orchestrator-kept",
            &serde_json::json!({ "reason": "--keep-orchestrator" }),
        )?;
        return Ok(OrchestratorEnd::Kept);
    }
    if !runs_inside(env, &target.session) {
        return close_orchestrator(store, env, id);
    }
    match start_closer(env, id) {
        Ok(pid) => {
            store.append_event(
                env.actor,
                None,
                Some(id),
                "detached",
                &serde_json::json!({
                    "command": "park --close-orchestrator",
                    "pid": pid,
                    "reason": "the park runs inside the orchestrator it closes",
                }),
            )?;
            Ok(OrchestratorEnd::Detached { pid })
        }
        Err(error) => {
            let end = OrchestratorEnd::Failed {
                problem: format!("the detached closer did not start: {error}"),
            };
            log_closed(store, env, id, &end)?;
            Ok(end)
        }
    }
}

/// Whether this process runs inside the orchestrator `session`: in its surface, or under one of its processes, so
/// ending the orchestrator would end it too.
fn runs_inside(env: &Env<'_>, session: &SessionRow) -> bool {
    let own_surface = super::resume::SURFACE_VARS
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|value| !value.is_empty()));
    if let Some(own) = own_surface
        && orchestrator_surface(env, session).is_some_and(|surface| surface.as_str() == own)
    {
        return true;
    }
    let roots: Vec<u32> = session
        .processes()
        .iter()
        .map(|record| record.pid)
        .collect();
    if roots.is_empty() {
        return false;
    }
    let (procs, _) = headwork_adapters::proc::snapshot();
    headwork_core::reap::descends_from(&procs, std::process::id(), &roots)
}

/// Starts `headwork park --close-orchestrator <session> --after <this pid>` detached and outside any surface.
fn start_closer(env: &Env<'_>, session: &str) -> Result<u32> {
    let mut process = std::process::Command::new(env.exe);
    process
        .args(["park", "--close-orchestrator", session, "--after"])
        .arg(std::process::id().to_string())
        .arg("--home")
        .arg(env.home.root())
        .current_dir(env.home.root());
    for var in super::resume::SURFACE_VARS {
        process.env_remove(var);
    }
    let (pid, _) = headwork_adapters::proc::spawn_detached(&mut process)?;
    Ok(pid)
}

/// How long the detached closer waits for the park that started it to exit.
pub const CLOSER_WAIT: Duration = Duration::from_secs(120);

/// The detached closer: closes the orchestrator `session` once the park process `after` is gone.
///
/// It waits up to [`CLOSER_WAIT`] first, because while the park runs, ending the orchestrator's process tree would
/// reach this process through it.
///
/// # Errors
///
/// Fails when the store fails.
pub fn close_orchestrator_after(
    store: &mut Store,
    env: &Env<'_>,
    session: &str,
    after: u32,
) -> Result<OrchestratorEnd> {
    wait_until(CLOSER_WAIT, || {
        !headwork_adapters::proc::is_alive(after).unwrap_or(false)
    });
    close_orchestrator(store, env, session)
}

/// Closes the orchestrator session `id` so Claude Code archives its Remote Control session.
///
/// It types `/exit` into its pane and waits up to `[watch] exit_grace_s` for it to leave, once plainly, so a running
/// turn ends first, then after Escape and a cleared line, so an open dialog does not take the Enter; then it ends what
/// is left of it and closes its pane. One running outside headwork has no recorded processes: it counts as gone when
/// the host no longer shows its Claude session, and closing its pane ends it.
///
/// # Errors
///
/// Fails when the store fails.
pub fn close_orchestrator(store: &mut Store, env: &Env<'_>, id: &str) -> Result<OrchestratorEnd> {
    let Some((session, _)) = store.session(id)? else {
        return Ok(OrchestratorEnd::Gone);
    };
    let managed = session_alive(&session);
    let surface = if managed {
        orchestrator_surface(env, &session)
    } else {
        unmanaged_surface(env, id)
    };
    if !managed && surface.is_none() {
        log_closed(store, env, id, &OrchestratorEnd::Gone)?;
        return Ok(OrchestratorEnd::Gone);
    }
    let records = session.processes();
    let gone = || {
        if managed {
            !records.iter().any(super::session_procs::runs)
        } else {
            unmanaged_surface(env, id).is_none()
        }
    };
    let left = match &surface {
        Some(surface) => ask_exit(store, env, id, surface, &gone)?,
        None => false,
    };
    let mut problem = None;
    if !left && managed {
        super::session_procs::end(&records)?.log(store, env.actor, None, id)?;
        if !wait_until(Duration::from_secs(10), &gone) {
            problem = Some("processes of the orchestrator are still alive".to_owned());
        }
    }
    if let Some(surface) = &surface
        && surface_shown(env, surface)
        && let Err(error) = env.host.close_surface(surface)
    {
        problem.get_or_insert_with(|| format!("its pane did not close: {error}"));
    }
    if managed {
        store.set_session_state(id, SessionState::Ended, env.actor)?;
    }
    let end = match problem {
        Some(problem) => OrchestratorEnd::Failed { problem },
        None if left => OrchestratorEnd::Exited,
        None => OrchestratorEnd::Ended,
    };
    log_closed(store, env, id, &end)?;
    Ok(end)
}

/// Whether the host still shows `surface` in any workspace or pane; a host that does not answer counts as showing it.
fn surface_shown(env: &Env<'_>, surface: &SurfaceId) -> bool {
    let Ok(workspaces) = env.host.workspaces() else {
        return true;
    };
    let in_workspace = workspaces.iter().any(|workspace| {
        env.host
            .surfaces(&workspace.id)
            .map_or(true, |surfaces| surfaces.contains(surface))
    });
    in_workspace
        || env.host.panes().map_or(true, |panes| {
            panes
                .iter()
                .any(|pane| &pane.surface == surface && pane.running)
        })
}

/// The surface where the host shows the Claude session `id`, for an orchestrator running outside headwork.
fn unmanaged_surface(env: &Env<'_>, id: &str) -> Option<SurfaceId> {
    env.host
        .surface_states()
        .ok()?
        .into_iter()
        .find(|state| state.session_id.as_deref() == Some(id))
        .map(|state| state.surface)
}

/// Types `/exit` into the orchestrator's `surface`, first plainly and then interrupting, each time waiting up to
/// `[watch] exit_grace_s` for `gone`. Returns whether it left; a grace of 0 asks nothing.
fn ask_exit(
    store: &Store,
    env: &Env<'_>,
    id: &str,
    surface: &SurfaceId,
    gone: &dyn Fn() -> bool,
) -> Result<bool> {
    let grace = env.config.watch.exit_grace_s;
    if grace == 0 {
        return Ok(false);
    }
    for (attempt, interrupt) in [(1_u32, false), (2, true)] {
        if let Err(error) = type_exit(env.host, surface, interrupt) {
            store.append_event(
                env.actor,
                None,
                Some(id),
                "exit-unasked",
                &serde_json::json!({ "error": error.to_string(), "attempt": attempt }),
            )?;
            return Ok(false);
        }
        store.append_event(
            env.actor,
            None,
            Some(id),
            "exit-asked",
            &serde_json::json!({ "grace_s": grace, "attempt": attempt, "interrupt": interrupt }),
        )?;
        if wait_until(Duration::from_secs(u64::from(grace)), gone) {
            return Ok(true);
        }
        store.append_event(
            env.actor,
            None,
            Some(id),
            "exit-ignored",
            &serde_json::json!({ "grace_s": grace, "attempt": attempt }),
        )?;
    }
    Ok(false)
}

fn log_closed(store: &Store, env: &Env<'_>, id: &str, end: &OrchestratorEnd) -> Result<()> {
    store.append_event(
        env.actor,
        None,
        Some(id),
        "orchestrator-closed",
        &serde_json::to_value(end)?,
    )?;
    Ok(())
}
