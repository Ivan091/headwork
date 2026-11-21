//! Leads and their parts (`docs/leads.md`): a task's shape, the slots a lead's parts may take, and the queue order
//! that keeps a waiting part right behind its lead.
//!
//! A task is a tree of depth one: a lead may have parts, and a part has exactly one lead and no parts of its own.
//! The store keeps this in two columns, `tasks.parent_id` and `tasks.lead`; [`TaskShape::from_columns`] is the only
//! way from them to a shape, and refuses the one combination that cannot be: a part that is also a lead.

use std::cmp::Ordering;

use crate::vocabulary::TaskState;

/// What a task is in the lead tree. `P` is the store's task id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TaskShape<P> {
    /// A top-level task that may start parts.
    Lead,
    /// A session a lead started for one piece of its task.
    Part {
        /// The lead.
        parent: P,
    },
    /// A task outside the lead tree, as every task was before leads: no parent, no parts.
    Plain,
}

/// Why two columns do not make a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ShapeError {
    /// A task with a parent is marked a lead: parts do not start parts.
    #[error("a part (a task with a parent) cannot be a lead: parts do not start parts")]
    PartIsLead,
}

impl<P: Copy> TaskShape<P> {
    /// The shape stored as `parent_id` and `lead`.
    ///
    /// # Errors
    ///
    /// Returns [`ShapeError::PartIsLead`] for a parent together with `lead`.
    pub const fn from_columns(parent: Option<P>, lead: bool) -> Result<Self, ShapeError> {
        match (parent, lead) {
            (Some(_), true) => Err(ShapeError::PartIsLead),
            (Some(parent), false) => Ok(Self::Part { parent }),
            (None, true) => Ok(Self::Lead),
            (None, false) => Ok(Self::Plain),
        }
    }

    /// The `parent_id` column.
    #[must_use]
    pub const fn parent(self) -> Option<P> {
        match self {
            Self::Part { parent } => Some(parent),
            Self::Lead | Self::Plain => None,
        }
    }

    /// The `lead` column.
    #[must_use]
    pub const fn is_lead(self) -> bool {
        matches!(self, Self::Lead)
    }

    /// Whether the task is a part.
    #[must_use]
    pub const fn is_part(self) -> bool {
        matches!(self, Self::Part { .. })
    }
}

/// Whether a part still keeps its lead from closing: every state but `closed`. A dropped or failed part still holds
/// its worktree and its tab until it is closed, so the lead waits for that too.
#[must_use]
pub const fn part_keeps_lead_open(state: TaskState) -> bool {
    state.holds_name()
}

/// Facts for a part's start: how many of its lead's parts take a slot now, and how many may.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartSlots {
    /// The lead's other parts that take a worker slot now.
    pub live: u32,
    /// `[leads] max_parts`.
    pub max_parts: u32,
}

impl PartSlots {
    /// Whether one more part fits.
    #[must_use]
    pub const fn has_room(self) -> bool {
        self.live < self.max_parts
    }
}

/// Where a task stands in the queue. A part queues under its lead's place: `lead` carries the lead's own entry key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry<P> {
    /// The task.
    pub id: P,
    /// Its own place.
    pub key: QueueKey<P>,
    /// For a part, its lead's place; a part waits right behind its lead, ahead of tasks queued after the lead.
    pub lead: Option<QueueKey<P>>,
}

/// One task's place in the queue: ranked ones first by rank, then the rest oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueKey<P> {
    /// The rank `queue --order` gave it.
    pub rank: Option<i64>,
    /// When it was queued, as the store writes it (sortable text).
    pub queued_at: String,
    /// The task id, the last tie-break.
    pub id: P,
}

impl<P: Ord> QueueKey<P> {
    fn compare(&self, other: &Self) -> Ordering {
        let unranked = |key: &Self| key.rank.is_none();
        unranked(self)
            .cmp(&unranked(other))
            .then_with(|| self.rank.cmp(&other.rank))
            .then_with(|| self.queued_at.cmp(&other.queued_at))
            .then_with(|| self.id.cmp(&other.id))
    }
}

impl<P: Ord> QueueEntry<P> {
    /// The key the entry sorts by first: its lead's for a part, its own otherwise.
    fn group(&self) -> &QueueKey<P> {
        self.lead.as_ref().unwrap_or(&self.key)
    }

    fn compare(&self, other: &Self) -> Ordering {
        self.group()
            .compare(other.group())
            .then_with(|| self.lead.is_some().cmp(&other.lead.is_some()))
            .then_with(|| self.key.compare(&other.key))
    }
}

/// The queue in start order.
///
/// A task without a lead sorts by its own rank, then age; a part sorts at its lead's place, right behind the lead
/// itself and every earlier part of the same lead, so a started task finishes before tasks queued after it start.
#[must_use]
pub fn queue_order<P: Ord + Copy>(mut entries: Vec<QueueEntry<P>>) -> Vec<P> {
    entries.sort_by(QueueEntry::compare);
    entries.into_iter().map(|entry| entry.id).collect()
}

/// What a part's tab shows for `{part}`: its slug after its lead's slug and a `-`, or its whole slug when it does not
/// start that way.
#[must_use]
pub fn part_label<'a>(slug: &'a str, lead_slug: &str) -> &'a str {
    slug.strip_prefix(lead_slug)
        .and_then(|rest| rest.strip_prefix('-'))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(slug)
}

/// Whether a part has lost its lead.
///
/// The part is not closing or closed, while its lead is over (`closing`, `closed`, `dropped`) or `failed`. The tick
/// reports such a part as `orphan-part`; nobody else would close it.
#[must_use]
pub const fn orphaned(part: TaskState, lead: TaskState) -> bool {
    let part_open = !matches!(part, TaskState::Closing | TaskState::Closed);
    let lead_gone = matches!(
        lead,
        TaskState::Closing | TaskState::Closed | TaskState::Dropped | TaskState::Failed
    );
    part_open && lead_gone
}

/// One task's facts for [`status`]: its state, whether an ask of it waits on the user, and whether it was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusFacts {
    /// The task's state.
    pub state: TaskState,
    /// An ask of the task waits on the user.
    pub asking: bool,
    /// The task was dropped at some point (a closed task keeps no other trace of it).
    pub dropped: bool,
}

impl StatusFacts {
    const fn needs_user(self) -> bool {
        self.asking || matches!(self.state, TaskState::NeedsUser)
    }
}

/// A top-level task's one-line status (`docs/leads.md`, section 4), rolled up from its parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    /// Waiting in the queue.
    Queued,
    /// The lead or a part works.
    Working,
    /// The lead or a part waits on the user.
    NeedsYou,
    /// The lead's pull request is open.
    InReview,
    /// Merged, closing or closed after its work landed.
    Finished,
    /// The session never started.
    Failed,
    /// The session went away without `done`; a resume brings it back.
    Interrupted,
    /// Stopped for a restart.
    Parked,
    /// Dropped by the user or the orchestrator.
    Dropped,
}

impl TaskStatus {
    /// The words the orchestrator tells the user.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Working => "working",
            Self::NeedsYou => "needs you",
            Self::InReview => "in review",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Parked => "parked",
            Self::Dropped => "dropped",
        }
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A top-level task's status from its own facts and its parts'.
///
/// The lead's end decides first (dropped, failed, interrupted, parked, closed); while it is open, any ask of the lead
/// or a part makes it `needs you`, ahead of the queue, the review and a merge waiting for its parts.
#[must_use]
pub fn status(task: StatusFacts, parts: &[StatusFacts]) -> TaskStatus {
    if task.dropped {
        return TaskStatus::Dropped;
    }
    match task.state {
        TaskState::Dropped => return TaskStatus::Dropped,
        TaskState::Failed => return TaskStatus::Failed,
        TaskState::Interrupted => return TaskStatus::Interrupted,
        TaskState::Parking | TaskState::Parked => return TaskStatus::Parked,
        TaskState::Closed => return TaskStatus::Finished,
        TaskState::Queued
        | TaskState::Spawning
        | TaskState::Working
        | TaskState::NeedsUser
        | TaskState::InReview
        | TaskState::Merged
        | TaskState::Closing => {}
    }
    let open_parts: Vec<&StatusFacts> = parts
        .iter()
        .filter(|part| !matches!(part.state, TaskState::Closing | TaskState::Closed))
        .collect();
    if task.needs_user() || open_parts.iter().any(|part| part.needs_user()) {
        return TaskStatus::NeedsYou;
    }
    match task.state {
        TaskState::Queued => TaskStatus::Queued,
        TaskState::InReview => TaskStatus::InReview,
        TaskState::Merged | TaskState::Closing if open_parts.is_empty() => TaskStatus::Finished,
        TaskState::Spawning
        | TaskState::Working
        | TaskState::NeedsUser
        | TaskState::Merged
        | TaskState::Closing
        | TaskState::Closed
        | TaskState::Dropped
        | TaskState::Failed
        | TaskState::Interrupted
        | TaskState::Parking
        | TaskState::Parked => TaskStatus::Working,
    }
}
