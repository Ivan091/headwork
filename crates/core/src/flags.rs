//! The per-task flags the orchestrator and leads choose: whether the task closes silently and whether a review is planned
//! (`docs/decisions.md`, 2026-11-23, silent and review chosen per task).

use std::fmt;

use crate::review::Level;
use crate::vocabulary::CloseMode;

/// Who set a task's flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DecidedBy {
    /// The user's own words named the values.
    User,
    /// The caller picked them with the rules' decision table.
    Judged,
}

impl DecidedBy {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Judged => "judged",
        }
    }

    /// The value a word names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "user" => Some(Self::User),
            "judged" => Some(Self::Judged),
            _ => None,
        }
    }
}

impl fmt::Display for DecidedBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

/// How the flags of a new task were chosen, kept in its spawn event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagDecision {
    /// Who chose.
    pub decided_by: DecidedBy,
    /// The caller's one-line reason; empty when the user's words set the values.
    pub reason: String,
}

/// Why a spawn's flags were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecisionError {
    /// The flags were judged but no reason was given.
    #[error(
        "`close` or `review` was chosen without `reason`: give a one-line reason, or `decided_by: user` when the user's \
         own words set it"
    )]
    ReasonMissing,
}

/// Checks the flags of a spawn: when `close` or `review` was given, `decided_by` is `user` or `judged` (absent means
/// `judged`) and a judged choice needs a non-empty `reason`. Nothing given means no decision.
///
/// # Errors
///
/// Returns [`DecisionError::ReasonMissing`] for a judged choice without a reason.
pub fn check_decision(
    flags_given: bool,
    decided_by: Option<DecidedBy>,
    reason: Option<&str>,
) -> Result<Option<FlagDecision>, DecisionError> {
    if !flags_given {
        return Ok(None);
    }
    let decided_by = decided_by.unwrap_or(DecidedBy::Judged);
    let reason = reason.map(str::trim).unwrap_or_default();
    if decided_by == DecidedBy::Judged && reason.is_empty() {
        return Err(DecisionError::ReasonMissing);
    }
    Ok(Some(FlagDecision {
        decided_by,
        reason: reason.to_owned(),
    }))
}

/// A review level as a spawn argument: `0` is none.
///
/// # Errors
///
/// Returns the number when it is above 3.
pub const fn review_from_number(number: u8) -> Result<Option<Level>, u8> {
    match number {
        0 => Ok(None),
        1..=3 => Ok(Level::from_number(number)),
        other => Err(other),
    }
}

/// When a planned review is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMoment {
    /// The task reports its first pull request.
    FirstPullRequest,
    /// The task calls `done`.
    Done,
}

/// What to do with a task's planned review at a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedReview {
    /// Nothing is planned, or it is not due.
    None,
    /// Request a review of this level now.
    Request(Level),
}

/// Whether a planned review starts now: the task is a lead, reviews are enabled and a level is planned. Both moments
/// start it; a disabled review config leaves the plan stored and ignored.
#[must_use]
pub const fn planned_review(
    is_lead: bool,
    reviews_enabled: bool,
    plan: Option<Level>,
    moment: PlanMoment,
) -> PlannedReview {
    match (is_lead, reviews_enabled, plan, moment) {
        (true, true, Some(level), PlanMoment::FirstPullRequest | PlanMoment::Done) => {
            PlannedReview::Request(level)
        }
        _ => PlannedReview::None,
    }
}

/// A prompt of the user that changes a task's flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagPrompt {
    /// `silent`.
    Silent,
    /// `confirm`.
    Confirm,
    /// `no review`.
    NoReview,
}

impl FlagPrompt {
    /// The words the context line quotes.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Confirm => "confirm",
            Self::NoReview => "no review",
        }
    }

    /// The close mode the prompt sets, if it sets one.
    #[must_use]
    pub const fn close(self) -> Option<CloseMode> {
        match self {
            Self::Silent => Some(CloseMode::Silent),
            Self::Confirm => Some(CloseMode::Confirm),
            Self::NoReview => None,
        }
    }
}

/// Recognises a prompt that is exactly `silent`, `confirm` or `no review`: trimmed, any case, trailing `.` and `!` dropped.
#[must_use]
pub fn parse_flag_prompt(prompt: &str) -> Option<FlagPrompt> {
    let word = prompt
        .trim()
        .trim_end_matches(['.', '!'])
        .trim_end()
        .to_lowercase();
    match word.as_str() {
        "silent" => Some(FlagPrompt::Silent),
        "confirm" => Some(FlagPrompt::Confirm),
        "no review" => Some(FlagPrompt::NoReview),
        _ => None,
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn close_mode_words_parse_and_default_by_kind() {
        assert_eq!("silent".parse(), Ok(CloseMode::Silent));
        assert_eq!("confirm".parse(), Ok(CloseMode::Confirm));
        assert!("loud".parse::<CloseMode>().is_err());
        assert_eq!(CloseMode::default_for("silent"), CloseMode::Silent);
        assert_eq!(CloseMode::default_for("explore"), CloseMode::Silent);
        assert_eq!(CloseMode::default_for("task"), CloseMode::Confirm);
        assert_eq!(CloseMode::Silent.to_string(), "silent");
        assert_eq!(
            serde_json::to_string(&CloseMode::Silent).unwrap(),
            "\"silent\""
        );
    }

    #[test]
    fn asks_before_close_follows_the_mode() {
        let mut leads = crate::LeadsConfig::default();
        assert!(leads.asks_before_close(CloseMode::Confirm));
        assert!(!leads.asks_before_close(CloseMode::Silent));
        leads.ask_before_close = false;
        assert!(!leads.asks_before_close(CloseMode::Confirm));
    }

    #[test]
    fn decisions_need_a_reason_when_judged() {
        let judged = |reason: &str| FlagDecision {
            decided_by: DecidedBy::Judged,
            reason: reason.to_owned(),
        };
        let table = [
            (false, None, None, Ok(None)),
            (false, Some(DecidedBy::Judged), None, Ok(None)),
            (true, None, None, Err(DecisionError::ReasonMissing)),
            (
                true,
                Some(DecidedBy::Judged),
                Some("  "),
                Err(DecisionError::ReasonMissing),
            ),
            (
                true,
                Some(DecidedBy::Judged),
                Some("docs typo"),
                Ok(Some(judged("docs typo"))),
            ),
            (
                true,
                Some(DecidedBy::User),
                None,
                Ok(Some(FlagDecision {
                    decided_by: DecidedBy::User,
                    reason: String::new(),
                })),
            ),
        ];
        for (given, by, reason, want) in table {
            assert_eq!(
                check_decision(given, by, reason),
                want,
                "{given} {by:?} {reason:?}"
            );
        }
    }

    #[test]
    fn review_numbers() {
        assert_eq!(review_from_number(0), Ok(None));
        assert_eq!(review_from_number(2), Ok(Some(Level::Standard)));
        assert_eq!(review_from_number(4), Err(4));
    }

    #[test]
    fn planned_review_decisions() {
        use PlanMoment::{Done, FirstPullRequest};
        let level = Some(Level::Standard);
        let table = [
            (
                true,
                true,
                level,
                FirstPullRequest,
                PlannedReview::Request(Level::Standard),
            ),
            (
                true,
                true,
                level,
                Done,
                PlannedReview::Request(Level::Standard),
            ),
            (false, true, level, Done, PlannedReview::None),
            (true, false, level, Done, PlannedReview::None),
            (true, true, None, Done, PlannedReview::None),
        ];
        for (lead, enabled, plan, moment, want) in table {
            assert_eq!(
                planned_review(lead, enabled, plan, moment),
                want,
                "{lead} {enabled} {plan:?} {moment:?}"
            );
        }
    }

    #[test]
    fn flag_prompts() {
        let table = [
            ("silent", Some(FlagPrompt::Silent)),
            ("  Silent. ", Some(FlagPrompt::Silent)),
            ("CONFIRM!", Some(FlagPrompt::Confirm)),
            ("no review", Some(FlagPrompt::NoReview)),
            ("No Review!.", Some(FlagPrompt::NoReview)),
            ("make it silent", None),
            ("silent please", None),
            ("no  review", None),
            ("", None),
        ];
        for (prompt, want) in table {
            assert_eq!(parse_flag_prompt(prompt), want, "{prompt:?}");
        }
        assert_eq!(FlagPrompt::Silent.close(), Some(CloseMode::Silent));
        assert_eq!(FlagPrompt::NoReview.close(), None);
    }
}
