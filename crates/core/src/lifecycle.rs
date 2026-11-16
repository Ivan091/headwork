//! The task lifecycle: every transition of design section 2.3 with its guard.
//!
//! [`apply`] is the only way a task changes state. It takes the task as it is and a [`Trigger`] carrying the facts
//! the guard needs, and returns the [`Step`] to record or the [`Refusal`] that explains why not. It has no I/O; the
//! store gathers the facts and writes the result.

use crate::vocabulary::{MachineMode, TaskState};

/// The parts of a task the lifecycle reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskSnapshot {
    /// The current state.
    pub state: TaskState,
    /// The state to return to after a park or an interruption.
    pub state_before_park: Option<TaskState>,
    /// A standing role that never closes when idle.
    pub keep: bool,
}

/// The state of the task another task depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dependency {
    /// No dependency.
    None,
    /// The dependency is merged.
    Merged,
    /// The dependency exists and is not merged yet.
    Waiting,
}

/// Whether a Fable task may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FableGate {
    /// The task does not run Fable, or the project lets Fable run without asking.
    NotNeeded,
    /// The user allowed it.
    Allowed,
    /// The approval is not answered, or was refused.
    Waiting,
}

/// Facts for `queued` → `spawning`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartFacts {
    /// Tasks on the machine that take a worker slot now.
    pub live_workers: u32,
    /// The machine's `max_workers`.
    pub max_workers: u32,
    /// Tasks of the same project that take a slot now.
    pub project_live: u32,
    /// The project's own cap, if it has one.
    pub project_cap: Option<u32>,
    /// The task's kind is main-only and another live task of the project already runs that kind.
    pub main_only_kind_busy: bool,
    /// The dependency's state.
    pub dependency: Dependency,
    /// The Fable approval.
    pub fable: FableGate,
    /// The machine mode.
    pub machine: MachineMode,
}

/// Facts for `spawning` → `working`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionFacts {
    /// The hook's session id is the one the task was launched with.
    pub session_matches: bool,
    /// The launcher process is alive.
    pub launcher_alive: bool,
}

/// Who reports a merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeSource {
    /// The tick, from the git host.
    GitHost,
    /// The worker, after its delivery command merged.
    Worker,
}

/// How a project's pull requests get merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergePolicy {
    /// The project's delivery command merges; the worker may report it.
    Deliver,
    /// A person merges; only the git host can report it.
    Human,
}

/// Why a merged task starts closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseCause {
    /// The worker called `done`.
    Done,
    /// The tick, ten minutes after the merge.
    GraceElapsed,
}

/// Facts for `merged` → `closing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseFacts {
    /// What started the close.
    pub cause: CloseCause,
    /// An ask of the task still waits on the user.
    pub open_ask: bool,
    /// A job of the task still runs.
    pub running_job: bool,
    /// The worker sent its report to the orchestrator.
    pub report_sent: bool,
}

/// Where the task's pull request stands when its worker finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrEnd {
    /// The task has no pull request: a lookup, a report, work in the main checkout.
    None,
    /// The pull request is open.
    Open,
    /// The pull request is merged.
    Merged,
    /// The pull request was closed without a merge.
    Closed,
}

/// Facts for `working` → `closing` when the worker finishes without a merged pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinishFacts {
    /// An ask of the task still waits on the user.
    pub open_ask: bool,
    /// A job of the task still runs.
    pub running_job: bool,
    /// The worker sent its report to the orchestrator.
    pub report_sent: bool,
    /// The task's pull request.
    pub pr: PrEnd,
}

/// What happened to a task's worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeEnd {
    /// The task ran in the main checkout and has no worktree.
    NoWorktree,
    /// Removed by the project's command.
    Removed,
    /// Kept by the user's answer.
    Kept,
    /// Still there.
    Present,
}

/// Facts for `closing` → `closed`, verified by the tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupFacts {
    /// The host pane is gone.
    pub pane_gone: bool,
    /// The launcher and all its descendants are gone.
    pub processes_gone: bool,
    /// The worktree's end.
    pub worktree: WorktreeEnd,
}

/// Facts for `parking` → `parked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParkedFacts {
    /// The session wrote its handoff and its pane is closed.
    pub handoff_written: bool,
    /// The park deadline passed and the tool forced it.
    pub deadline_forced: bool,
}

/// An event that may move a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A slot is free: `queued` → `spawning`.
    Start(StartFacts),
    /// `SessionStart` arrived: `spawning` → `working`, or back to the state before a park.
    SessionStarted(SessionFacts),
    /// No `SessionStart` within 120 s: `spawning` → `failed`.
    SpawnTimedOut,
    /// The pane or `claude` could not be started, or `claude` exited before its session started: `spawning` →
    /// `failed`.
    LaunchFailed,
    /// `AskUserQuestion` or a permission prompt: `working` → `needs_user`.
    AskOpened,
    /// The answer is recorded: `needs_user` → `working`.
    AskAnswered,
    /// A review is reported or a pull request opened: `working` → `in_review`.
    ReviewOpened {
        /// The pull request's repository is in the project's `allowed_repos`.
        repo_allowed: bool,
    },
    /// The user asked for changes or the pull request closed unmerged: `in_review` → `working`.
    ChangesRequested,
    /// The pull request is merged: `in_review` → `merged`.
    Merged {
        /// Who reports it.
        source: MergeSource,
        /// The project's merge policy.
        policy: MergePolicy,
    },
    /// `merged` → `closing`.
    Close(CloseFacts),
    /// The worker finishes a task that has no merged pull request to close through: `working` → `closing`.
    Finish(FinishFacts),
    /// The orchestrator or the user drops the task: any state that is not closing or over → `dropped`.
    Drop,
    /// `dropped` → `closing`, at once.
    BeginClosing,
    /// The orchestrator or the user closes a task without dropping it, such as a keep task or one stuck after `done`: any
    /// state that is not closing or over → `closing`.
    ForceClose,
    /// Pane, processes and worktree are verified gone: `closing` → `closed`.
    Cleaned(CleanupFacts),
    /// The launcher or claude process vanished: a running state → `interrupted`.
    ProcessLost {
        /// The machine mode.
        machine: MachineMode,
    },
    /// A park started: `working`, `needs_user` or `in_review` → `parking`.
    Park,
    /// `parking` → `parked`.
    Parked(ParkedFacts),
    /// `parked` or `interrupted` → `spawning`.
    Resume {
        /// The machine mode.
        machine: MachineMode,
    },
}

impl Trigger {
    /// A short name for logs and refusals.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Start(_) => "start",
            Self::SessionStarted(_) => "session-started",
            Self::SpawnTimedOut => "spawn-timed-out",
            Self::LaunchFailed => "launch-failed",
            Self::AskOpened => "ask-opened",
            Self::AskAnswered => "ask-answered",
            Self::ReviewOpened { .. } => "review-opened",
            Self::ChangesRequested => "changes-requested",
            Self::Merged { .. } => "merged",
            Self::Close(_) => "close",
            Self::Finish(_) => "finish",
            Self::Drop => "drop",
            Self::BeginClosing => "begin-closing",
            Self::ForceClose => "force-close",
            Self::Cleaned(_) => "cleaned",
            Self::ProcessLost { .. } => "process-lost",
            Self::Park => "park",
            Self::Parked(_) => "parked",
            Self::Resume { .. } => "resume",
        }
    }
}

/// A transition to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// The state before.
    pub from: TaskState,
    /// The state after.
    pub to: TaskState,
    /// The new value of `state_before_park`.
    pub state_before_park: Option<TaskState>,
}

/// Why a transition was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The trigger does not apply to the task's state.
    #[error("`{trigger}` does not apply to a task in state `{state}`")]
    WrongState {
        /// The trigger's name.
        trigger: &'static str,
        /// The task's state.
        state: TaskState,
    },
    /// All worker slots are taken.
    #[error("all {max} worker slots are taken")]
    NoFreeSlot {
        /// The machine's cap.
        max: u32,
    },
    /// The project's slots are taken.
    #[error("the project's {cap} slots are taken")]
    ProjectCapReached {
        /// The project's cap.
        cap: u32,
    },
    /// Another task of the same main-only kind runs in the project.
    #[error("another task of this main-only kind already runs in the project's main checkout")]
    MainOnlyKindBusy,
    /// The dependency is not merged.
    #[error("the task it depends on is not merged yet")]
    DependencyNotMerged,
    /// The user has not allowed Fable.
    #[error("the user has not allowed Fable for this task")]
    FableNotAllowed,
    /// The machine mode forbids it.
    #[error("the machine is `{0}`")]
    MachineMode(MachineMode),
    /// The session id is not the task's.
    #[error("the session id is not the one the task was launched with")]
    SessionMismatch,
    /// The launcher is gone.
    #[error("the launcher is not running")]
    LauncherDead,
    /// The pull request's repository is not allowed.
    #[error("the pull request's repository is not in the project's allowed_repos")]
    RepoNotAllowed,
    /// The project merges by hand; only the git host may report a merge.
    #[error("the project merges by hand; only the git host can mark the task merged")]
    HumanMergeOnly,
    /// Keep tasks do not close on their own.
    #[error("a keep task closes only when the orchestrator closes it")]
    KeepTask,
    /// An ask waits on the user.
    #[error("an ask of the task still waits on the user")]
    OpenAsk,
    /// A job still runs.
    #[error("a job of the task still runs")]
    RunningJob,
    /// The report to the orchestrator is missing.
    #[error("the task has not sent its report to the orchestrator")]
    ReportMissing,
    /// The task's pull request is still open.
    #[error("the task's pull request is still open: merge it, or have it closed, before finishing")]
    PrOpen,
    /// The task's pull request is merged, so it finishes through `in_review` and `merged`.
    #[error("the task's pull request is merged: the task closes through in_review and merged")]
    PrMerged,
    /// The pane is still open.
    #[error("the host pane is still open")]
    PaneOpen,
    /// Processes of the task are alive.
    #[error("processes of the task are still alive")]
    ProcessesAlive,
    /// The worktree is still there.
    #[error("the worktree is neither removed nor kept")]
    WorktreePresent,
    /// Neither a handoff nor a forced deadline.
    #[error("no handoff yet and the park deadline has not forced it")]
    NoHandoff,
}

impl Trigger {
    /// Whether the trigger applies to a task in `state` at all, before its guard is checked.
    #[must_use]
    pub const fn applies_to(&self, state: TaskState) -> bool {
        use TaskState as S;
        match self {
            Self::Start(_) => matches!(state, S::Queued),
            Self::SessionStarted(_) | Self::SpawnTimedOut | Self::LaunchFailed => {
                matches!(state, S::Spawning)
            }
            Self::AskOpened | Self::ReviewOpened { .. } | Self::Finish(_) => {
                matches!(state, S::Working)
            }
            Self::AskAnswered => matches!(state, S::NeedsUser),
            Self::ChangesRequested | Self::Merged { .. } => matches!(state, S::InReview),
            Self::Close(_) => matches!(state, S::Merged),
            Self::Drop | Self::ForceClose => droppable(state),
            Self::BeginClosing => matches!(state, S::Dropped),
            Self::Cleaned(_) => matches!(state, S::Closing),
            Self::ProcessLost { .. } => {
                matches!(state, S::Working | S::NeedsUser | S::InReview | S::Merged)
            }
            Self::Park => matches!(state, S::Working | S::NeedsUser | S::InReview),
            Self::Parked(_) => matches!(state, S::Parking),
            Self::Resume { .. } => matches!(state, S::Parked | S::Interrupted),
        }
    }

    /// Checks the trigger's guard against its facts and the task.
    ///
    /// # Errors
    ///
    /// Returns the first failing check as a [`Refusal`].
    pub const fn guard(&self, task: TaskSnapshot) -> Result<(), Refusal> {
        match *self {
            Self::Start(facts) => check_start(&facts),
            Self::SessionStarted(facts) => {
                if !facts.session_matches {
                    Err(Refusal::SessionMismatch)
                } else if !facts.launcher_alive {
                    Err(Refusal::LauncherDead)
                } else {
                    Ok(())
                }
            }
            Self::ReviewOpened {
                repo_allowed: false,
            } => Err(Refusal::RepoNotAllowed),
            Self::Merged {
                source: MergeSource::Worker,
                policy: MergePolicy::Human,
            } => Err(Refusal::HumanMergeOnly),
            Self::Close(facts) => check_close(task.keep, facts),
            Self::Finish(facts) => check_finish(task.keep, facts),
            Self::Cleaned(facts) => check_cleanup(facts),
            Self::ProcessLost {
                machine: MachineMode::Parking,
            } => Err(Refusal::MachineMode(MachineMode::Parking)),
            Self::Parked(ParkedFacts {
                handoff_written: false,
                deadline_forced: false,
            }) => Err(Refusal::NoHandoff),
            Self::Resume {
                machine: machine @ (MachineMode::Parking | MachineMode::Parked),
            } => Err(Refusal::MachineMode(machine)),
            Self::SpawnTimedOut
            | Self::LaunchFailed
            | Self::ForceClose
            | Self::AskOpened
            | Self::AskAnswered
            | Self::ReviewOpened { repo_allowed: true }
            | Self::ChangesRequested
            | Self::Merged { .. }
            | Self::Drop
            | Self::BeginClosing
            | Self::ProcessLost { .. }
            | Self::Park
            | Self::Parked(_)
            | Self::Resume { .. } => Ok(()),
        }
    }

    const fn step(&self, task: TaskSnapshot) -> Step {
        use TaskState as S;
        let from = task.state;
        let keep = task.state_before_park;
        let (to, state_before_park) = match self {
            Self::Start(_) | Self::Resume { .. } => (S::Spawning, keep),
            Self::SessionStarted(_) => (resume_target(keep), None),
            Self::SpawnTimedOut | Self::LaunchFailed => (S::Failed, keep),
            Self::AskOpened => (S::NeedsUser, keep),
            Self::AskAnswered | Self::ChangesRequested => (S::Working, keep),
            Self::ReviewOpened { .. } => (S::InReview, keep),
            Self::Merged { .. } => (S::Merged, keep),
            Self::Close(_) | Self::Finish(_) | Self::BeginClosing => (S::Closing, keep),
            Self::ForceClose => (S::Closing, None),
            Self::Drop => (S::Dropped, None),
            Self::Cleaned(_) => (S::Closed, keep),
            Self::ProcessLost { .. } => (S::Interrupted, Some(from)),
            Self::Park => (S::Parking, Some(from)),
            Self::Parked(_) => (S::Parked, keep),
        };
        Step {
            from,
            to,
            state_before_park,
        }
    }
}

/// Decides one transition.
///
/// # Errors
///
/// Returns a [`Refusal`] when the trigger does not apply to the task's state or its guard fails.
pub const fn apply(task: &TaskSnapshot, trigger: &Trigger) -> Result<Step, Refusal> {
    if !trigger.applies_to(task.state) {
        return Err(Refusal::WrongState {
            trigger: trigger.name(),
            state: task.state,
        });
    }
    match trigger.guard(*task) {
        Ok(()) => Ok(trigger.step(*task)),
        Err(refusal) => Err(refusal),
    }
}

/// Whether `drop` applies: every state that is not closing or over.
#[must_use]
pub const fn droppable(state: TaskState) -> bool {
    !matches!(
        state,
        TaskState::Closing | TaskState::Closed | TaskState::Dropped
    )
}

/// The state a resumed session returns to. An open question is asked again after a resume, so `needs_user` comes
/// back as `working`.
#[must_use]
pub const fn resume_target(before: Option<TaskState>) -> TaskState {
    match before {
        Some(TaskState::InReview) => TaskState::InReview,
        Some(TaskState::Merged) => TaskState::Merged,
        Some(
            TaskState::Queued
            | TaskState::Spawning
            | TaskState::Working
            | TaskState::NeedsUser
            | TaskState::Closing
            | TaskState::Closed
            | TaskState::Dropped
            | TaskState::Failed
            | TaskState::Interrupted
            | TaskState::Parking
            | TaskState::Parked,
        )
        | None => TaskState::Working,
    }
}

/// The guard of `queued` → `spawning`.
///
/// # Errors
///
/// Returns the first failing check as a [`Refusal`].
pub const fn check_start(facts: &StartFacts) -> Result<(), Refusal> {
    if !matches!(facts.machine, MachineMode::Normal | MachineMode::Resuming) {
        return Err(Refusal::MachineMode(facts.machine));
    }
    if facts.live_workers >= facts.max_workers {
        return Err(Refusal::NoFreeSlot {
            max: facts.max_workers,
        });
    }
    if let Some(cap) = facts.project_cap
        && facts.project_live >= cap
    {
        return Err(Refusal::ProjectCapReached { cap });
    }
    if facts.main_only_kind_busy {
        return Err(Refusal::MainOnlyKindBusy);
    }
    if matches!(facts.dependency, Dependency::Waiting) {
        return Err(Refusal::DependencyNotMerged);
    }
    if matches!(facts.fable, FableGate::Waiting) {
        return Err(Refusal::FableNotAllowed);
    }
    Ok(())
}

const fn check_close(keep: bool, facts: CloseFacts) -> Result<(), Refusal> {
    if keep {
        return Err(Refusal::KeepTask);
    }
    if facts.open_ask {
        return Err(Refusal::OpenAsk);
    }
    if facts.running_job {
        return Err(Refusal::RunningJob);
    }
    if !facts.report_sent {
        return Err(Refusal::ReportMissing);
    }
    Ok(())
}

const fn check_finish(keep: bool, facts: FinishFacts) -> Result<(), Refusal> {
    if keep {
        return Err(Refusal::KeepTask);
    }
    if facts.open_ask {
        return Err(Refusal::OpenAsk);
    }
    if facts.running_job {
        return Err(Refusal::RunningJob);
    }
    if !facts.report_sent {
        return Err(Refusal::ReportMissing);
    }
    match facts.pr {
        PrEnd::Open => Err(Refusal::PrOpen),
        PrEnd::Merged => Err(Refusal::PrMerged),
        PrEnd::None | PrEnd::Closed => Ok(()),
    }
}

const fn check_cleanup(facts: CleanupFacts) -> Result<(), Refusal> {
    if !facts.pane_gone {
        return Err(Refusal::PaneOpen);
    }
    if !facts.processes_gone {
        return Err(Refusal::ProcessesAlive);
    }
    if matches!(facts.worktree, WorktreeEnd::Present) {
        return Err(Refusal::WorktreePresent);
    }
    Ok(())
}
