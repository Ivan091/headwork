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
use headwork_adapters::proc;
use headwork_core::config::ParkPolicy;
use headwork_core::lifecycle::{ParkedFacts, Trigger};
use headwork_core::{JobState, MachineMode, RecapKind, SessionState, TaskShape, TaskState};
use headwork_store::{JobRow, NewRecap, SessionRow, Store, Task, TaskId};
use serde::Serialize;

use super::close::{close_pane, wait_until};
use super::{Env, jobs, recap};

/// What a session is told when a park begins.
pub const PARK_PROMPT: &str = "headwork: the machine is about to restart. Reach a safe point now and call the \
    headwork `recap` tool with what you were doing, your next step and what is running. Register any long process \
    with `job_register`. Do not start new work.";

/// What the orchestrator is told when a park begins. It keeps running; the restart ends it and the resume reopens it.
pub const ORCHESTRATOR_PARK_PROMPT: &str = "headwork: the machine is about to restart. Call the headwork `recap` tool with \
    what you are coordinating, what waits on the user and your next step. Do not start new work. headwork reopens \
    you after the restart with this recap.";

/// How a park waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParkOptions {
    /// How long to wait for recaps and checkpoints before forcing what is left.
    pub deadline: Duration,
    /// How often to look.
    pub poll: Duration,
}

impl Default for ParkOptions {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(300),
            poll: Duration::from_secs(2),
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
}

/// What a park did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParkReport {
    /// The park's id.
    pub park: i64,
    /// The sessions, in the order they were parked.
    pub sessions: Vec<ParkedSession>,
    /// The orchestrator session, when one was running; it is left running.
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
    let orchestrator = orchestrator
        .map(|orchestrator| finish_orchestrator(store, env, record.id, orchestrator))
        .transpose()?;
    let safe =
        sessions.iter().all(|s| s.problem.is_none()) && parked_jobs.iter().all(|j| !j.left_running);
    store.finish_park(record.id, env.actor)?;
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
            .claude_pid
            .or(session.launcher_pid)
            .is_some_and(|pid| proc::is_alive(pid).unwrap_or(true))
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
    let problem = stop_session(store, env, task.shape, target.session.as_ref())?;
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
    shape: TaskShape<TaskId>,
    session: Option<&SessionRow>,
) -> Result<Option<String>> {
    let Some(session) = session else {
        return Ok(None);
    };
    let mut problem =
        (!close_pane(env, shape, session)?).then(|| "the pane is still open".to_owned());
    let pids: Vec<u32> = [session.claude_pid, session.launcher_pid]
        .into_iter()
        .flatten()
        .collect();
    for pid in &pids {
        if proc::is_alive(*pid)? {
            proc::kill_tree(*pid)?;
        }
    }
    let gone = wait_until(Duration::from_secs(10), || {
        pids.iter().all(|pid| !proc::is_alive(*pid).unwrap_or(true))
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

/// Records the running orchestrator session on the park and asks it for its recap. It is not stopped: the orchestrator may be the
/// session that asked for the park, and it tells the user when it is safe to restart.
fn begin_orchestrator(
    store: &mut Store,
    env: &Env<'_>,
    park: i64,
) -> Result<Option<OrchestratorTarget>> {
    let Some(session) = store.live_orchestrator_session()?.filter(session_alive) else {
        return begin_unmanaged_orchestrator(store, env, park);
    };
    store.set_park_orchestrator(park, &session.id, env.actor)?;
    let problem = orchestrator_surface(env, &session).map_or_else(
        || Some("its pane is unknown, so it was not asked for a recap".to_owned()),
        |surface| {
            env.host
                .send_text(&surface, ORCHESTRATOR_PARK_PROMPT)
                .and_then(|()| env.host.send_key(&surface, Key::Enter))
                .err()
                .map(|error| format!("the park notice did not reach it: {error}"))
        },
    );
    Ok(Some(OrchestratorTarget {
        expects_recap: problem.is_none(),
        session,
        problem,
    }))
}

/// Records an orchestrator that runs outside headwork on the park, so the resume reopens its conversation under headwork.
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
    orchestrator`), so it has no `recap` tool; the resume reopens its conversation under headwork";

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
    orchestrator: OrchestratorTarget,
) -> Result<ParkedOrchestrator> {
    let recap = if store
        .orchestrator_park_recap(park, &orchestrator.session.id)?
        .is_some()
    {
        RecapSource::Session
    } else {
        let reason = if orchestrator.expects_recap {
            "the orchestrator wrote no recap before the deadline"
        } else if orchestrator.problem.as_deref() == Some(UNMANAGED_PROBLEM) {
            "the orchestrator ran outside headwork, so it could not write a recap; its conversation holds where it stood"
        } else {
            "the orchestrator could not be asked for a recap"
        };
        let body = format!(
            "# Recap (mechanical)\n\n- {reason}\n- read the board and the inbox to see where the tasks stand\n"
        );
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
        session: orchestrator.session.id,
        recap,
        problem: orchestrator.problem,
    })
}
