//! Sessions that ended or vanished while their task still counts as running (design section 2.3: a live state →
//! `interrupted` when the process is gone without `done` or a park).
//!
//! The tick is not the only one who notices: the `SessionEnd` hook moves the task at once, and `board` reconciles
//! before it prints, so a task whose session ended never shows as running even when no tick is scheduled.

use anyhow::Result;
use headwork_adapters::proc;
use headwork_core::{MachineMode, RecapKind, SessionState, TaskState, Trigger};
use headwork_store::{SessionRow, Store, StoreError, TaskId};
use serde::Serialize;

use super::Env;

/// The states in which a task has a session that should be alive.
pub const RUNNING: [TaskState; 4] = [
    TaskState::Working,
    TaskState::NeedsUser,
    TaskState::InReview,
    TaskState::Merged,
];

/// Why a running task's session counts as lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LostCause {
    /// The store records the session as ended or gone.
    SessionEnded,
    /// The launcher process, or `claude` when no launcher was recorded, is not running.
    Launcher,
    /// The host pane is gone.
    Pane,
}

impl LostCause {
    /// The name in events and findings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionEnded => "session-ended",
            Self::Launcher => "launcher",
            Self::Pane => "pane",
        }
    }
}

/// What can be told about a session without asking the terminal host: its recorded state and its processes.
///
/// # Errors
///
/// Fails when a process cannot be queried.
pub fn local_cause(session: &SessionRow) -> Result<Option<LostCause>> {
    if matches!(session.state, SessionState::Ended | SessionState::Gone) {
        return Ok(Some(LostCause::SessionEnded));
    }
    if let Some(pid) = session.launcher_pid.or(session.claude_pid)
        && !proc::is_alive(pid)?
    {
        return Ok(Some(LostCause::Launcher));
    }
    Ok(None)
}

/// Moves a running task whose session is lost to `interrupted` and returns whether it moved.
///
/// It writes a mechanical recap for the resume and marks the session gone unless it already ended. A task that is not
/// running, or a machine that is parking, stays where it is.
///
/// # Errors
///
/// Fails when the store fails.
pub fn mark(
    store: &mut Store,
    env: &Env<'_>,
    id: TaskId,
    session: &SessionRow,
    cause: LostCause,
) -> Result<bool> {
    let machine = store.machine_mode()?;
    if machine == MachineMode::Parking || !RUNNING.contains(&store.task(id)?.state) {
        return Ok(false);
    }
    match store.transition(id, &Trigger::ProcessLost { machine }, env.actor) {
        Ok(_) => {}
        Err(StoreError::Refused { .. }) => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let task = store.task(id)?;
    let reason = match cause {
        LostCause::SessionEnded => "the session ended",
        LostCause::Launcher | LostCause::Pane => "the session was found gone",
    };
    super::recap::note(store, env, &task, RecapKind::Interrupted, reason);
    if session.state != SessionState::Ended {
        store.set_session_state(&session.id, SessionState::Gone, env.actor)?;
    }
    Ok(true)
}

/// Every running task whose session ended or whose launcher is gone moves to `interrupted`. Returns the tasks that
/// moved, with their former state and the cause.
///
/// # Errors
///
/// Fails when the store fails or a process cannot be queried.
pub fn reconcile(store: &mut Store, env: &Env<'_>) -> Result<Vec<Lost>> {
    let mut moved = Vec::new();
    for state in RUNNING {
        for id in store.tasks_in(state)? {
            let Some(session) = store.session_of(id)? else {
                continue;
            };
            let Some(cause) = local_cause(&session)? else {
                continue;
            };
            if mark(store, env, id, &session, cause)? {
                moved.push(Lost {
                    task: id,
                    from: state,
                    cause,
                });
            }
        }
    }
    Ok(moved)
}

/// A task [`reconcile`] moved to `interrupted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Lost {
    /// The task.
    pub task: TaskId,
    /// Its state before.
    pub from: TaskState,
    /// Why.
    pub cause: LostCause,
}
