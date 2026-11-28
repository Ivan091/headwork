//! Stored backlog statuses (`docs/backlog.md`, section 10): the five statuses, the block an item carries while blocked,
//! the guard every move passes ([`transition`]) and what the end of a task does to the items linked to it
//! ([`on_task_end`]).
//!
//! Pure: the store gathers the facts, asks the guard and writes the row and its `backlog-moved` event together.

use std::fmt;
use std::str::FromStr;

use serde::ser::SerializeStruct as _;
use serde::{Deserialize, Serialize, Serializer};

use crate::backlog::ItemKind;
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

impl BacklogActor {
    /// Who moves an item for an event's actor: the user and the orchestrator as themselves, a worker as an explorer when
    /// it acts as the exploration adding its picks (`explorer`) and else as a worker, the tick and hooks as headwork.
    #[must_use]
    pub const fn from_actor(actor: crate::Actor, explorer: bool) -> Self {
        match actor {
            crate::Actor::User => Self::User,
            crate::Actor::Orchestrator => Self::Orchestrator,
            crate::Actor::Worker if explorer => Self::Explorer,
            crate::Actor::Worker => Self::Worker,
            crate::Actor::Tick | crate::Actor::Hook => Self::Headwork,
        }
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
    /// The link to a task that is over ends and the status stays; headwork only, for a done or blocked item and an
    /// in-progress epic whose explorer ended.
    Unlink,
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
            Self::Unlink => "unlinked",
        }
    }
}

fn waiting_text(open: usize, cancelled: usize) -> String {
    let part = |count: usize, word: &str| (count > 0).then(|| format!("{count} {word}"));
    let parts = [part(open, "open"), part(cancelled, "cancelled")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let noun = if open + cancelled == 1 {
        "dependency"
    } else {
        "dependencies"
    };
    format!("{} {noun}", parts.join(" and "))
}

/// What the guard knows about an item besides its status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts the guard reads, one per condition of a transition table row"
)]
pub struct MoveFacts<'a> {
    /// The name of the live task linked to it (any state but closed, dropped and failed).
    pub live_task: Option<&'a str>,
    /// Whether the acting worker's own task is the one linked to it; for headwork at a task's end, whether the ending task
    /// is.
    pub linked_to_actor: bool,
    /// Whether it is a child of the acting explorer's epic.
    pub in_actor_epic: bool,
    /// Whether it is the acting explorer's epic itself.
    pub is_actor_epic: bool,
    /// How many of its children are open (backlog, in progress or blocked); 0 for an item that is not an epic.
    pub open_children: usize,
    /// Whether a task can work on it: its project is one headwork runs, or a spawn names the project.
    pub has_project: bool,
    /// Whether the move names a task to link that is not the live task linked to it.
    pub links_another_task: bool,
    /// How many items it depends on that are neither done nor cancelled.
    pub open_dependencies: usize,
    /// How many items it depends on that are cancelled.
    pub cancelled_dependencies: usize,
    /// The note of a finish or the reason of a cancel the move came with.
    pub reason: Option<&'a str>,
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
    /// Finished, and the link ended.
    Finish,
    /// Finished by headwork while its task still lives (it merged): the link stays, so the task can take it back if it
    /// opens another pull request ([`MoveKind::Unfinish`]).
    MergeFinish,
    /// Blocked.
    Block,
    /// Back from blocked.
    Unblock,
    /// Cancelled.
    Cancel,
    /// Done or cancelled back to the backlog.
    Reopen,
    /// Done back to in progress: its task, merged, opened another pull request.
    Unfinish,
    /// The link to a task that is over ends; the status stays, and a done item's note becomes the move's.
    Unlink,
}

/// What a move does to the item's link to a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEffect {
    /// The link stays as it is.
    Keep,
    /// The link ends.
    Clear,
    /// The item is linked to the task the move names, or to none when it names none.
    Set,
}

/// What a move does to the item's `done_on` and `done_note`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateEffect {
    /// They stay as they are.
    Keep,
    /// `done_on` becomes today and `done_note` the move's note or reason.
    Stamp,
    /// `done_on` stays and `done_note` becomes the move's note, when it has one.
    Renote,
    /// Both are cleared.
    Clear,
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
    /// What it does to the link.
    pub link: LinkEffect,
    /// What it does to the done date and note.
    pub dates: DateEffect,
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
    /// An explorer finishing its epic while children are still open.
    #[error("the epic has {0} open children: finish or cancel them first")]
    ChildrenOpen(usize),
    /// Anyone but headwork finishing an item a live task works on.
    #[error("task {0} works on it: only headwork finishes it, when the task finishes")]
    FinishedByTask(String),
    /// A cancel of an item a live task works on, at the store; the ops-level cancel drops the task first.
    #[error("task {0} works on it: drop the task first, then cancel the item")]
    DropTaskFirst(String),
    /// A start, respawn, relink or unblock of an item that still waits for dependencies.
    #[error("it waits for {}: finish them, or remove the dependencies", waiting_text(*open, *cancelled))]
    Dependencies {
        /// Dependencies not yet done or cancelled.
        open: usize,
        /// Dependencies that were cancelled.
        cancelled: usize,
    },
    /// A start or link while a live task already works on it.
    #[error("task {0} already works on it")]
    TaskWorks(String),
    /// A start of an item no task can work on.
    #[error("no task can work on it: its project is not one headwork runs")]
    NoProject,
    /// A note or reason that runs over several lines, which the file could not hold.
    #[error("the {0} must be one line")]
    MultiLine(&'static str),
    /// A block note holding `[` or `]`, which would break the status tag it is written into.
    #[error("the {0} must not hold `[` or `]`")]
    Bracket(&'static str),
    /// A done note whose parentheses do not pair up, which would break the `(done D: note)` group it is written into.
    #[error("the {0} must pair its parentheses")]
    Parentheses(&'static str),
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
    ItemMove {
        from,
        to,
        kind,
        link: LinkEffect::Keep,
        dates: DateEffect::Keep,
    }
}

/// The link and date effects of an allowed move: a start, relink or respawn links the move's task; a finish stamps the
/// date and ends the link, except headwork's finish of an item whose task still lives (merged), which keeps it so the
/// task can take it back; a cancel stamps and ends the link; a reopen clears the date and ends the link (a done item
/// may still be linked to its merged task); an unfinish clears the date; a block for a lost
/// task ends the link; everything else keeps both.
fn with_effects(step: ItemMove) -> ItemMove {
    let (link, dates) = match (step.kind, &step.to) {
        (MoveKind::Start | MoveKind::Relink | MoveKind::Respawn, _) => {
            (LinkEffect::Set, DateEffect::Keep)
        }
        (MoveKind::MergeFinish, _) => (LinkEffect::Keep, DateEffect::Stamp),
        (MoveKind::Finish | MoveKind::Cancel, _) => (LinkEffect::Clear, DateEffect::Stamp),
        (MoveKind::Unlink, BacklogStatus::Done) => (LinkEffect::Clear, DateEffect::Renote),
        (MoveKind::Unlink, _) => (LinkEffect::Clear, DateEffect::Keep),
        (MoveKind::Reopen, _) => (LinkEffect::Clear, DateEffect::Clear),
        (MoveKind::Unfinish, _) => (LinkEffect::Keep, DateEffect::Clear),
        (MoveKind::Block, BacklogStatus::Blocked(block))
            if block.reason == BlockReason::TaskLost =>
        {
            (LinkEffect::Clear, DateEffect::Keep)
        }
        (MoveKind::Add | MoveKind::Block | MoveKind::Unblock, _) => {
            (LinkEffect::Keep, DateEffect::Keep)
        }
    };
    ItemMove {
        link,
        dates,
        ..step
    }
}

fn one_line(field: &'static str, text: Option<&str>) -> Result<(), MoveRefusal> {
    let Some(text) = text else {
        return Ok(());
    };
    if text.contains(['\n', '\r']) {
        Err(MoveRefusal::MultiLine(field))
    } else {
        Ok(())
    }
}

/// Refuses a text whose parentheses do not pair up: a `)` with no `(` before it, or a `(` left open.
fn balanced(field: &'static str, text: Option<&str>) -> Result<(), MoveRefusal> {
    let mut depth = 0_usize;
    for c in text.unwrap_or_default().chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(MoveRefusal::Parentheses(field))?;
            }
            _ => {}
        }
    }
    if depth == 0 {
        Ok(())
    } else {
        Err(MoveRefusal::Parentheses(field))
    }
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
    if let Target::Blocked { note, .. } = to {
        one_line("note", note.as_deref())?;
        if note
            .as_deref()
            .is_some_and(|note| note.contains(['[', ']']))
        {
            return Err(MoveRefusal::Bracket("note"));
        }
    }
    one_line("reason", facts.reason)?;
    if matches!(to, Target::Done | Target::Unlink) {
        balanced("note", facts.reason)?;
    }
    decide(from, to, actor, facts).map(with_effects)
}

fn decide(
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
    if actor == A::Explorer && facts.is_actor_epic {
        return finish_epic(from, to, facts);
    }
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
        Target::Blocked { reason, note } => block(from, *reason, note.as_ref(), actor),
        Target::Unblock => match from {
            BacklogStatus::Blocked(block)
                if matches!(actor, A::Headwork | A::Worker | A::Orchestrator | A::User) =>
            {
                if matches!(
                    block.reason,
                    BlockReason::Depends | BlockReason::DepCancelled
                ) && (facts.open_dependencies > 0 || facts.cancelled_dependencies > 0)
                {
                    return Err(MoveRefusal::Dependencies {
                        open: facts.open_dependencies,
                        cancelled: facts.cancelled_dependencies,
                    });
                }
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
            finish(from, actor, facts).map(|kind_of| moved(kind, BacklogStatus::Done, kind_of))
        }
        Target::Unlink => {
            unlink(from, actor, facts).map(|()| moved(kind, from.clone(), MoveKind::Unlink))
        }
        Target::Cancelled => cancel(from, actor, facts)
            .map(|()| moved(kind, BacklogStatus::Cancelled, MoveKind::Cancel)),
    }
}

/// The one move an explorer makes on its epic itself: in progress to done once no child is open (`plan_finish`). The link
/// ends even though the live task is the explorer's own.
fn finish_epic(
    from: &BacklogStatus,
    to: &Target,
    facts: &MoveFacts<'_>,
) -> Result<ItemMove, MoveRefusal> {
    match (from, to) {
        (BacklogStatus::InProgress, Target::Done) if facts.open_children > 0 => {
            Err(MoveRefusal::ChildrenOpen(facts.open_children))
        }
        (BacklogStatus::InProgress, Target::Done) => Ok(moved(
            Some(ItemStatus::InProgress),
            BacklogStatus::Done,
            MoveKind::Finish,
        )),
        _ => Err(not_allowed(BacklogActor::Explorer, Some(from.kind()), to)),
    }
}

fn block(
    from: &BacklogStatus,
    reason: BlockReason,
    note: Option<&String>,
    actor: BacklogActor,
) -> Result<ItemMove, MoveRefusal> {
    let before = match from {
        BacklogStatus::Backlog => ReturnTo::Backlog,
        BacklogStatus::InProgress => ReturnTo::InProgress,
        BacklogStatus::Blocked(Block {
            reason: current @ (BlockReason::Depends | BlockReason::DepCancelled),
            before,
            ..
        }) if actor == BacklogActor::Headwork
            && matches!(reason, BlockReason::Depends | BlockReason::DepCancelled)
            && *current != reason =>
        {
            *before
        }
        BacklogStatus::Blocked(_) => return Err(MoveRefusal::Already(ItemStatus::Blocked)),
        BacklogStatus::Done | BacklogStatus::Cancelled => {
            return Err(not_allowed(
                actor,
                Some(from.kind()),
                &Target::Blocked { reason, note: None },
            ));
        }
    };
    if !matches!(from, BacklogStatus::Blocked(_)) {
        block_allowed(before, reason, actor)?;
    }
    Ok(moved(
        Some(from.kind()),
        BacklogStatus::Blocked(Block {
            reason,
            note: note.cloned(),
            before,
        }),
        MoveKind::Block,
    ))
}

/// Headwork ends the link of a done or blocked item whose task is over, and of an in-progress epic whose explorer, the
/// task linked to it, ended (merged, while that task still lives, or closed); the status stays.
fn unlink(
    from: &BacklogStatus,
    actor: BacklogActor,
    facts: &MoveFacts<'_>,
) -> Result<(), MoveRefusal> {
    match from {
        BacklogStatus::Done | BacklogStatus::Blocked(_)
            if actor == BacklogActor::Headwork && facts.live_task.is_none() =>
        {
            Ok(())
        }
        BacklogStatus::InProgress if actor == BacklogActor::Headwork && facts.linked_to_actor => {
            Ok(())
        }
        BacklogStatus::Done
        | BacklogStatus::Blocked(_)
        | BacklogStatus::Backlog
        | BacklogStatus::InProgress
        | BacklogStatus::Cancelled => Err(not_allowed(actor, Some(from.kind()), &Target::Unlink)),
    }
}

fn start(
    from: &BacklogStatus,
    actor: BacklogActor,
    facts: &MoveFacts<'_>,
) -> Result<MoveKind, MoveRefusal> {
    use BacklogActor as A;
    let refuse = || not_allowed(actor, Some(from.kind()), &Target::InProgress);
    if *from == BacklogStatus::Done && actor == A::Headwork {
        let Some(task) = facts.live_task else {
            return Err(refuse());
        };
        if facts.links_another_task {
            return Err(MoveRefusal::TaskWorks(task.to_owned()));
        }
        if !facts.has_project {
            return Err(MoveRefusal::NoProject);
        }
        return Ok(MoveKind::Unfinish);
    }
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
    if matches!(kind, MoveKind::Start | MoveKind::Respawn | MoveKind::Relink)
        && (facts.open_dependencies > 0 || facts.cancelled_dependencies > 0)
    {
        return Err(MoveRefusal::Dependencies {
            open: facts.open_dependencies,
            cancelled: facts.cancelled_dependencies,
        });
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
        (ReturnTo::Backlog, A::Headwork) => matches!(reason, R::Depends | R::DepCancelled),
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
) -> Result<MoveKind, MoveRefusal> {
    use BacklogActor as A;
    match (from, actor) {
        (BacklogStatus::Done, _) => Err(MoveRefusal::Already(ItemStatus::Done)),
        (BacklogStatus::InProgress, A::Headwork) if facts.live_task.is_some() => {
            Ok(MoveKind::MergeFinish)
        }
        (BacklogStatus::InProgress, A::Headwork) => Ok(MoveKind::Finish),
        (
            BacklogStatus::Backlog | BacklogStatus::InProgress | BacklogStatus::Blocked(_),
            A::User | A::Orchestrator,
        ) => facts.live_task.map_or(Ok(MoveKind::Finish), |task| {
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
            if actor == A::Orchestrator
                && facts.reason.is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(MoveRefusal::ReasonRequired);
            }
            Ok(())
        }
        _ => Err(not_allowed(actor, Some(from.kind()), &Target::Cancelled)),
    }
}

/// What the task said about its item when it ended: the outcome of the worker's `done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskOutcome {
    /// The task finished the work.
    #[default]
    Done,
    /// The task closed without finishing the work: the item is blocked as `partial`.
    Partial,
}

impl TaskOutcome {
    /// The word a `done` call gives for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Partial => "partial",
        }
    }
}

impl FromStr for TaskOutcome {
    type Err = UnknownWord;

    fn from_str(word: &str) -> Result<Self, Self::Err> {
        match word {
            "done" => Ok(Self::Done),
            "partial" => Ok(Self::Partial),
            other => Err(UnknownWord {
                kind: "task outcome",
                word: other.to_owned(),
            }),
        }
    }
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
    /// The item is blocked (`partial`) with this note: the task closed without finishing it.
    Partial {
        /// `task <name> partial`, then the worker's note.
        note: String,
    },
    /// An epic whose explorer merged or closed: it stays in progress and the link ends, so a later explorer can take it.
    Release {
        /// `task <name> ended, the epic stays in progress`.
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
            Self::Partial { note } => Target::Blocked {
                reason: BlockReason::Partial,
                note: Some(note.clone()),
            },
            Self::Release { .. } => Target::Unlink,
        }
    }
}

/// What a task that moves to `state` does to each in-progress item linked to it (`docs/backlog.md`, section 10).
///
/// `merged` finishes it; `closed` (an item still in progress, so it never saw `merged`) finishes it, naming the merged
/// pull requests or saying there was none; `dropped` and `failed` block it as `task_lost`, unless the task already merged
/// work (an item a merged task took back when it opened another pull request), which finishes it; any other state leaves
/// it alone. `merged_urls` are the task's merged pull requests, oldest first.
///
/// An epic (`kind`) is never finished by its explorer's end, only by `plan_finish` or the user: merged or closed, whatever
/// the outcome, it is released (in progress, link ended); dropped or failed, it is blocked as `task_lost`.
#[must_use]
pub fn on_task_end(
    state: TaskState,
    outcome: TaskOutcome,
    kind: ItemKind,
    merged_urls: &[String],
    task_name: &str,
) -> Option<TaskEndEffect> {
    if kind == ItemKind::Epic {
        return epic_task_end(state, task_name);
    }
    let merged = || {
        if merged_urls.is_empty() {
            format!("task {task_name} merged")
        } else {
            format!("task {task_name} merged {}", merged_urls.join(", "))
        }
    };
    match (state, outcome) {
        (TaskState::Merged | TaskState::Closed, TaskOutcome::Partial) => {
            Some(TaskEndEffect::Partial {
                note: format!("task {task_name} partial"),
            })
        }
        (TaskState::Merged, TaskOutcome::Done) => Some(TaskEndEffect::Finish { note: merged() }),
        (TaskState::Closed, TaskOutcome::Done) => Some(TaskEndEffect::Finish {
            note: if merged_urls.is_empty() {
                format!("task {task_name}, no pull request")
            } else {
                merged()
            },
        }),
        (TaskState::Dropped | TaskState::Failed, _) if !merged_urls.is_empty() => {
            Some(TaskEndEffect::Finish { note: merged() })
        }
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

fn epic_task_end(state: TaskState, task_name: &str) -> Option<TaskEndEffect> {
    match state {
        TaskState::Merged | TaskState::Closed => Some(TaskEndEffect::Release {
            note: format!("task {task_name} ended, the epic stays in progress"),
        }),
        TaskState::Dropped => Some(TaskEndEffect::Lost {
            note: format!("task {task_name} dropped"),
        }),
        TaskState::Failed => Some(TaskEndEffect::Lost {
            note: format!("task {task_name} failed"),
        }),
        TaskState::Queued
        | TaskState::Spawning
        | TaskState::Working
        | TaskState::NeedsUser
        | TaskState::InReview
        | TaskState::Closing
        | TaskState::Interrupted
        | TaskState::Parking
        | TaskState::Parked => None,
    }
}

/// What a task's state does to an item linked to it that is not in progress (`docs/backlog.md`, section 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedEnd {
    /// A done item goes back to in progress: its task, merged, opened another pull request.
    Unfinish,
    /// The link ends and the status stays; a done item's note becomes this one, naming every merged pull request.
    Unlink {
        /// The new done note, for a done item.
        note: Option<String>,
    },
}

/// What a task that moves to `state` does to a linked item of status `status` other than in progress.
///
/// Back in review, a done item (finished when the task merged) is in progress again; closed, dropped or failed, a done
/// item loses its link and its note names every merged pull request, and a blocked one loses its link and keeps its block.
#[must_use]
pub fn on_linked_end(
    state: TaskState,
    status: ItemStatus,
    merged_urls: &[String],
    task_name: &str,
) -> Option<LinkedEnd> {
    let over = matches!(
        state,
        TaskState::Closed | TaskState::Dropped | TaskState::Failed
    );
    match status {
        ItemStatus::Done if state == TaskState::InReview => Some(LinkedEnd::Unfinish),
        ItemStatus::Done if over => Some(LinkedEnd::Unlink {
            note: Some(if merged_urls.is_empty() {
                format!("task {task_name}, no pull request")
            } else {
                format!("task {task_name} merged {}", merged_urls.join(", "))
            }),
        }),
        ItemStatus::Blocked if over => Some(LinkedEnd::Unlink { note: None }),
        ItemStatus::Backlog
        | ItemStatus::InProgress
        | ItemStatus::Blocked
        | ItemStatus::Done
        | ItemStatus::Cancelled => None,
    }
}
