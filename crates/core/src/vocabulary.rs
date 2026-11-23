//! Closed sets of words stored as text: task and machine states, models, actors.
//!
//! A word may have older spellings after `|`: they still parse, from a store, an environment or an event log written
//! before a rename, and are never written. The orchestrator and its workers were called root and children until
//! 2026-11-14.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A word that is not one of an enum's known spellings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} `{word}`")]
pub struct UnknownWord {
    /// What was being parsed, such as "task state".
    pub kind: &'static str,
    /// The word that did not match.
    pub word: String,
}

macro_rules! text_enum {
    (
        $(#[$doc:meta])*
        $name:ident, $kind:literal {
            $($(#[$vdoc:meta])* $variant:ident => $text:literal $(| $alias:literal)*,)+
        }
    ) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(
                $(#[$vdoc])*
                #[serde(rename = $text $(, alias = $alias)*)]
                $variant,
            )+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The stored spelling.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.pad(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = UnknownWord;

            fn from_str(word: &str) -> Result<Self, Self::Err> {
                match word {
                    $($text $(| $alias)* => Ok(Self::$variant),)+
                    other => Err(UnknownWord { kind: $kind, word: other.to_owned() }),
                }
            }
        }
    };
}

pub(crate) use text_enum;

text_enum!(
    /// Where a task is in its lifecycle (design section 2.3).
    TaskState, "task state" {
        /// Waiting for a slot, its dependency or the Fable approval.
        Queued => "queued",
        /// A pane and launcher are starting; no `SessionStart` seen yet.
        Spawning => "spawning",
        /// The session runs.
        Working => "working",
        /// The session waits on the user.
        NeedsUser => "needs_user",
        /// A pull request is open or a review was reported.
        InReview => "in_review",
        /// The pull request is merged.
        Merged => "merged",
        /// Pane, process tree and worktree are being removed.
        Closing => "closing",
        /// Everything is gone and verified.
        Closed => "closed",
        /// Dropped by the orchestrator or the user; closing follows at once.
        Dropped => "dropped",
        /// The session never started.
        Failed => "failed",
        /// The process went away without `done` or a park.
        Interrupted => "interrupted",
        /// Reaching a safe point before a restart.
        Parking => "parking",
        /// Stopped for a restart, waiting for resume.
        Parked => "parked",
    }
);

impl TaskState {
    /// Whether the task still holds its name: every state but `closed`, as in the store's unique index. A dropped task
    /// keeps it until its worktree and pane are gone, so a new task cannot collide with them.
    #[must_use]
    pub const fn holds_name(self) -> bool {
        !matches!(self, Self::Closed)
    }

    /// Whether the task occupies a worker slot: a session is starting, running, or being closed.
    #[must_use]
    pub const fn takes_slot(self) -> bool {
        match self {
            Self::Spawning
            | Self::Working
            | Self::NeedsUser
            | Self::InReview
            | Self::Merged
            | Self::Closing
            | Self::Parking => true,
            Self::Queued
            | Self::Closed
            | Self::Dropped
            | Self::Failed
            | Self::Interrupted
            | Self::Parked => false,
        }
    }

    /// Whether the task occupies a worker slot, knowing whether its newest session is over (ended, gone, or never
    /// started): a task in `closing` whose session is over runs nothing and only waits on its worktree, so it frees its
    /// slot.
    #[must_use]
    pub const fn holds_slot(self, session_over: bool) -> bool {
        self.takes_slot() && !(session_over && matches!(self, Self::Closing))
    }
}

text_enum!(
    /// The machine-wide mode (design section 6).
    MachineMode, "machine mode" {
        /// Everything runs.
        Normal => "normal",
        /// A park is in progress; spawns and the queue refuse.
        Parking => "parking",
        /// Everything is stopped; safe to restart.
        Parked => "parked",
        /// A resume is in progress.
        Resuming => "resuming",
    }
);

text_enum!(
    /// A Claude model a session may run.
    Model, "model" {
        /// Lookups and trivial runs.
        Haiku => "haiku",
        /// Small tweaks and routine edits.
        Sonnet => "sonnet",
        /// Substantial or delicate work.
        Opus => "opus",
        /// Only with the user's approval.
        Fable => "fable",
    }
);

text_enum!(
    /// How much effort a Claude session spends (`claude --effort`).
    Effort, "effort" {
        /// The least effort.
        Low => "low",
        /// Moderate effort.
        Medium => "medium",
        /// High effort.
        High => "high",
        /// Higher than high.
        Xhigh => "xhigh",
        /// The most effort.
        Max => "max",
    }
);

text_enum!(
    /// Who caused an event.
    Actor, "actor" {
        /// The orchestrator session.
        Orchestrator => "orchestrator" | "root",
        /// A worker session.
        Worker => "worker" | "child",
        /// A person at the CLI.
        User => "user",
        /// The watchdog tick.
        Tick => "tick",
        /// A Claude Code hook.
        Hook => "hook",
    }
);

text_enum!(
    /// What kind of thing an ask waits on.
    AskKind, "ask kind" {
        /// A question with options.
        Question => "question",
        /// A review of a render, preview or pull request.
        Review => "review",
        /// A permission prompt.
        Permission => "permission",
        /// A decision the tool itself needs, such as a Fable approval.
        Tool => "tool",
    }
);

text_enum!(
    /// Where an ask is.
    AskState, "ask state" {
        /// Stored, not yet put to the user.
        Pending => "pending",
        /// Put to the user.
        Open => "open",
        /// Answered.
        Answered => "answered",
        /// Timed out.
        Expired => "expired",
        /// Replaced by a newer ask, for example after a resume.
        Superseded => "superseded",
    }
);

impl AskState {
    /// Whether the ask still waits on the user.
    #[must_use]
    pub const fn is_waiting(self) -> bool {
        matches!(self, Self::Pending | Self::Open)
    }
}

text_enum!(
    /// Where a session's process is.
    SessionState, "session state" {
        /// Launched, no `SessionStart` yet.
        Starting => "starting",
        /// Running.
        Live => "live",
        /// Ended normally.
        Ended => "ended",
        /// Vanished without an end.
        Gone => "gone",
    }
);

text_enum!(
    /// Where a registered long-running job is.
    JobState, "job state" {
        /// The process runs.
        Running => "running",
        /// A park asked it to stop and waits.
        Stopping => "stopping",
        /// Stopped for a restart; `resume` runs it again when it asked to be restarted.
        Parked => "parked",
        /// Finished.
        Done => "done",
        /// Could not be restarted.
        Failed => "failed",
    }
);

text_enum!(
    /// Why a recap was written.
    RecapKind, "recap kind" {
        /// The session wrote it itself.
        Session => "session",
        /// The tool wrote it from the store because the session could not.
        Mechanical => "mechanical",
        /// Written as a task closed.
        Close => "close",
        /// Written when the tick found the session gone.
        Interrupted => "interrupted",
    }
);

text_enum!(
    /// The permission mode a session starts in, as `claude --permission-mode` spells it.
    PermissionMode, "permission mode" {
        /// Auto mode: a classifier reviews risky actions.
        Auto => "auto",
        /// Plan mode: read-only until a plan is approved.
        Plan => "plan",
        /// Every risky action asks.
        Default => "default",
        /// File edits pass without asking.
        AcceptEdits => "acceptEdits",
    }
);

text_enum!(
    /// Where a pull request is, as `prs.state` holds it.
    PullState, "pull request state" {
        /// Open, its gate running or done.
        Open => "open",
        /// Merged.
        Merged => "merged",
        /// Closed unmerged.
        Closed => "closed",
    }
);

text_enum!(
    /// Where a review is (`docs/review-flows.md`, section 7).
    ReviewState, "review state" {
        /// The user asked for it; the lead has not started it.
        Requested => "requested",
        /// The reviewer lead runs the flow's nodes.
        Running => "running",
        /// The findings went to the implementing lead.
        Delivered => "delivered",
        /// The reviewer lead re-checks the fixes and weighs the disputes.
        Rechecking => "rechecking",
        /// Disputes the reviewer lead could not settle wait on the user.
        Asking => "asking",
        /// Finished.
        Done => "done",
        /// Ended before it finished: the lead closed or the user dropped the review part.
        Cancelled => "cancelled",
    }
);

impl ReviewState {
    /// Whether the review is still open: every state but `done` and `cancelled`.
    #[must_use]
    pub const fn is_open(self) -> bool {
        !matches!(self, Self::Done | Self::Cancelled)
    }
}

text_enum!(
    /// Where one node instance of a review round is.
    NodeState, "review node state" {
        /// Its inputs are done; it waits for the reviewer lead to take it.
        Ready => "ready",
        /// Handed to a subagent.
        Running => "running",
        /// Its findings or verdict are in.
        Done => "done",
        /// The subagent failed; the round goes on without it.
        Failed => "failed",
    }
);

impl NodeState {
    /// Whether the instance is over, its result in or given up.
    #[must_use]
    pub const fn is_over(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }
}

text_enum!(
    /// How much a review finding weighs.
    Severity, "finding severity" {
        /// A defect with a concrete failing scenario.
        Important => "important",
        /// A minor issue.
        Nit => "nit",
        /// A doubt about the approach or the scope.
        Doubt => "doubt",
    }
);

text_enum!(
    /// What a verify node concluded about one finding.
    FindingVerdict, "finding verdict" {
        /// The finding holds.
        Confirmed => "confirmed",
        /// The finding may hold; it goes on.
        Plausible => "plausible",
        /// The finding does not hold; it is dropped.
        Rejected => "rejected",
    }
);

impl FindingVerdict {
    /// Whether a finding with this verdict reaches the implementing lead.
    #[must_use]
    pub const fn passes(self) -> bool {
        matches!(self, Self::Confirmed | Self::Plausible)
    }
}

text_enum!(
    /// Where a delivered finding stands between the implementing lead and the reviewer lead.
    Resolution, "finding resolution" {
        /// Delivered; the implementing lead has not answered it.
        Open => "open",
        /// The lead says it fixed it; the re-check is due.
        Fixed => "fixed",
        /// The re-check found the fix good.
        FixAccepted => "fix-accepted",
        /// The lead disputes it with a reason; the reviewer lead weighs it.
        Disputed => "disputed",
        /// The reviewer lead accepted the dispute.
        DisputeAccepted => "dispute-accepted",
        /// The re-check found the fix wanting; the finding is open again.
        FixRejected => "fix-rejected",
        /// The dispute went to the user.
        Asked => "asked",
        /// The user kept the finding.
        UserKept => "user-kept",
        /// The user dropped the finding.
        UserDropped => "user-dropped",
    }
);

impl Resolution {
    /// Whether the finding waits on the implementing lead: open, its fix rejected, or kept by the user.
    #[must_use]
    pub const fn awaits_lead(self) -> bool {
        matches!(self, Self::Open | Self::FixRejected | Self::UserKept)
    }

    /// Whether the finding is settled for good.
    #[must_use]
    pub const fn is_settled(self) -> bool {
        matches!(
            self,
            Self::FixAccepted | Self::DisputeAccepted | Self::UserDropped
        )
    }
}

text_enum!(
    /// A session's role.
    Role, "role" {
        /// The coordinating session.
        Orchestrator => "orchestrator" | "root",
        /// A task session.
        Worker => "worker" | "child",
    }
);

text_enum!(
    /// A hard rule a hook enforces; every decision in the event log names one.
    RuleId, "rule" {
        /// The orchestrator edits a file outside the headwork home, the temp folder and the configured writable folders.
        OrchestratorEdit => "orchestrator-edit" | "root-edit",
        /// The orchestrator commits, pushes, merges or rebases, or merges or opens a pull request.
        OrchestratorGit => "orchestrator-git" | "root-git",
        /// The orchestrator runs a build tool.
        OrchestratorBuild => "orchestrator-build" | "root-build",
        /// The orchestrator runs a project's delivery or worktree script.
        OrchestratorDelivery => "orchestrator-delivery" | "root-delivery",
        /// A session runs a command only the user runs (publish, upload, release, login).
        UserOnly => "user-only",
        /// A worker edits a file outside its worktree, the temp folder and the configured writable folders.
        WorkerEditOutside => "worker-edit-outside" | "child-edit-outside",
        /// A worker edits a file under one of its project's read-only paths.
        WorkerReadOnly => "worker-read-only" | "child-read-only",
        /// A worker commits or pushes on the default branch.
        WorkerDefaultBranch => "worker-default-branch" | "child-default-branch",
        /// A worker pushes to a repository its project does not allow.
        WorkerPushRepo => "worker-push-repo" | "child-push-repo",
        /// A worker uses a global delivery skill where its project names its own delivery command.
        WorkerGlobalSkill => "worker-global-skill" | "child-global-skill",
        /// A worker starts a Fable subagent without the user's approval.
        WorkerFable => "worker-fable" | "child-fable",
        /// A worker starts an Opus subagent without an `Opus because:` line in its prompt.
        SubagentOpus => "subagent-opus",
        /// A worker starts a long job in the background instead of registering it.
        WorkerLongJob => "worker-long-job" | "child-long-job",
        /// A worker runs an orchestrator command of the headwork CLI (spawn, queue, park, close of another task...).
        WorkerAdmin => "worker-admin" | "child-admin",
        /// A worker runs a git write (commit, push, reset...) against a folder outside its writable folders.
        WorkerGitOutside => "worker-git-outside",
        /// A question has fewer than two or more than four options.
        AskOptionCount => "ask-option-count",
        /// An option means "needs changes"; the user types changes instead.
        AskChangesOption => "ask-changes-option",
        /// The recommended option is not the first, or more than one is recommended.
        AskRecommendedFirst => "ask-recommended-first",
        /// No push notification preceded the ask.
        AskPush => "ask-push",
        /// A review ask without the file sent first.
        AskReviewFile => "ask-review-file",
        /// A worker stops while working, without asking, reporting or closing.
        StopUnfinished => "stop-unfinished",
        /// A session ends its turn with a plain-text question.
        StopPlainQuestion => "stop-plain-question",
        /// A reviewer lead (a part of kind `review`) edits a file outside the scratchpad, or runs a git write or a build.
        ReviewerWrite => "reviewer-write",
        /// A lead stops while the review the user asked for has not started.
        ReviewUnstarted => "review-unstarted",
        /// A reviewer lead stops while its review has a ready node or a resolution waits on its verdict.
        ReviewUnfinished => "review-unfinished",
    }
);

text_enum!(
    /// A reconciliation the tick makes or a condition it reports; every tick finding in the event log names one.
    FindingId, "finding" {
        /// A spawning task's session never reported `SessionStart` in time.
        SpawnTimeout => "spawn-timeout",
        /// A running task's pane or launcher is gone.
        PaneLost => "pane-lost",
        /// A running pane carries a task label no task owns.
        OrphanPane => "orphan-pane",
        /// A working task has an open pull request.
        PrOpened => "pr-opened",
        /// A task's pull request was merged.
        PrMerged => "pr-merged",
        /// A task's pull request was closed without a merge.
        PrClosed => "pr-closed",
        /// A merged task's grace period ended and it started closing.
        MergedGrace => "merged-grace",
        /// A closing task still had processes, which the tick ended.
        LeftoverProcess => "leftover-process",
        /// A worktree could not be removed and the user is asked to keep or delete it.
        WorktreeRefused => "worktree-refused",
        /// Free disk space fell below the warning level.
        DiskWarn => "disk-warn",
        /// Free disk space fell below the stop level.
        DiskStop => "disk-stop",
        /// A file grew too fast or is too large.
        Runaway => "runaway",
        /// A working session made no move for too long.
        Stuck => "stuck",
        /// A session runs an older rules version than the machine holds.
        RulesStale => "rules-stale",
        /// An ask waits on the user for too long.
        AskStale => "ask-stale",
        /// A starting session stopped at Claude Code's folder trust prompt and the user is asked.
        TrustPrompt => "trust-prompt",
        /// A part is still open while its lead is closed, closing, dropped or failed.
        OrphanPart => "orphan-part",
        /// A part's tab runs outside its lead's workspace (the user moved it).
        StrayTab => "stray-tab",
        /// The tick could not look at something it checks, such as the git host or a volume.
        TickError => "tick-error",
        /// A tick has held `tick.lock` for longer than the hung threshold, so later runs cannot start.
        TickHung => "tick-hung",
        /// `headwork update` failed in a way worth telling (deterministic, or transient from its third attempt); it
        /// stands until a later release installs. `update` raises and clears it, not the tick.
        UpdateFailed => "update-failed",
    }
);

text_enum!(
    /// How a session sits in its workspace: alone in a pane, or as one tab among others.
    PlaceKind, "place kind" {
        /// A pane of its own.
        Pane => "pane",
        /// A tab: stacked on a pane that holds other sessions, or placed by the host in tabs mode.
        Tab => "tab",
    }
);

text_enum!(
    /// Whose tokens a usage row counts, fixed when the row is written (`docs/usage.md`, section 3).
    UsageRole, "usage role" {
        /// The orchestrator session.
        Orchestrator => "orchestrator",
        /// A lead task's session.
        Lead => "lead",
        /// A part of a lead.
        Part => "part",
        /// A plain worker task's session.
        Worker => "worker",
        /// A helper session a worker opened with `helper_tab`.
        Helper => "helper",
        /// A Claude session headwork did not start.
        Unmanaged => "unmanaged",
    }
);

text_enum!(
    /// What `headwork usage` groups its totals by.
    UsageGroup, "usage grouping" {
        /// The task's project.
        Project => "project",
        /// The task, `<project>-<slug>`.
        Task => "task",
        /// The task's kind.
        Kind => "kind",
        /// The model that answered.
        Model => "model",
        /// The effort level of the turn.
        Effort => "effort",
        /// The usage role.
        Role => "role",
        /// The subagent type; the session itself is `main`.
        Agent => "agent",
        /// The session's folder.
        Folder => "folder",
        /// The UTC day.
        Day => "day",
        /// The Claude session.
        Session => "session",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_round_trips() {
        for state in TaskState::ALL {
            assert_eq!(state.as_str().parse::<TaskState>(), Ok(*state), "{state}");
        }
        for model in Model::ALL {
            assert_eq!(model.as_str().parse::<Model>(), Ok(*model), "{model}");
        }
    }

    #[test]
    fn old_role_spellings_parse_and_new_ones_are_written() {
        assert_eq!("root".parse::<Role>(), Ok(Role::Orchestrator), "root");
        assert_eq!("child".parse::<Role>(), Ok(Role::Worker), "child");
        assert_eq!("child".parse::<Actor>(), Ok(Actor::Worker), "actor");
        assert_eq!(
            "child-edit-outside".parse::<RuleId>(),
            Ok(RuleId::WorkerEditOutside),
            "rule id"
        );
        assert_eq!(Role::Orchestrator.as_str(), "orchestrator", "role written");
        assert_eq!(
            RuleId::OrchestratorGit.as_str(),
            "orchestrator-git",
            "rule written"
        );
        for rule in RuleId::ALL {
            assert_eq!(rule.as_str().parse::<RuleId>(), Ok(*rule), "{rule}");
        }
    }

    #[test]
    fn a_closing_task_whose_session_is_over_frees_its_slot() {
        assert!(
            TaskState::Closing.holds_slot(false),
            "closing, session live"
        );
        assert!(
            !TaskState::Closing.holds_slot(true),
            "closing, session over"
        );
        assert!(
            TaskState::Working.holds_slot(true),
            "working keeps its slot until lost"
        );
        assert!(
            !TaskState::Queued.holds_slot(false),
            "queued never holds one"
        );
    }

    #[test]
    fn unknown_words_are_refused() {
        let error = "asleep".parse::<TaskState>();
        assert_eq!(
            error,
            Err(UnknownWord {
                kind: "task state",
                word: "asleep".to_owned()
            }),
            "unknown state"
        );
    }

    #[test]
    fn only_closed_releases_the_name() {
        let releasing: Vec<_> = TaskState::ALL.iter().filter(|s| !s.holds_name()).collect();
        assert_eq!(
            releasing,
            [&TaskState::Closed],
            "only closed releases the name"
        );
    }
}
