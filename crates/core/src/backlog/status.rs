//! Stored backlog statuses (`docs/backlog.md`, section 10): the five statuses, the block an item carries while blocked,
//! the guard every move passes ([`transition`]) and what the end of a task does to the items linked to it
//! ([`on_task_end`]).
//!
//! Pure: the store gathers the facts, asks the guard and writes the row and its `backlog-moved` event together.

use std::fmt;
use std::str::FromStr;

use serde::ser::SerializeStruct as _;
use serde::{Deserialize, Serialize, Serializer};

use crate::vocabulary::text_enum;
use crate::{TaskState, UnknownWord};

text_enum!(
    /// Where an item stands, without the details a block carries.
    ItemStatus, "backlog status" {
        /// Wanted, not started.
        Backlog => "backlog",
        /// Spun up: a task works on it, or is about to.
        InProgress => "in_progress",
        /// Cannot move until something happens; the reason is stored.
        Blocked => "blocked",
        /// Finished.
        Done => "done",
        /// Not wanted any more; kept in the store, never exported. `dropped` is its spelling before 2026-11-27.
        Cancelled => "cancelled" | "dropped",
    }
);

impl ItemStatus {
    /// The statuses of an item still to finish: backlog, in progress and blocked.
    pub const OPEN: &'static [Self] = &[Self::Backlog, Self::InProgress, Self::Blocked];

    /// Whether the item is still to finish.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Backlog | Self::InProgress | Self::Blocked)
    }
}

text_enum!(
    /// Why an item is blocked.
    BlockReason, "block reason" {
        /// A dependency is not done.
        Depends => "depends",
        /// Its task was dropped or failed and the item is not done.
        TaskLost => "task_lost",
        /// Its task closed without finishing it.
        Partial => "partial",
        /// It needs the user's call.
        Decision => "decision",
        /// It waits for something outside, named in the note.
        Event => "event",
        /// A dependency was cancelled.
        DepCancelled => "dep_cancelled",
    }
);

text_enum!(
    /// The status an unblock returns a blocked item to.
    ReturnTo, "status before block" {
        /// Back to the backlog.
        Backlog => "backlog",
        /// Back in progress.
        InProgress => "in_progress",
    }
);

impl ReturnTo {
    /// The status this is.
    #[must_use]
    pub const fn status(self) -> ItemStatus {
        match self {
            Self::Backlog => ItemStatus::Backlog,
            Self::InProgress => ItemStatus::InProgress,
        }
    }

    /// The status this is, as an item holds it.
    #[must_use]
    pub const fn backlog_status(self) -> BacklogStatus {
        match self {
            Self::Backlog => BacklogStatus::Backlog,
            Self::InProgress => BacklogStatus::InProgress,
        }
    }
}

/// What a blocked item carries: why, a note, and where an unblock takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Why it is blocked.
    pub reason: BlockReason,
    /// The event, the decision, the old task.
    pub note: Option<String>,
    /// The status an unblock returns it to.
    pub before: ReturnTo,
}

/// Where an item stands; only `Blocked` carries a [`Block`], so a reason without a block cannot be written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BacklogStatus {
    /// Wanted, not started.
    #[default]
    Backlog,
    /// Spun up.
    InProgress,
    /// Waiting on what its block names.
    Blocked(Block),
    /// Finished.
    Done,
    /// Not wanted any more.
    Cancelled,
}

impl BacklogStatus {
    /// The status without its block.
    #[must_use]
    pub const fn kind(&self) -> ItemStatus {
        match self {
            Self::Backlog => ItemStatus::Backlog,
            Self::InProgress => ItemStatus::InProgress,
            Self::Blocked(_) => ItemStatus::Blocked,
            Self::Done => ItemStatus::Done,
            Self::Cancelled => ItemStatus::Cancelled,
        }
    }

    /// The block, when blocked.
    #[must_use]
    pub const fn block(&self) -> Option<&Block> {
        match self {
            Self::Blocked(block) => Some(block),
            Self::Backlog | Self::InProgress | Self::Done | Self::Cancelled => None,
        }
    }

    /// Whether the item is still to finish.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.kind().is_open()
    }

    /// The status a row holds: `status`, then `blocked_reason`, `blocked_note` and `status_before_block`, which only a
    /// blocked row has.
    ///
    /// # Errors
    ///
    /// Returns the [`UnknownWord`] of a word that does not parse, or of a blocked row that lacks its reason or the status
    /// to return to.
    pub fn from_columns(
        status: &str,
        reason: Option<&str>,
        note: Option<String>,
        before: Option<&str>,
    ) -> Result<Self, UnknownWord> {
        Ok(match status.parse::<ItemStatus>()? {
            ItemStatus::Backlog => Self::Backlog,
            ItemStatus::InProgress => Self::InProgress,
            ItemStatus::Done => Self::Done,
            ItemStatus::Cancelled => Self::Cancelled,
            ItemStatus::Blocked => {
                let missing = |kind| UnknownWord {
                    kind,
                    word: String::new(),
                };
                Self::Blocked(Block {
                    reason: reason.ok_or_else(|| missing("block reason"))?.parse()?,
                    note,
                    before: before
                        .ok_or_else(|| missing("status before block"))?
                        .parse()?,
                })
            }
        })
    }
}

impl fmt::Display for BacklogStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blocked(block) => match &block.note {
                Some(note) => write!(f, "blocked ({}: {note})", block.reason),
                None => write!(f, "blocked ({})", block.reason),
            },
            Self::Backlog | Self::InProgress | Self::Done | Self::Cancelled => {
                f.write_str(self.kind().as_str())
            }
        }
    }
}

impl Serialize for BacklogStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let block = self.block();
        let mut out = serializer.serialize_struct("BacklogStatus", 4)?;
        out.serialize_field("status", &self.kind())?;
        out.serialize_field("blocked_reason", &block.map(|block| block.reason))?;
        out.serialize_field(
            "blocked_note",
            &block.and_then(|block| block.note.as_deref()),
        )?;
        out.serialize_field("status_before_block", &block.map(|block| block.before))?;
        out.end()
    }
}

text_enum!(
    /// Who moves an item (`docs/backlog.md`, section 10).
    BacklogActor, "backlog actor" {
        /// The user at the CLI, with no `HEADWORK_ROLE`.
        User => "user",
        /// The orchestrator, through its tools or the CLI it runs.
        Orchestrator => "orchestrator",
        /// The explore task linked to an epic, for that epic's children only.
        Explorer => "explorer",
        /// The task linked to the item, for that item only.
        Worker => "worker",
        /// headwork itself: hooks, close, tick, queue, reacting to a fact it holds.
        Headwork => "headwork",
    }
);

/// Where a move takes an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// To the backlog: a new item, or a done or cancelled one reopened.
    Backlog,
    /// Spun up: started, relinked to a new task, or respawned after its task was lost.
    InProgress,
    /// Blocked for `reason`, with a note.
    Blocked {
        /// Why.
        reason: BlockReason,
        /// The event, the decision, the old task.
        note: Option<String>,
    },
    /// Back to the status it had before its block.
    Unblock,
    /// Finished.
    Done,
    /// Not wanted any more.
    Cancelled,
}

impl Target {
    /// The status word this target names; an unblock names none of its own.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::InProgress => "in_progress",
            Self::Blocked { .. } => "blocked",
            Self::Unblock => "unblocked",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }
}

/// What the guard knows about an item besides its status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts the guard reads, one per row condition of the transition table"
)]
pub struct MoveFacts<'a> {
    /// The name of the live task linked to it (any state but closed, dropped and failed).
    pub live_task: Option<&'a str>,
    /// Whether the acting worker's own task is the one linked to it.
    pub linked_to_actor: bool,
    /// Whether it is a child of the acting explorer's epic (always false until epics exist).
    pub in_actor_epic: bool,
    /// Whether a task can work on it: its project is one headwork runs, or a spawn names the project.
    pub has_project: bool,
    /// Whether the move came with a non-empty reason.
    pub has_reason: bool,
}

/// What kind of move the guard allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveKind {
    /// A new item.
    Add,
    /// Backlog to in progress.
    Start,
    /// In progress again, for a new task, after the old one ended.
    Relink,
    /// Blocked (task lost, partial) back to in progress.
    Respawn,
    /// Finished.
    Finish,
    /// Blocked.
    Block,
    /// Back from blocked.
    Unblock,
    /// Cancelled.
    Cancel,
    /// Done or cancelled back to the backlog.
    Reopen,
}

/// A move the guard allowed: the status before (none for a new item), the status after and the kind of move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemMove {
    /// The status before; `None` for a new item.
    pub from: Option<ItemStatus>,
    /// The status after, with its block.
    pub to: BacklogStatus,
    /// What kind of move it is.
    pub kind: MoveKind,
}

/// Why the guard refused a move.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoveRefusal {
    /// A worker touched an item its task is not linked to.
    #[error("a worker may change only the item its own task is linked to")]
    NotLinked,
    /// An explorer touched an item outside its epic.
    #[error("an explorer may change only its own epic's children")]
    OutsideEpic,
    /// Anyone but headwork finishing an item a live task works on.
    #[error("task {0} works on it: only headwork finishes it, when the task finishes")]
    FinishedByTask(String),
    /// A cancel of an item a live task works on (P1: the drop is not part of the cancel yet).
    #[error("task {0} works on it: drop the task first, then cancel the item")]
    DropTaskFirst(String),
    /// A start or link while a live task already works on it.
    #[error("task {0} already works on it")]
    TaskWorks(String),
    /// A start of an item no task can work on.
    #[error("no task can work on it: its project is not one headwork runs")]
    NoProject,
    /// The orchestrator cancelling without saying why.
    #[error("the orchestrator cancels an item only with a reason (the user's words)")]
    ReasonRequired,
    /// A done item cancelled.
    #[error("it is done: a done item is never cancelled")]
    CancelDone,
    /// A move to the status it already has.
    #[error("it is already {0}")]
    Already(ItemStatus),
    /// A block for a reason this actor may not give.
    #[error("the {actor} may not block an item for `{reason}`")]
    Reason {
        /// Who tried.
        actor: BacklogActor,
        /// The reason given.
        reason: BlockReason,
    },
    /// A move the table has no row for, for this actor.
    #[error("the {actor} may not move an item from {from} to {to}")]
    NotAllowed {
        /// Who tried.
        actor: BacklogActor,
        /// The status before, or `new`.
        from: String,
        /// The target's word.
        to: &'static str,
    },
}

fn not_allowed(actor: BacklogActor, from: Option<ItemStatus>, to: &Target) -> MoveRefusal {
    MoveRefusal::NotAllowed {
        actor,
        from: from.map_or_else(|| "new".to_owned(), |status| status.as_str().to_owned()),
        to: to.word(),
    }
}

const fn moved(from: Option<ItemStatus>, to: BacklogStatus, kind: MoveKind) -> ItemMove {
    ItemMove { from, to, kind }
}

/// The guard every move of an item passes (`docs/backlog.md`, section 10, "Transitions and who may make each").
///
/// `from` is the item's status, `None` for an item not yet added. A worker may only block its own in-progress item for a
/// decision or an event and unblock it; an explorer may add items and touch only its epic's children; only headwork
/// finishes an item a live task works on; a cancel of such an item is refused until its task is dropped; a done item is
/// never cancelled; the orchestrator cancels only with a reason.
///
/// # Errors
///
/// Returns the [`MoveRefusal`] that names why the move is not allowed.
pub fn transition(
    from: Option<&BacklogStatus>,
    to: &Target,
    actor: BacklogActor,
    facts: &MoveFacts<'_>,
) -> Result<ItemMove, MoveRefusal> {
    use BacklogActor as A;
    let kind = from.map(BacklogStatus::kind);
    let refuse = || not_allowed(actor, kind, to);
    if actor == A::Worker && !facts.linked_to_actor {
        return Err(MoveRefusal::NotLinked);
    }
    let Some(from) = from else {
        return match (to, actor) {
            (Target::Backlog, A::User | A::Orchestrator | A::Explorer | A::Headwork) => {
                Ok(moved(None, BacklogStatus::Backlog, MoveKind::Add))
            }
            (
                Target::Blocked {
                    reason: BlockReason::Depends,
                    note,
                },
                A::Explorer,
            ) if facts.in_actor_epic => Ok(moved(
                None,
                BacklogStatus::Blocked(Block {
                    reason: BlockReason::Depends,
                    note: note.clone(),
                    before: ReturnTo::Backlog,
                }),
                MoveKind::Add,
            )),
            (_, A::Explorer) => Err(MoveRefusal::OutsideEpic),
            _ => Err(refuse()),
        };
    };
    if actor == A::Explorer && !facts.in_actor_epic {
        return Err(MoveRefusal::OutsideEpic);
    }
    let kind = Some(from.kind());
    match to {
        Target::Backlog => match from {
            BacklogStatus::Done | BacklogStatus::Cancelled
                if matches!(actor, A::User | A::Orchestrator) =>
            {
                Ok(moved(kind, BacklogStatus::Backlog, MoveKind::Reopen))
            }
            BacklogStatus::Backlog => Err(MoveRefusal::Already(ItemStatus::Backlog)),
            BacklogStatus::Done
            | BacklogStatus::Cancelled
            | BacklogStatus::InProgress
            | BacklogStatus::Blocked(_) => Err(refuse()),
        },
        Target::InProgress => {
            start(from, actor, facts).map(|kind_of| moved(kind, BacklogStatus::InProgress, kind_of))
        }
        Target::Blocked { reason, note } => {
            let before = match from {
                BacklogStatus::Backlog => ReturnTo::Backlog,
                BacklogStatus::InProgress => ReturnTo::InProgress,
                BacklogStatus::Blocked(_) => {
                    return Err(MoveRefusal::Already(ItemStatus::Blocked));
                }
                BacklogStatus::Done | BacklogStatus::Cancelled => return Err(refuse()),
            };
            block_allowed(before, *reason, actor)?;
            Ok(moved(
                kind,
                BacklogStatus::Blocked(Block {
                    reason: *reason,
                    note: note.clone(),
                    before,
                }),
                MoveKind::Block,
            ))
        }
        Target::Unblock => match from {
            BacklogStatus::Blocked(block)
                if matches!(actor, A::Headwork | A::Worker | A::Orchestrator | A::User) =>
            {
                Ok(moved(
                    kind,
                    block.before.backlog_status(),
                    MoveKind::Unblock,
                ))
            }
            BacklogStatus::Blocked(_)
            | BacklogStatus::Backlog
            | BacklogStatus::InProgress
            | BacklogStatus::Done
            | BacklogStatus::Cancelled => Err(refuse()),
        },
        Target::Done => {
            finish(from, actor, facts).map(|()| moved(kind, BacklogStatus::Done, MoveKind::Finish))
        }
        Target::Cancelled => cancel(from, actor, facts)
            .map(|()| moved(kind, BacklogStatus::Cancelled, MoveKind::Cancel)),
    }
}

fn start(
    from: &BacklogStatus,
    actor: BacklogActor,
    facts: &MoveFacts<'_>,
) -> Result<MoveKind, MoveRefusal> {
    use BacklogActor as A;
    let refuse = || not_allowed(actor, Some(from.kind()), &Target::InProgress);
    let kind = match from {
        BacklogStatus::Backlog if matches!(actor, A::User | A::Orchestrator | A::Headwork) => {
            MoveKind::Start
        }
        BacklogStatus::InProgress if matches!(actor, A::User | A::Orchestrator | A::Headwork) => {
            MoveKind::Relink
        }
        BacklogStatus::Blocked(Block {
            reason: BlockReason::TaskLost | BlockReason::Partial,
            ..
        }) if matches!(actor, A::User | A::Orchestrator | A::Headwork) => MoveKind::Respawn,
        BacklogStatus::Backlog
        | BacklogStatus::InProgress
        | BacklogStatus::Blocked(_)
        | BacklogStatus::Done
        | BacklogStatus::Cancelled => return Err(refuse()),
    };
    if let Some(task) = facts.live_task {
        return Err(MoveRefusal::TaskWorks(task.to_owned()));
    }
    if !facts.has_project {
        return Err(MoveRefusal::NoProject);
    }
    Ok(kind)
}

fn block_allowed(
    before: ReturnTo,
    reason: BlockReason,
    actor: BacklogActor,
) -> Result<(), MoveRefusal> {
    use BacklogActor as A;
    use BlockReason as R;
    let allowed = match (before, actor) {
        (ReturnTo::InProgress, A::Worker | A::Orchestrator | A::User)
        | (ReturnTo::Backlog, A::Orchestrator | A::User) => {
            matches!(reason, R::Decision | R::Event)
        }
        (ReturnTo::InProgress, A::Headwork) => matches!(reason, R::TaskLost | R::Partial),
        (ReturnTo::Backlog, A::Headwork) => matches!(reason, R::DepCancelled),
        (ReturnTo::Backlog, A::Explorer) => matches!(reason, R::Depends),
        (ReturnTo::InProgress, A::Explorer) | (ReturnTo::Backlog, A::Worker) => {
            return Err(MoveRefusal::NotAllowed {
                actor,
                from: before.status().as_str().to_owned(),
                to: "blocked",
            });
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(MoveRefusal::Reason { actor, reason })
    }
}

fn finish(
    from: &BacklogStatus,
    actor: BacklogActor,
    facts: &MoveFacts<'_>,
) -> Result<(), MoveRefusal> {
    use BacklogActor as A;
    match (from, actor) {
        (BacklogStatus::Done, _) => Err(MoveRefusal::Already(ItemStatus::Done)),
        (BacklogStatus::InProgress, A::Headwork) => Ok(()),
        (
            BacklogStatus::Backlog | BacklogStatus::InProgress | BacklogStatus::Blocked(_),
            A::User | A::Orchestrator,
        ) => facts.live_task.map_or(Ok(()), |task| {
            Err(MoveRefusal::FinishedByTask(task.to_owned()))
        }),
        _ => Err(not_allowed(actor, Some(from.kind()), &Target::Done)),
    }
}

fn cancel(
    from: &BacklogStatus,
    actor: BacklogActor,
    facts: &MoveFacts<'_>,
) -> Result<(), MoveRefusal> {
    use BacklogActor as A;
    match (from, actor) {
        (BacklogStatus::Done, _) => Err(MoveRefusal::CancelDone),
        (BacklogStatus::Cancelled, _) => Err(MoveRefusal::Already(ItemStatus::Cancelled)),
        (
            BacklogStatus::Backlog | BacklogStatus::InProgress | BacklogStatus::Blocked(_),
            A::User | A::Orchestrator,
        )
        | (BacklogStatus::Backlog | BacklogStatus::Blocked(_), A::Explorer) => {
            if let Some(task) = facts.live_task {
                return Err(MoveRefusal::DropTaskFirst(task.to_owned()));
            }
            if actor == A::Orchestrator && !facts.has_reason {
                return Err(MoveRefusal::ReasonRequired);
            }
            Ok(())
        }
        _ => Err(not_allowed(actor, Some(from.kind()), &Target::Cancelled)),
    }
}

/// What the task said about its item when it ended; `Done` until the worker's `done` can say otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskOutcome {
    /// The task finished the work.
    #[default]
    Done,
}

/// What the end of a task does to each in-progress item linked to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskEndEffect {
    /// The item is done today with this note, and the link ends.
    Finish {
        /// `task <name> merged <urls>`, or `task <name>, no pull request`.
        note: String,
    },
    /// The item is blocked (`task_lost`) with this note, back to in progress on unblock, and the link ends.
    Lost {
        /// `task <name> dropped` or `task <name> failed`.
        note: String,
    },
}

impl TaskEndEffect {
    /// The move this effect asks of the guard.
    #[must_use]
    pub fn target(&self) -> Target {
        match self {
            Self::Finish { .. } => Target::Done,
            Self::Lost { note } => Target::Blocked {
                reason: BlockReason::TaskLost,
                note: Some(note.clone()),
            },
        }
    }
}

/// What a task that moves to `state` does to each in-progress item linked to it (`docs/backlog.md`, section 10).
///
/// `merged` finishes it; `closed` (an item still in progress, so it never saw `merged`) finishes it, naming the merged
/// pull requests or saying there was none; `dropped` and `failed` block it as `task_lost`; any other state leaves it
/// alone. `merged_urls` are the task's merged pull requests, oldest first.
#[must_use]
pub fn on_task_end(
    state: TaskState,
    outcome: TaskOutcome,
    merged_urls: &[String],
    task_name: &str,
) -> Option<TaskEndEffect> {
    let merged = || {
        if merged_urls.is_empty() {
            format!("task {task_name} merged")
        } else {
            format!("task {task_name} merged {}", merged_urls.join(", "))
        }
    };
    match (state, outcome) {
        (TaskState::Merged, TaskOutcome::Done) => Some(TaskEndEffect::Finish { note: merged() }),
        (TaskState::Closed, TaskOutcome::Done) => Some(TaskEndEffect::Finish {
            note: if merged_urls.is_empty() {
                format!("task {task_name}, no pull request")
            } else {
                merged()
            },
        }),
        (TaskState::Dropped, _) => Some(TaskEndEffect::Lost {
            note: format!("task {task_name} dropped"),
        }),
        (TaskState::Failed, _) => Some(TaskEndEffect::Lost {
            note: format!("task {task_name} failed"),
        }),
        (
            TaskState::Queued
            | TaskState::Spawning
            | TaskState::Working
            | TaskState::NeedsUser
            | TaskState::InReview
            | TaskState::Closing
            | TaskState::Interrupted
            | TaskState::Parking
            | TaskState::Parked,
            _,
        ) => None,
    }
}
