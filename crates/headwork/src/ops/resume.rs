//! `resume`: bring parked sessions back after a restart (design section 6.2).
//!
//! Each parked (and, by hand, interrupted) task gets its pane again in its project's workspace, running
//! `headwork launch --fresh`: a new Claude session under the same name that starts from the task's brief and its
//! recap, never the old conversation, so the context stays small. The orchestrator comes back the same way. What a
//! session knew therefore travels in its recap (`park`, `recap`). Jobs that asked to be restarted run again, and a
//! summary for the orchestrator is written to `recaps/<park-id>.md`.
//!
//! Every session, the orchestrator too, comes back in the permission mode it was last in: the last `permissionMode`
//! its transcript records (the user may have switched it with shift+tab), else the one its hooks last reported, else
//! the task's (or `headwork orchestrator`'s) default.
//!
//! A lead comes back before its parts (`docs/leads.md`, section 4), so its workspace is open again when each part
//! becomes a tab in it, in the order the parts were made; a part whose lead did not come back waits for it.
//!
//! Resume is run by hand, or by whatever the user schedules at logon (`--logon`, which honours
//! `machine.resume_after_crash`); headwork installs nothing itself.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use headwork_adapters::host::{Target, TerminalHost};
use headwork_core::config::{CrashResume, ParkPolicy};
use headwork_core::lifecycle::Trigger;
use headwork_core::{JobState, MachineMode, PermissionMode, PlaceKind, RecapKind, Role, TaskState};
use headwork_store::{NewSession, Place, SessionRow, Store, Task};
use serde::Serialize;

use super::place::{self, Opening};
use super::{Env, jobs, park, spawn};

/// How a resume runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeOptions {
    /// Started by a logon task: interrupted sessions come back only as `machine.resume_after_crash` allows.
    pub logon: bool,
    /// How long to wait for the terminal host to answer.
    pub wait_for_host: Duration,
    /// Claude's config folder, where transcripts live.
    pub claude_dir: PathBuf,
}

/// What became of one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum TaskOutcome {
    /// A new session starts in a new pane from the brief and the recap.
    Fresh {
        /// The host surface.
        surface: String,
        /// Why it is new.
        reason: String,
    },
    /// Left alone, with the reason.
    Skipped {
        /// Why.
        reason: String,
    },
    /// The pane could not be opened; the task is `failed`.
    Failed {
        /// Why.
        error: String,
    },
}

/// One task of a resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResumedTask {
    /// `<project>-<slug>`.
    pub name: String,
    /// What happened.
    #[serde(flatten)]
    pub outcome: TaskOutcome,
    /// The question it had open, which it is told to ask again.
    pub open_question: Option<String>,
}

/// One job of a resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestartedJob {
    /// Its label.
    pub label: String,
    /// What happened.
    pub outcome: String,
}

/// What a resume did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResumeReport {
    /// The park it undoes, none after a crash.
    pub park: Option<i64>,
    /// The tasks.
    pub tasks: Vec<ResumedTask>,
    /// The jobs.
    pub jobs: Vec<RestartedJob>,
    /// The orchestrator session, when there was one to bring back or one still runs.
    pub orchestrator: Option<TaskOutcome>,
    /// The summary file for the orchestrator.
    pub summary: String,
    /// Tasks started from the queue afterwards.
    pub queue_started: usize,
}

/// The permission mode Claude's transcript of a session last records, when it has one headwork knows.
#[must_use]
pub fn transcript_mode(claude_dir: &Path, session: &str) -> Option<PermissionMode> {
    let file = format!("{session}.jsonl");
    std::fs::read_dir(claude_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|folder| folder.path().join(&file))
        .find(|path| path.is_file())
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| last_mode(&text))
}

/// The last top-level `permissionMode` among the transcript lines in `text`; none when there is none or the last one
/// is a mode headwork cannot start a session in.
#[must_use]
pub fn last_mode(text: &str) -> Option<PermissionMode> {
    text.lines()
        .rev()
        .filter(|line| line.contains("\"permissionMode\""))
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("permissionMode")?
                .as_str()
                .map(str::to_owned)
        })
        .and_then(|word| word.parse().ok())
}

/// The permission mode a session comes back in: its transcript's last, else the one its hooks last reported.
fn parked_mode(opts: &ResumeOptions, session: Option<&SessionRow>) -> Option<PermissionMode> {
    let session = session?;
    transcript_mode(&opts.claude_dir, &session.id).or(session.permission_mode)
}

/// Waits for the host to answer, up to `wait`.
fn wait_for_host(host: &dyn TerminalHost, wait: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        match host.ping() {
            Ok(()) => return Ok(()),
            Err(error) if start.elapsed() >= wait => {
                return Err(error)
                    .context("the terminal host does not answer; start it, then run resume again");
            }
            Err(_) => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

/// Resumes the machine.
///
/// # Errors
///
/// Fails when the machine is parking, the host does not answer, or the store fails. A task whose pane cannot be
/// opened is reported as failed and does not stop the rest.
pub fn resume(store: &mut Store, env: &Env<'_>, opts: &ResumeOptions) -> Result<ResumeReport> {
    match store.machine_mode()? {
        MachineMode::Parking => bail!("a park is still running; wait for it to finish"),
        MachineMode::Normal | MachineMode::Parked | MachineMode::Resuming => {}
    }
    wait_for_host(env.host, opts.wait_for_host)?;
    let park = store.current_park()?;
    let was_parked = store.machine_mode()? == MachineMode::Parked;
    store.set_machine_mode(MachineMode::Resuming, env.actor)?;
    let orchestrator = orchestrator_to_resume(store, env, park.as_ref().map(|p| p.id), was_parked)?;
    if let Some(OrchestratorPlan::Back {
        session,
        crash: true,
    }) = &orchestrator
        && store.live_orchestrator_session()?.is_none()
    {
        store.append_event(
            env.actor,
            None,
            Some(&session.id),
            "orchestrator-unmanaged",
            &serde_json::json!({ "during": "resume" }),
        )?;
    }
    close_restored_panes(
        store,
        env,
        orchestrator.as_ref().and_then(OrchestratorPlan::session),
    )?;

    let mut tasks = Vec::new();
    for (task, crash) in to_resume(store)? {
        if let Some(reason) = waiting_for_lead(store, &task)? {
            tasks.push(ResumedTask {
                name: task.name.to_string(),
                outcome: TaskOutcome::Skipped { reason },
                open_question: None,
            });
            continue;
        }
        if crash && opts.logon && env.config.machine.resume_after_crash != CrashResume::Always {
            tasks.push(ResumedTask {
                name: task.name.to_string(),
                outcome: TaskOutcome::Skipped {
                    reason: match env.config.machine.resume_after_crash {
                        CrashResume::Ask => {
                            "interrupted by a crash; the orchestrator asks whether to resume it"
                                .to_owned()
                        }
                        CrashResume::Never | CrashResume::Always => {
                            "interrupted by a crash; resume_after_crash is never".to_owned()
                        }
                    },
                },
                open_question: None,
            });
            continue;
        }
        tasks.push(resume_task(store, env, opts, &task, crash)?);
    }

    let jobs = restart_jobs(store, env)?;
    let summary = write_summary(
        store,
        env,
        park.as_ref().map(|p| p.id),
        was_parked,
        &tasks,
        &jobs,
        orchestrator.as_ref(),
    )?;
    let orchestrator =
        orchestrator.map(|plan| resume_orchestrator(store, env, opts, plan, &summary));
    store.finish_resume(
        park.as_ref().map(|p| p.id),
        &super::slashed(&summary),
        env.actor,
    )?;
    let pumped = spawn::pump(store, env)?;
    drop(env.host.notify(&format!(
            "headwork resumed {} session(s); summary at {}",
            tasks
                .iter()
                .filter(|t| matches!(
                    t.outcome,
                    TaskOutcome::Fresh { .. }
                ))
                .count(),
            super::slashed(&summary)
        )));
    Ok(ResumeReport {
        park: park.map(|p| p.id),
        tasks,
        jobs,
        orchestrator,
        summary: super::slashed(&summary),
        queue_started: pumped
            .iter()
            .filter(|p| matches!(p.outcome, spawn::Outcome::Started { .. }))
            .count(),
    })
}

/// The parked tasks, then the interrupted ones (`true`), each oldest first, with every part moved behind the leads
/// and plain tasks, grouped by its lead in the order the parts were made.
fn to_resume(store: &Store) -> Result<Vec<(Task, bool)>> {
    let mut ids: Vec<(headwork_store::TaskId, bool)> = store
        .tasks_by_age(TaskState::Parked)?
        .into_iter()
        .map(|id| (id, false))
        .collect();
    ids.extend(
        store
            .tasks_by_age(TaskState::Interrupted)?
            .into_iter()
            .map(|id| (id, true)),
    );
    let mut queue = Vec::with_capacity(ids.len());
    for (id, crash) in ids {
        queue.push((store.task(id)?, crash));
    }
    queue.sort_by_key(|(task, _)| task.shape.parent().map(|lead| (lead, task.id)));
    Ok(queue)
}

/// Why a part waits instead of resuming: its lead is not back.
fn waiting_for_lead(store: &Store, task: &Task) -> Result<Option<String>> {
    let Some(parent) = task.shape.parent() else {
        return Ok(None);
    };
    let lead = store.task(parent)?;
    Ok((!lead.state.takes_slot()).then(|| {
        format!(
            "its lead {} is {}, not back; the part resumes with its lead",
            lead.name, lead.state
        )
    }))
}

fn resume_task(
    store: &mut Store,
    env: &Env<'_>,
    opts: &ResumeOptions,
    task: &Task,
    crash: bool,
) -> Result<ResumedTask> {
    let name = task.name.to_string();
    let open_question = store.latest_recap(task.id)?.and_then(|recap| {
        recap.body.lines().find_map(|line| {
            line.strip_prefix("- ask this again after the resume: ")
                .map(str::to_owned)
        })
    });
    let dir = env.task_dir(task)?;
    if !dir.is_dir() {
        store.transition(
            task.id,
            &Trigger::Resume {
                machine: MachineMode::Resuming,
            },
            env.actor,
        )?;
        store.transition(task.id, &Trigger::LaunchFailed, env.actor)?;
        return Ok(ResumedTask {
            name,
            outcome: TaskOutcome::Failed {
                error: format!("the folder {} is gone", dir.display()),
            },
            open_question,
        });
    }
    store.transition(
        task.id,
        &Trigger::Resume {
            machine: MachineMode::Resuming,
        },
        env.actor,
    )?;
    let mode = parked_mode(opts, store.session_of(task.id)?.as_ref());
    let prepared =
        ensure_handoff(store, env, task, crash).and_then(|()| session_for(store, env, task));
    let (session_id, fresh_reason) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let error = format!("{error:#}");
            store.append_event(
                env.actor,
                Some(task.id),
                None,
                "launch-error",
                &serde_json::json!({ "error": error }),
            )?;
            store.transition(task.id, &Trigger::LaunchFailed, env.actor)?;
            return Ok(ResumedTask {
                name,
                outcome: TaskOutcome::Failed { error },
                open_question,
            });
        }
    };
    if let Some(mode) = mode {
        store.set_session_mode(&session_id, mode, env.actor)?;
    }
    let flags: &[&str] = if crash {
        &["--fresh", "--crash"]
    } else {
        &["--fresh"]
    };
    let opened = open_pane(store, env, task, &dir, &session_id, flags).or_else(|first| {
        store.append_event(
            env.actor,
            Some(task.id),
            Some(&session_id),
            "resume-retry",
            &serde_json::json!({ "error": format!("{first:#}") }),
        )?;
        open_pane(store, env, task, &dir, &session_id, flags)
    });
    Ok(match opened {
        Ok(surface) => ResumedTask {
            name,
            outcome: TaskOutcome::Fresh {
                surface,
                reason: fresh_reason,
            },
            open_question,
        },
        Err(error) => {
            let error = format!("{error:#}");
            store.session_ended(&session_id, None)?;
            store.append_event(
                env.actor,
                Some(task.id),
                Some(&session_id),
                "launch-error",
                &serde_json::json!({ "error": error }),
            )?;
            store.transition(task.id, &Trigger::LaunchFailed, env.actor)?;
            ResumedTask {
                name,
                outcome: TaskOutcome::Failed { error },
                open_question,
            }
        }
    })
}

/// Makes sure the file the new session reads holds what the old one knew. A park wrote it; after a crash (or when it is
/// missing) it is built now from the task's newest recap and the facts the store and the worktree still hold.
fn ensure_handoff(store: &Store, env: &Env<'_>, task: &Task, crash: bool) -> Result<()> {
    if !crash && env.home.handoff_path(&task.name).is_file() {
        return Ok(());
    }
    let own = store
        .latest_recap_of_kind(task.id, RecapKind::Session)?
        .map(|recap| {
            format!(
                "(written by the session before the restart, at {})

{}",
                recap.created_at, recap.body
            )
        });
    let facts = super::recap::mechanical(
        store,
        env,
        task,
        if crash {
            "resumed after a crash"
        } else {
            "resumed with no recap on file"
        },
    )?;
    super::recap::write_handoff(env, task, own.as_deref(), &facts)?;
    Ok(())
}

fn open_pane(
    store: &mut Store,
    env: &Env<'_>,
    task: &Task,
    dir: &Path,
    session: &str,
    flags: &[&str],
) -> Result<String> {
    let main = env.main_path(task)?;
    super::trust::before_start(store, env, task)?;
    let (workspace, fresh) = place::workspace_for(store, env, task, &main)?;
    let mut argv = vec![
        env.exe.display().to_string(),
        "launch".to_owned(),
        "--task".to_owned(),
        task.id.to_string(),
    ];
    argv.extend(flags.iter().map(|flag| (*flag).to_owned()));
    argv.extend(["--home".to_owned(), env.home.root().display().to_string()]);
    place::open(
        store,
        env,
        &Opening {
            task,
            session,
            workspace: &workspace,
            fresh,
            dir,
            argv: &argv,
        },
    )
}

fn restart_jobs(store: &mut Store, env: &Env<'_>) -> Result<Vec<RestartedJob>> {
    let mut parked = store.jobs_in(JobState::Parked, None)?;
    parked.sort_by_key(|job| (job.park_policy != ParkPolicy::Checkpoint, job.id));
    let mut out = Vec::new();
    for job in parked {
        if !job.restart {
            continue;
        }
        let outcome = match jobs::restart(store, &job, &env.shell(), env.actor) {
            Ok(pid) => format!("started again as process {pid}"),
            Err(error) => {
                let error = format!("{error:#}");
                store.set_job_state(job.id, JobState::Failed, Some(&error), env.actor)?;
                format!("could not start again: {error}")
            }
        };
        out.push(RestartedJob {
            label: job.label.clone(),
            outcome,
        });
    }
    Ok(out)
}

fn write_summary(
    store: &Store,
    env: &Env<'_>,
    park: Option<i64>,
    was_parked: bool,
    tasks: &[ResumedTask],
    jobs: &[RestartedJob],
    orchestrator: Option<&OrchestratorPlan>,
) -> Result<PathBuf> {
    let dir = env.home.recaps_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = park.map_or_else(
        || format!("crash-{}.md", chrono_free_stamp()),
        |id| format!("park-{id}.md"),
    );
    let path = dir.join(file);
    let mut text = String::new();
    let cause = if was_parked {
        "a park"
    } else {
        "a crash or a stop without a park"
    };
    let _ = writeln!(text, "# Resume after {cause}\n");
    let _ = writeln!(text, "## Sessions\n");
    if tasks.is_empty() {
        text.push_str("none were parked or interrupted\n");
    }
    for task in tasks {
        let line = match &task.outcome {
            TaskOutcome::Fresh { surface, reason } => {
                format!("new session in pane {surface} from its brief and recap: {reason}")
            }
            TaskOutcome::Skipped { reason } => format!("not resumed: {reason}"),
            TaskOutcome::Failed { error } => format!("could not be resumed: {error}"),
        };
        let _ = writeln!(text, "- {}: {line}", task.name);
        if let Some(question) = &task.open_question {
            let _ = writeln!(text, "  - waits on the user: {question}");
        }
    }
    let _ = writeln!(text, "\n## Jobs\n");
    if jobs.is_empty() {
        text.push_str("none were restarted\n");
    }
    for job in jobs {
        let _ = writeln!(text, "- {}: {}", job.label, job.outcome);
    }
    let lost: Vec<String> = store
        .jobs_in(JobState::Failed, None)?
        .into_iter()
        .chain(store.jobs_in(JobState::Running, None)?)
        .filter_map(|job| job.note.map(|note| format!("- {}: {note}", job.label)))
        .collect();
    if !lost.is_empty() {
        let _ = writeln!(text, "\n## Lost or left behind\n");
        for line in lost {
            let _ = writeln!(text, "{line}");
        }
    }
    if let Some(OrchestratorPlan::Back { session, .. }) = orchestrator {
        let _ = writeln!(text, "\n## Orchestrator\n");
        if let Some(recap) = store.latest_session_recap(&session.id)? {
            let _ = writeln!(text, "Your recap ({}, {}):\n", recap.kind, recap.created_at);
            let _ = writeln!(text, "{}", recap.body.trim_end());
        } else {
            let body = super::recap::orchestrator_mechanical(store, "no recap was written")
                .unwrap_or_else(|error| {
                    format!("the recap could not be built from the store: {error:#}")
                });
            text.push_str("You left no recap of your own; this one was built from the store:\n\n");
            text.push_str(body.trim_end());
            text.push('\n');
        }
    }
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// What becomes of the orchestrator session.
#[derive(Debug)]
enum OrchestratorPlan {
    /// Another orchestrator session still runs; it is left alone.
    StillRuns,
    /// This session comes back: the one the park found running, or one a crash cut off.
    Back {
        /// The session.
        session: Box<SessionRow>,
        /// It was cut off by a crash, not parked.
        crash: bool,
    },
}

impl OrchestratorPlan {
    const fn session(&self) -> Option<&str> {
        match self {
            Self::StillRuns => None,
            Self::Back { session, .. } => Some(session.id.as_str()),
        }
    }
}

/// The orchestrator session to bring back. After a park it is the one the park recorded; otherwise an orchestrator the store still
/// counts as running whose processes are gone, else one the store knows that the host shows running outside headwork (wmux
/// reopens Claude panes after a reboot with a plain `claude --resume <id>`), which comes back under headwork. The
/// orchestrator comes back at logon too, whatever `resume_after_crash` says: it is the session that asks about the rest.
fn orchestrator_to_resume(
    store: &Store,
    env: &Env<'_>,
    park: Option<i64>,
    was_parked: bool,
) -> Result<Option<OrchestratorPlan>> {
    let live = store.live_orchestrator_session()?;
    if live.as_ref().is_some_and(park::session_alive) {
        return Ok(Some(OrchestratorPlan::StillRuns));
    }
    let parked = match park.filter(|_| was_parked) {
        Some(id) => store.park_orchestrator(id)?,
        None => None,
    };
    if let Some(id) = parked {
        return Ok(store
            .session(&id)?
            .map(|(session, _)| OrchestratorPlan::Back {
                session: Box::new(session),
                crash: false,
            }));
    }
    let session = match live {
        Some(session) => Some(session),
        None => park::unmanaged_orchestrator(store, env)?.map(|(session, _)| session),
    };
    Ok(session.map(|session| OrchestratorPlan::Back {
        session: Box::new(session),
        crash: true,
    }))
}

fn resume_orchestrator(
    store: &mut Store,
    env: &Env<'_>,
    opts: &ResumeOptions,
    plan: OrchestratorPlan,
    summary: &Path,
) -> TaskOutcome {
    let OrchestratorPlan::Back { session, crash } = plan else {
        return TaskOutcome::Skipped {
            reason: "an orchestrator session still runs".to_owned(),
        };
    };
    match open_orchestrator(store, env, opts, &session, summary) {
        Ok(outcome) => outcome,
        Err(error) => {
            let error = format!("{error:#}");
            drop(store.append_event(
                env.actor,
                None,
                Some(&session.id),
                "orchestrator-resume-failed",
                &serde_json::json!({ "error": error, "crash": crash }),
            ));
            TaskOutcome::Failed { error }
        }
    }
}

/// Opens the orchestrator's pane in the `orchestrator` workspace, in the folder it ran in, as a new orchestrator session
/// that starts from the recap in the summary.
fn open_orchestrator(
    store: &mut Store,
    env: &Env<'_>,
    opts: &ResumeOptions,
    old: &SessionRow,
    summary: &Path,
) -> Result<TaskOutcome> {
    let dir = store.session_dir(&old.id)?.with_context(|| {
        format!(
            "the folder orchestrator session {} ran in is unknown (it was started before headwork recorded it); start the \
             orchestrator with `headwork orchestrator`",
            old.id
        )
    })?;
    let dir = PathBuf::from(dir);
    let mode = parked_mode(opts, Some(old));
    let session = uuid::Uuid::new_v4().to_string();
    store.record_session(
        &NewSession {
            id: session.clone(),
            task_id: None,
            role: Role::Orchestrator,
            rc_name: old.rc_name.clone(),
            host: env.config.machine.host.as_str().to_owned(),
            host_workspace: None,
            host_surface: None,
        },
        env.actor,
    )?;
    store.set_session_dir(&session, &super::slashed(&dir))?;
    let mut argv = vec![
        env.exe.display().to_string(),
        "orchestrator".to_owned(),
        "--session".to_owned(),
        session.clone(),
    ];
    if let Some(mode) = mode {
        argv.extend(["--mode".to_owned(), mode.as_str().to_owned()]);
    }
    argv.extend([
        "--summary".to_owned(),
        super::slashed(summary),
        "--home".to_owned(),
        env.home.root().display().to_string(),
    ]);
    let opened = env
        .host
        .ensure_workspace(ORCHESTRATOR_WORKSPACE, &dir)
        .map_err(anyhow::Error::from)
        .and_then(|(workspace, fresh)| {
            let target = place::tab(env.host, &workspace, fresh)?;
            let pane = env
                .host
                .spawn(&workspace, &dir, ORCHESTRATOR_WORKSPACE, &argv, &target)?;
            Ok((workspace, target, pane))
        });
    let (workspace, target, pane) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            store.session_ended(&session, None)?;
            return Err(error);
        }
    };
    let pane_id = match &target {
        Target::Pane { pane, .. } => Some(pane.as_str()),
        Target::Tab => None,
    };
    store.set_session_place(
        &session,
        &Place {
            workspace: workspace.as_str(),
            surface: pane.surface.as_str(),
            pane: pane_id,
            kind: Some(if pane_id.is_some() {
                PlaceKind::Pane
            } else {
                PlaceKind::Tab
            }),
        },
        env.actor,
    )?;
    store.session_ended(&old.id, None)?;
    Ok(TaskOutcome::Fresh {
        surface: pane.surface.to_string(),
        reason: FRESH_REASON.to_owned(),
    })
}

/// The workspace, and the pane label, a resumed orchestrator opens in.
pub const ORCHESTRATOR_WORKSPACE: &str = "orchestrator";

fn chrono_free_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Why every resumed session is a new one.
pub const FRESH_REASON: &str =
    "a resume always opens a new session so its context is fresh; the recap carries what it knew";

/// The new session a task comes back as, and why it is new.
fn session_for(store: &mut Store, env: &Env<'_>, task: &Task) -> Result<(String, String)> {
    let id = uuid::Uuid::new_v4().to_string();
    store.record_session(
        &NewSession {
            id: id.clone(),
            task_id: Some(task.id),
            role: Role::Worker,
            rc_name: task.name.to_string(),
            host: env.config.machine.host.as_str().to_owned(),
            host_workspace: None,
            host_surface: None,
        },
        env.actor,
    )?;
    Ok((id, FRESH_REASON.to_owned()))
}

/// Whether a resume started now would close the host surface `surface`.
///
/// It does when the surface runs a session the resume brings back, such as an orchestrator the host restored outside
/// headwork that runs the resume itself. Such a resume must run detached, or closing that surface ends it halfway.
///
/// # Errors
///
/// Fails when the store fails.
pub fn would_close(store: &Store, env: &Env<'_>, surface: &str) -> Result<bool> {
    let park = store.current_park()?;
    let was_parked = store.machine_mode()? == MachineMode::Parked;
    let orchestrator = orchestrator_to_resume(store, env, park.map(|p| p.id), was_parked)?;
    let ids = resumed_sessions(
        store,
        orchestrator.as_ref().and_then(OrchestratorPlan::session),
    )?;
    let Ok(states) = env.host.surface_states() else {
        return Ok(false);
    };
    Ok(states.into_iter().any(|state| {
        state.surface.as_str() == surface && state.session_id.is_some_and(|id| ids.contains(&id))
    }))
}

/// The environment variables in which a terminal host tells a program the surface it runs in.
pub const SURFACE_VARS: [&str; 2] = ["WMUX_SURFACE_ID", "CMUX_SURFACE_ID"];

/// Starts `headwork resume` again as a detached process with no surface of its own, so it outlives the surface it was
/// started in when it closes that surface. Returns its process id.
///
/// # Errors
///
/// Fails when the process does not start or the store fails.
pub fn start_detached(
    store: &Store,
    env: &Env<'_>,
    logon: bool,
    wait_for_host: Duration,
) -> Result<u32> {
    let mut process = std::process::Command::new(env.exe);
    process.arg("resume");
    if logon {
        process.arg("--logon");
    }
    process
        .args(["--wait", &wait_for_host.as_secs().to_string()])
        .arg("--home")
        .arg(env.home.root())
        .current_dir(env.home.root());
    for var in SURFACE_VARS {
        process.env_remove(var);
    }
    let (pid, _) = headwork_adapters::proc::spawn_detached(&mut process)?;
    store.append_event(
        env.actor,
        None,
        None,
        "detached",
        &serde_json::json!({ "command": "resume", "pid": pid, "reason": "it closes the surface it was started in" }),
    )?;
    Ok(pid)
}

/// The Claude sessions a resume brings back: the orchestrator's and those of the parked and interrupted tasks.
fn resumed_sessions(store: &Store, orchestrator: Option<&str>) -> Result<Vec<String>> {
    let mut ids: Vec<String> = orchestrator.map(str::to_owned).into_iter().collect();
    for state in [TaskState::Parked, TaskState::Interrupted] {
        for id in store.tasks_in(state)? {
            ids.extend(store.session_of(id)?.map(|session| session.id));
        }
    }
    Ok(ids)
}

/// Closes the panes the host restored by itself for sessions that are about to be resumed here, so no conversation
/// runs twice. After a reboot wmux reopens each Claude pane with a plain `claude --resume <id>`.
fn close_restored_panes(store: &Store, env: &Env<'_>, orchestrator: Option<&str>) -> Result<()> {
    let ids = resumed_sessions(store, orchestrator)?;
    let states = match env.host.surface_states() {
        Ok(states) => states,
        Err(error) => {
            store.append_event(
                env.actor,
                None,
                None,
                "restored-panes-unchecked",
                &serde_json::json!({ "error": error.to_string() }),
            )?;
            return Ok(());
        }
    };
    for state in states {
        let Some(session) = state.session_id.filter(|id| ids.contains(id)) else {
            continue;
        };
        let closed = env.host.close_surface(&state.surface);
        store.append_event(
            env.actor,
            None,
            Some(&session),
            "restored-pane-closed",
            &serde_json::json!({
                "surface": state.surface,
                "error": closed.err().map(|error| error.to_string()),
            }),
        )?;
    }
    Ok(())
}
