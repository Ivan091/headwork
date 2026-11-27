//! A review's state machine and the dispute rule (`docs/review-flows.md`, sections 4 and 7).

use serde::Serialize;

use super::flow::{DisputeRule, LevelSpec, Unsettled};
use crate::vocabulary::ReviewState;

/// What moves a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewTrigger {
    /// The implementing lead called `review_start`.
    Start,
    /// The reviewer lead delivered the round's findings.
    Deliver,
    /// The implementing lead fixed or disputed every open finding.
    Resolve,
    /// The reviewer lead recorded its re-check.
    Verdict,
    /// The user answered the disputes put to them.
    Answer,
    /// The reviewer lead closed the review.
    Finish,
    /// The lead closed, or the user dropped the review part.
    Cancel,
}

impl ReviewTrigger {
    /// The trigger's name, as events and refusals spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Deliver => "deliver",
            Self::Resolve => "resolve",
            Self::Verdict => "verdict",
            Self::Answer => "answer",
            Self::Finish => "finish",
            Self::Cancel => "cancel",
        }
    }
}

/// The facts a transition's guard reads, counted after the trigger's own writes (a verdict's resolutions, the user's
/// answers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReviewFacts {
    /// Every node of the round is done or failed.
    pub round_complete: bool,
    /// Findings waiting on the implementing lead: open, fix rejected, or kept by the user.
    pub awaiting_lead: u32,
    /// Findings the lead fixed or disputed that have no verdict yet.
    pub awaiting_verdict: u32,
    /// Disputes put to the user that have no answer yet.
    pub asked: u32,
    /// Another full round is due.
    pub round_due: bool,
}

/// Why a review did not move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReviewRefusal {
    /// The trigger does not apply in this state.
    #[error("a review in state `{from}` cannot {trigger}")]
    WrongState {
        /// The current state.
        from: ReviewState,
        /// The trigger's name.
        trigger: &'static str,
    },
    /// A node of the round is still ready or running (`review-nodes-open`).
    #[error(
        "a node of this round is still ready or running; run it and hand its result back first"
    )]
    NodesOpen,
    /// Findings still wait on the implementing lead.
    #[error("{0} finding(s) still wait on the implementing lead: fix or dispute each")]
    FindingsOpen(u32),
    /// Fixes or disputes still wait on the reviewer lead's verdict.
    #[error("{0} fix(es) or dispute(s) still wait on a verdict")]
    VerdictsOpen(u32),
    /// Disputes still wait on the user.
    #[error("{0} dispute(s) still wait on the user")]
    AskWaiting(u32),
    /// Another round is due before the review may finish.
    #[error("another full round is due on the fixed commit")]
    RoundDue,
}

/// The state a trigger takes a review to, or why not.
///
/// `requested` → `running` → `delivered` → `rechecking`, and from there: `asking` while a dispute waits on the user,
/// `delivered` again while a finding waits on the lead (a rejected fix, a finding the user kept), `running` when another
/// round is due, else it stays `rechecking` until `finish`. `finish` also ends a delivered round with nothing in it.
/// `cancel` ends any open review.
///
/// # Errors
///
/// Returns a [`ReviewRefusal`] when the trigger does not apply in this state or its guard fails.
pub const fn apply(
    from: ReviewState,
    trigger: ReviewTrigger,
    facts: ReviewFacts,
) -> Result<ReviewState, ReviewRefusal> {
    let wrong = ReviewRefusal::WrongState {
        from,
        trigger: trigger.name(),
    };
    match (from, trigger) {
        (ReviewState::Done | ReviewState::Cancelled, _) => Err(wrong),
        (_, ReviewTrigger::Cancel) => Ok(ReviewState::Cancelled),
        (ReviewState::Requested, ReviewTrigger::Start) => Ok(ReviewState::Running),
        (ReviewState::Running, ReviewTrigger::Deliver) => {
            if facts.round_complete {
                Ok(ReviewState::Delivered)
            } else {
                Err(ReviewRefusal::NodesOpen)
            }
        }
        (ReviewState::Delivered, ReviewTrigger::Resolve) => {
            if facts.awaiting_lead > 0 {
                Err(ReviewRefusal::FindingsOpen(facts.awaiting_lead))
            } else {
                Ok(ReviewState::Rechecking)
            }
        }
        (ReviewState::Rechecking, ReviewTrigger::Verdict) => {
            if facts.awaiting_verdict > 0 {
                Err(ReviewRefusal::VerdictsOpen(facts.awaiting_verdict))
            } else {
                Ok(after_recheck(facts))
            }
        }
        (ReviewState::Asking, ReviewTrigger::Answer) => {
            if facts.asked > 0 {
                Err(ReviewRefusal::AskWaiting(facts.asked))
            } else {
                Ok(after_recheck(facts))
            }
        }
        (ReviewState::Delivered | ReviewState::Rechecking, ReviewTrigger::Finish) => {
            if facts.awaiting_lead > 0 {
                Err(ReviewRefusal::FindingsOpen(facts.awaiting_lead))
            } else if facts.awaiting_verdict > 0 {
                Err(ReviewRefusal::VerdictsOpen(facts.awaiting_verdict))
            } else if facts.asked > 0 {
                Err(ReviewRefusal::AskWaiting(facts.asked))
            } else if facts.round_due {
                Err(ReviewRefusal::RoundDue)
            } else {
                Ok(ReviewState::Done)
            }
        }
        (
            ReviewState::Requested
            | ReviewState::Running
            | ReviewState::Delivered
            | ReviewState::Rechecking
            | ReviewState::Asking,
            _,
        ) => Err(wrong),
    }
}

const fn after_recheck(facts: ReviewFacts) -> ReviewState {
    if facts.asked > 0 {
        ReviewState::Asking
    } else if facts.awaiting_lead > 0 {
        ReviewState::Delivered
    } else if facts.round_due {
        ReviewState::Running
    } else {
        ReviewState::Rechecking
    }
}

/// Whether another round is due after `round`.
///
/// Either the level asks for more rounds and this round fixed an important finding, or commits landed while the round
/// ran (`late`) and the round is not past the level's rounds, so late commits earn exactly one more fix-scoped round
/// even at a one-round level.
///
/// A fix counts while its verdict is due and once it is accepted. Fixed doubts and nits alone end the review, since a
/// second round that only polishes costs as much as the first (`docs/review-flows.md`, section 13).
#[must_use]
pub const fn round_due(spec: &LevelSpec, round: u8, fixed_important: bool, late: bool) -> bool {
    (fixed_important && round < spec.rounds) || (late && round <= spec.rounds)
}

/// What the reviewer lead's fresh re-check concluded about a dispute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewerView {
    /// The dispute meets the level's bar.
    Accepts,
    /// The finding still stands.
    Keeps,
}

/// What becomes of a dispute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisputeCall {
    /// The dispute is accepted.
    Accept,
    /// The reviewer lead argues back once; the implementing lead answers again.
    ArgueBack,
    /// The dispute goes to the user.
    AskUser,
}

/// Settles a dispute by the level.
///
/// A `reason` level accepts every dispute; an `evidence` level accepts what the reviewer accepts, argues back once
/// first where the level says so, then accepts or asks the user by `unsettled`.
#[must_use]
pub const fn settle_dispute(
    spec: &LevelSpec,
    view: ReviewerView,
    argued_back: bool,
) -> DisputeCall {
    match (spec.dispute, view) {
        (DisputeRule::Reason, _) | (DisputeRule::Evidence, ReviewerView::Accepts) => {
            DisputeCall::Accept
        }
        (DisputeRule::Evidence, ReviewerView::Keeps) => {
            if spec.argue_back && !argued_back {
                DisputeCall::ArgueBack
            } else {
                match spec.unsettled {
                    Unsettled::Accept => DisputeCall::Accept,
                    Unsettled::Ask => DisputeCall::AskUser,
                }
            }
        }
    }
}
