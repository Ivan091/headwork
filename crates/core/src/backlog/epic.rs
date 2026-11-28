//! The epic explorer's spin-up (`docs/backlog.md`, section 12): the slug of its task, the name of its plan notes file and
//! the brief it starts from; and the epic runner (`docs/backlog.md`, section 13): the slug and brief of a child's task, the
//! re-plan child's brief, and [`epic_step`], the decision what one epic does in one pass.
//!
//! Pure: the binary reads the epic from the store, writes the brief to a file and spawns the task.

use std::fmt::Write as _;

use crate::backlog::{BacklogDate, BacklogStatus, BlockReason, ItemStatus};
use crate::guard::grants::explore_report_check;
use crate::guard::{GuardPath, Places};

/// How many characters of the epic's text the short slug keeps.
const SHORT_CHARS: usize = 24;

/// How many words of the epic's text the short slug keeps.
const SHORT_WORDS: usize = 4;

/// The first words of `text` in kebab case: ASCII letters and digits, lower case, at most four words and 24 characters,
/// never ending in a hyphen; empty when the text has no such word.
#[must_use]
pub fn short_slug(text: &str) -> String {
    let mut out = String::new();
    let words = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(SHORT_WORDS);
    for word in words {
        let word = word.to_ascii_lowercase();
        let extra = usize::from(!out.is_empty());
        if out.len() + extra + word.len() > SHORT_CHARS {
            if out.is_empty() {
                out.extend(word.chars().take(SHORT_CHARS));
            }
            break;
        }
        if extra == 1 {
            out.push('-');
        }
        out.push_str(&word);
    }
    out
}

/// The slug of the explorer task of epic `id`: `epic-<id>-<short slug of the text>`, or `epic-<id>` when the text gives
/// no word.
#[must_use]
pub fn epic_slug(id: i64, text: &str) -> String {
    match short_slug(text) {
        short if short.is_empty() => format!("epic-{id}"),
        short => format!("epic-{id}-{short}"),
    }
}

/// The file name of epic `id`'s plan notes, under `<brief_writable root>/epics/`: `<id>-<short slug>.md`, or `<id>.md`.
#[must_use]
pub fn plan_file_name(id: i64, text: &str) -> String {
    match short_slug(text) {
        short if short.is_empty() => format!("{id}.md"),
        short => format!("{id}-{short}.md"),
    }
}

/// What the explorer's brief is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpicBrief<'a> {
    /// The epic's id.
    pub id: i64,
    /// The epic's text.
    pub text: &'a str,
    /// The epic's notes, one line each.
    pub notes: &'a [String],
    /// The epic's project.
    pub project: &'a str,
    /// The plan notes file, on the brief's `Writes:` line.
    pub plan_path: &'a str,
}

/// The brief of epic `brief.id`'s explorer: a `Goal:` line, the epic's text and notes, the project and a `Writes:` line
/// naming the plan notes file.
#[must_use]
pub fn epic_brief(brief: &EpicBrief<'_>) -> String {
    let EpicBrief {
        id,
        text,
        notes,
        project,
        plan_path,
    } = *brief;
    let mut out = format!(
        "Goal: Plan epic #{id}: {text}\n\n\
         You explore epic #{id} of project {project}, read only. Plan it as your rules say (\"You explore an epic\"): write \
         the plan to the file below, create the epic's children with `plan_add`, order them, and check them with \
         `plan_list`. The user's spin-up of the epic is the go for the whole plan: no `Backlog` ask.\n\n\
         ## The epic\n\n#{id} {text}\n"
    );
    if !notes.is_empty() {
        out.push('\n');
        for note in notes {
            let _infallible = writeln!(out, "- {note}");
        }
    }
    let _infallible = write!(out, "\nProject: {project}\n\nWrites: {plan_path}\n");
    out
}

/// The slug of the task of child `child` of epic `epic`: `e<epic>-<child>-<short slug of the text>`, or `e<epic>-<child>`
/// when the text gives no word.
#[must_use]
pub fn child_slug(epic: i64, child: i64, text: &str) -> String {
    match short_slug(text) {
        short if short.is_empty() => format!("e{epic}-{child}"),
        short => format!("e{epic}-{child}-{short}"),
    }
}

/// What the brief of a child added without one is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildBrief<'a> {
    /// The child's id.
    pub id: i64,
    /// The child's text.
    pub text: &'a str,
    /// The child's notes, one line each.
    pub notes: &'a [String],
    /// The epic's id.
    pub epic: i64,
    /// The epic's text.
    pub epic_text: &'a str,
    /// The child's project.
    pub project: &'a str,
}

/// The brief of a child that has no stored one (the user added it with `--parent`): a `Goal:` line from its text, its
/// notes, the epic it belongs to and the project.
#[must_use]
pub fn child_brief(brief: &ChildBrief<'_>) -> String {
    let ChildBrief {
        id,
        text,
        notes,
        epic,
        epic_text,
        project,
    } = *brief;
    let mut out = format!(
        "Goal: {text}\n\nBacklog item #{id}, a child of epic #{epic}: {epic_text}. The epic runner started it once every \
         item it depends on was done.\n"
    );
    if !notes.is_empty() {
        out.push('\n');
        for note in notes {
            let _infallible = writeln!(out, "- {note}");
        }
    }
    let _infallible = write!(out, "\nProject: {project}\n");
    out
}

/// `brief`, with a `Writes:` line naming `path` appended when the report guard would refuse it.
///
/// The `explore-no-report` guard's own rule ([`explore_report_check`] under `roots`) decides, so the spawn of an explore
/// child the runner starts is never refused for its report.
#[must_use]
pub fn with_writes_line(brief: &str, path: &str, roots: &[GuardPath], places: &Places) -> String {
    if explore_report_check(brief, roots, places).is_ok() {
        brief.to_owned()
    } else {
        format!("{}\n\nWrites: {path}\n", brief.trim_end())
    }
}

/// One child whose block caused a re-plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplanCause {
    /// The child.
    pub child: i64,
    /// Its text.
    pub text: String,
    /// Why it is blocked: `task_lost`, `partial` or `dep_cancelled`.
    pub reason: BlockReason,
    /// The block's note.
    pub note: Option<String>,
    /// The id of the `backlog-moved` event that blocked it; none for a block no event records (an import).
    pub event: Option<i64>,
}

/// Why the runner re-plans an epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplanTrigger {
    /// Children are blocked for a reason a plan has to answer.
    Blocked(Vec<ReplanCause>),
    /// No child is open: is the plan complete?
    PlanComplete,
}

impl ReplanTrigger {
    /// The reason, one line: `#12 task_lost, #14 partial` or `plan complete?`.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Blocked(causes) => causes
                .iter()
                .map(|cause| format!("#{} {}", cause.child, cause.reason))
                .collect::<Vec<_>>()
                .join(", "),
            Self::PlanComplete => "plan complete?".to_owned(),
        }
    }

    /// The text of the re-plan child: `Re-plan: <reason>`.
    #[must_use]
    pub fn child_text(&self) -> String {
        format!("Re-plan: {}", self.reason())
    }

    /// The causes; empty for [`ReplanTrigger::PlanComplete`].
    #[must_use]
    pub fn causes(&self) -> &[ReplanCause] {
        match self {
            Self::Blocked(causes) => causes,
            Self::PlanComplete => &[],
        }
    }
}

/// What a re-plan child's brief is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplanBrief<'a> {
    /// The epic's id.
    pub epic: i64,
    /// The epic's text.
    pub epic_text: &'a str,
    /// The epic's project.
    pub project: &'a str,
    /// Why the re-plan runs.
    pub trigger: &'a ReplanTrigger,
    /// The plan notes file, on the brief's `Writes:` line.
    pub plan_path: &'a str,
}

/// The brief of a re-plan child: a `Goal:` line, the reason, each cause with its note, what to do, the project and a
/// `Writes:` line naming the epic's plan notes file.
#[must_use]
pub fn replan_brief(brief: &ReplanBrief<'_>) -> String {
    let ReplanBrief {
        epic,
        epic_text,
        project,
        trigger,
        plan_path,
    } = *brief;
    let why = match trigger {
        ReplanTrigger::Blocked(_) => "children of the epic are blocked",
        ReplanTrigger::PlanComplete => "no child of the epic is open",
    };
    let mut out = format!(
        "Goal: Re-plan epic #{epic}: {reason}\n\n\
         You explore epic #{epic} of project {project} again, read only: {epic_text}. The epic runner started you because \
         {why}. Read the plan notes file below and `plan_list`, then change the plan as your rules say (\"You explore an \
         epic\"): add, edit, order or cancel children, or finish the epic with `plan_finish` when nothing is left to do. \
         Note in the plan notes file what you changed and why.\n",
        reason = trigger.reason(),
    );
    if !trigger.causes().is_empty() {
        out.push_str("\n## Blocked children\n\n");
        for cause in trigger.causes() {
            let _infallible = match &cause.note {
                Some(note) => writeln!(
                    out,
                    "- #{} {}: {} ({note})",
                    cause.child, cause.text, cause.reason
                ),
                None => writeln!(out, "- #{} {}: {}", cause.child, cause.text, cause.reason),
            };
        }
    }
    let _infallible = write!(out, "\nProject: {project}\n\nWrites: {plan_path}\n");
    out
}

/// One child of an epic, as the runner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpicChild {
    /// Its id.
    pub id: i64,
    /// Its text.
    pub text: String,
    /// Where it stands.
    pub status: BacklogStatus,
    /// Whether its task kind is `explore`: a re-plan, or a planned exploration.
    pub explore: bool,
    /// Whether an agent acts on it; a child the user owns is theirs to do.
    pub agent: bool,
    /// The first day it may start.
    pub not_before: Option<BacklogDate>,
    /// Why the runner may not start it, when it may not: its project is not one headwork runs or not the epic's, or
    /// its task kind is not one the runner starts ([`child_kind_refusal`]).
    pub skip: Option<String>,
    /// The id of the `backlog-moved` event into its current block, when blocked.
    pub block_event: Option<i64>,
    /// Whether its current block already caused a re-plan.
    pub block_replanned: bool,
}

/// What the runner knows about one epic in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpicFacts {
    /// The epic's id.
    pub epic: i64,
    /// Whether a live task explores it: one whose `epic_id` is the epic, or the live task linked to the epic itself.
    pub explorer_live: bool,
    /// Its children, in export order.
    pub children: Vec<EpicChild>,
    /// Its re-plans counted since the last churn block (`epic_replans` rows not cleared).
    pub replans: u32,
    /// Today.
    pub today: BacklogDate,
}

/// The limits one runner pass works under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpicLimits {
    /// `[epics] max_parallel`: children of one epic in progress at once, explore children aside; 0 is no limit.
    pub max_parallel: u32,
    /// `[epics] max_replans`: counted re-plans before the epic is blocked for a decision; 0 is no limit.
    pub max_replans: u32,
    /// How many starts the pass still has before it reaches `[watch] many_sessions`; `None` when that is 0 (off).
    pub room: Option<usize>,
}

/// What the runner does with one epic in one pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpicStep {
    /// An explorer of the epic is live: nothing starts and no re-plan fires until it ends.
    Hold,
    /// Start children, in order.
    Run {
        /// The children to start.
        starts: Vec<i64>,
        /// Ready children the runner may not start, each with why.
        skipped: Vec<(i64, String)>,
        /// Whether a ready child waits for room under `[watch] many_sessions`.
        held: bool,
    },
    /// Add a re-plan child for this trigger and start it.
    Replan(ReplanTrigger),
    /// The epic has had `max_replans` re-plans: block it for the user's decision instead.
    Churn {
        /// The counted re-plans.
        replans: u32,
    },
}

/// Why the runner does not start a child of task kind `kind`, if it does not.
///
/// `review` and `silent` never (a reviewer's forced worktree removal, a close without the `Close` ask), nor a kind outside `[epics] kinds` (`allowed`), nor one
/// main-only in the child's project (`main_only`) or among the kinds of its `extra_writable` (`extra_writable`): those
/// widen what a worker may do, and an explorer's plan alone is no go for that.
#[must_use]
pub fn child_kind_refusal(
    kind: &str,
    allowed: &[String],
    main_only: bool,
    extra_writable: bool,
) -> Option<String> {
    if kind == crate::config::REVIEW_KIND || kind == crate::config::SILENT_KIND {
        Some(format!("kind `{kind}` is never started by the epic runner"))
    } else if !allowed.iter().any(|own| own == kind) {
        Some(format!("kind `{kind}` is not in `[epics] kinds`"))
    } else if main_only {
        Some(format!("kind `{kind}` is main-only in the child's project"))
    } else if extra_writable {
        Some(format!(
            "kind `{kind}` gets the project's `extra_writable` paths"
        ))
    } else {
        None
    }
}

/// Whether a child counts against `[epics] max_parallel`: every task kind but `explore`.
#[must_use]
pub const fn counts_against_parallel(child: &EpicChild) -> bool {
    !child.explore
}

/// Whether a child holds the epic's re-plan triggers: an `explore` child in the backlog or in progress.
///
/// That is a re-plan about to run or running. A blocked explore child (lost, partial, waiting on a dependency) holds nothing, and its block is a
/// cause like any child's. The store's re-plan recheck uses the same rule.
#[must_use]
pub const fn holds_triggers(explore: bool, status: ItemStatus) -> bool {
    explore && matches!(status, ItemStatus::Backlog | ItemStatus::InProgress)
}

/// Whether a child holds the whole epic like a live explorer: an `explore` child in progress, claimed or running, whose
/// task may not exist yet.
#[must_use]
pub fn holds_epic(child: &EpicChild) -> bool {
    child.explore && child.status == BacklogStatus::InProgress
}

#[must_use]
fn ready(child: &EpicChild, today: &BacklogDate) -> bool {
    child.status == BacklogStatus::Backlog
        && child.agent
        && child.not_before.as_ref().is_none_or(|from| from <= today)
}

#[must_use]
fn trigger(facts: &EpicFacts) -> Option<ReplanTrigger> {
    let held = facts
        .children
        .iter()
        .any(|child| holds_triggers(child.explore, child.status.kind()));
    if held {
        return None;
    }
    let causes: Vec<ReplanCause> = facts
        .children
        .iter()
        .filter(|child| !child.block_replanned)
        .filter_map(|child| {
            let block = child.status.block()?;
            matches!(
                block.reason,
                BlockReason::TaskLost | BlockReason::Partial | BlockReason::DepCancelled
            )
            .then(|| ReplanCause {
                child: child.id,
                text: child.text.clone(),
                reason: block.reason,
                note: block.note.clone(),
                event: child.block_event,
            })
        })
        .collect();
    if !causes.is_empty() {
        return Some(ReplanTrigger::Blocked(causes));
    }
    (!facts.children.iter().any(|child| child.status.is_open()))
        .then_some(ReplanTrigger::PlanComplete)
}

/// The runner's decision for one epic in progress (`docs/backlog.md`, section 13).
///
/// A live explorer, or an explore child in progress (claimed, its task perhaps not created yet), holds everything.
/// Otherwise, with no explore child in the backlog ([`holds_triggers`]), a child blocked `task_lost`, `partial` or
/// `dep_cancelled` whose block caused no re-plan yet (an explore child too), else no open child at all, triggers a
/// re-plan, or the churn block once `max_replans` re-plans are counted. Without a trigger, ready children start: in the
/// backlog (so every dependency is done), owned by an agent, past `not_before`, in a project headwork runs. A ready
/// explore child starts alone; other children start while fewer than `max_parallel` non-explore children are in
/// progress, and never more than `room`.
#[must_use]
pub fn epic_step(facts: &EpicFacts, limits: &EpicLimits) -> EpicStep {
    if facts.explorer_live || facts.children.iter().any(holds_epic) {
        return EpicStep::Hold;
    }
    if let Some(trigger) = trigger(facts) {
        if limits.max_replans != 0 && facts.replans >= limits.max_replans {
            return EpicStep::Churn {
                replans: facts.replans,
            };
        }
        return EpicStep::Replan(trigger);
    }
    let (ready, skipped): (Vec<&EpicChild>, Vec<&EpicChild>) = facts
        .children
        .iter()
        .filter(|child| ready(child, &facts.today))
        .partition(|child| child.skip.is_none());
    let skipped = skipped
        .iter()
        .map(|child| (child.id, child.skip.clone().unwrap_or_default()))
        .collect();
    let room = limits.room.unwrap_or(usize::MAX);
    if let Some(explore) = ready.iter().find(|child| child.explore) {
        let starts = if room == 0 {
            Vec::new()
        } else {
            vec![explore.id]
        };
        return EpicStep::Run {
            starts,
            skipped,
            held: room == 0,
        };
    }
    let running = facts
        .children
        .iter()
        .filter(|child| counts_against_parallel(child) && child.status == BacklogStatus::InProgress)
        .count();
    let free = if limits.max_parallel == 0 {
        usize::MAX
    } else {
        usize::try_from(limits.max_parallel)
            .unwrap_or(usize::MAX)
            .saturating_sub(running)
    };
    let wanted = ready.len().min(free);
    let starts: Vec<i64> = ready
        .iter()
        .take(wanted.min(room))
        .map(|child| child.id)
        .collect();
    EpicStep::Run {
        held: starts.len() < wanted,
        starts,
        skipped,
    }
}
