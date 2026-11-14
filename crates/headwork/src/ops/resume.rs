//! `resume`: bring parked sessions back after a restart (design section 6.2).
//!
//! Each parked (and, by hand, interrupted) task gets its pane again in its project's workspace, running
//! `headwork launch --resume`, which continues the same Claude session under the same name and hands it its recap. A
//! session whose conversation is gone starts fresh from its brief and recap. Jobs that asked to be restarted run
//! again, and a summary for the root is written to `recaps/<park-id>.md`.
//!
//! Resume is run by hand, or by whatever the user schedules at logon (`--logon`, which honours
//! `machine.resume_after_crash`); headwork installs nothing itself.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use headwork_adapters::host::TerminalHost;
use headwork_core::config::{CrashResume, ParkPolicy};
use headwork_core::lifecycle::Trigger;
use headwork_core::{JobState, MachineMode, Role, TaskState};
use headwork_store::{NewSession, Store, Task};
use serde::Serialize;

use super::place::{self, Opening};
use super::{Env, jobs, spawn};

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
    /// The conversation continues in a new pane.
    Resumed {
        /// The host surface.
        surface: String,
    },
    /// The conversation could not be restored; a new session starts from the brief and recap.
    Fresh {
        /// The host surface.
        surface: String,
        /// Why.
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
    /// The summary file for the root.
    pub summary: String,
    /// Tasks started from the queue afterwards.
    pub queue_started: usize,
}

/// Whether Claude's transcript of a session exists under any project folder of `claude_dir`.
#[must_use]
pub fn transcript_exists(claude_dir: &Path, session: &str) -> bool {
    let file = format!("{session}.jsonl");
    std::fs::read_dir(claude_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|folder| folder.path().join(&file).is_file())
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
    close_restored_panes(store, env)?;

    let mut tasks = Vec::new();
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
    for (id, crash) in ids {
        let task = store.task(id)?;
        if crash && opts.logon && env.config.machine.resume_after_crash != CrashResume::Always {
            tasks.push(ResumedTask {
                name: task.name.to_string(),
                outcome: TaskOutcome::Skipped {
                    reason: match env.config.machine.resume_after_crash {
                        CrashResume::Ask => {
                            "interrupted by a crash; the root asks whether to resume it".to_owned()
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
    )?;
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
                    TaskOutcome::Resumed { .. } | TaskOutcome::Fresh { .. }
                ))
                .count(),
            super::slashed(&summary)
        )));
    Ok(ResumeReport {
        park: park.map(|p| p.id),
        tasks,
        jobs,
        summary: super::slashed(&summary),
        queue_started: pumped
            .iter()
            .filter(|p| matches!(p.outcome, spawn::Outcome::Started { .. }))
            .count(),
    })
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
    let (session_id, fresh_reason) = session_for(store, env, opts, task)?;
    let flags: &[&str] = match (&fresh_reason, crash) {
        (None, false) => &["--resume"],
        (None, true) => &["--resume", "--crash"],
        (Some(_), false) => &["--fresh"],
        (Some(_), true) => &["--fresh", "--crash"],
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
            outcome: match fresh_reason {
                None => TaskOutcome::Resumed { surface },
                Some(reason) => TaskOutcome::Fresh { surface, reason },
            },
            open_question,
        },
        Err(error) => {
            let error = format!("{error:#}");
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

fn open_pane(
    store: &mut Store,
    env: &Env<'_>,
    task: &Task,
    dir: &Path,
    session: &str,
    flags: &[&str],
) -> Result<String> {
    let main = env.main_path(task)?;
    let (workspace, fresh) = env.host.ensure_workspace(task.project().as_str(), &main)?;
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
        let outcome = match jobs::restart(store, &job, env.actor) {
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
            TaskOutcome::Resumed { surface } => format!("resumed in pane {surface}"),
            TaskOutcome::Fresh { surface, reason } => {
                format!("started fresh in pane {surface} from its brief and recap: {reason}")
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
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

fn chrono_free_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The session a task comes back as: its own when Claude still has the transcript, else a new one and the reason.
fn session_for(
    store: &mut Store,
    env: &Env<'_>,
    opts: &ResumeOptions,
    task: &Task,
) -> Result<(String, Option<String>)> {
    match store.session_of(task.id)? {
        Some(old) if transcript_exists(&opts.claude_dir, &old.id) => {
            store.session_resumed(&old.id, env.actor)?;
            Ok((old.id, None))
        }
        other => {
            let reason = match other {
                Some(old) => format!("the transcript of session {} is gone", old.id),
                None => "the task had no session".to_owned(),
            };
            let id = uuid::Uuid::new_v4().to_string();
            store.record_session(
                &NewSession {
                    id: id.clone(),
                    task_id: Some(task.id),
                    role: Role::Child,
                    rc_name: task.name.to_string(),
                    host: env.config.machine.host.as_str().to_owned(),
                    host_workspace: None,
                    host_surface: None,
                },
                env.actor,
            )?;
            Ok((id, Some(reason)))
        }
    }
}

/// Closes the panes the host restored by itself for sessions that are about to be resumed here, so no conversation
/// runs twice. After a reboot wmux reopens each Claude pane with a plain `claude --resume <id>`.
fn close_restored_panes(store: &Store, env: &Env<'_>) -> Result<()> {
    let mut ids = Vec::new();
    for state in [TaskState::Parked, TaskState::Interrupted] {
        for id in store.tasks_in(state)? {
            ids.extend(store.session_of(id)?.map(|session| session.id));
        }
    }
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
