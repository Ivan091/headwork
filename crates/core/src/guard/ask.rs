//! The `AskUserQuestion` checks: at least two concrete options, the recommendation first, no "needs changes" option,
//! and the push or file the ask must follow.

use super::Decision;
use crate::RuleId;

/// The fewest options an ask may have. There is no upper bound of our own: `AskUserQuestion` caps its own options.
pub const MIN_OPTIONS: usize = 2;

/// The mark of the recommended option.
pub const RECOMMENDED: &str = "(recommended)";

/// Options that mean "needs changes"; the user types changes instead of picking one.
const CHANGES: &[&str] = &[
    "needs changes",
    "need changes",
    "needs change",
    "request changes",
    "requested changes",
    "make changes",
    "changes needed",
    "change something",
    "revise",
    "other",
    "something else",
    "none of the above",
    "none of these",
];

/// One question of an ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question<'a> {
    /// The header chip.
    pub header: &'a str,
    /// The option labels, in order.
    pub options: Vec<&'a str>,
}

/// Whether something must precede an ask, and whether it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Precondition {
    /// Nothing is required.
    #[default]
    NotRequired,
    /// Required and done recently enough.
    Met,
    /// Required and missing.
    Missing,
}

/// What the ask must follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AskFacts {
    /// A push notification before the ask.
    pub push: Precondition,
    /// The file under review, sent before a review ask.
    pub review_file: Precondition,
}

/// Decides an ask.
#[must_use]
pub fn check_ask(questions: &[Question<'_>], facts: AskFacts) -> Decision {
    for question in questions {
        let count = question.options.len();
        if count < MIN_OPTIONS {
            return Decision::deny(
                RuleId::AskOptionCount,
                format!(
                    "give at least {MIN_OPTIONS} concrete options, not {count}; the user can always type their own answer"
                ),
            );
        }
        if let Some(label) = question.options.iter().find(|label| means_changes(label)) {
            return Decision::deny(
                RuleId::AskChangesOption,
                format!(
                    "drop the option `{label}`: the user types changes in their own words, so every option must be a \
                     concrete choice"
                ),
            );
        }
        let recommended: Vec<usize> = question
            .options
            .iter()
            .enumerate()
            .filter(|(_, label)| label.to_lowercase().contains(RECOMMENDED))
            .map(|(index, _)| index)
            .collect();
        if recommended.len() > 1 || recommended.iter().any(|index| *index != 0) {
            return Decision::deny(
                RuleId::AskRecommendedFirst,
                "mark at most one option `(Recommended)` and put it first",
            );
        }
    }
    if facts.push == Precondition::Missing {
        return Decision::deny(
            RuleId::AskPush,
            "send a PushNotification with the question first, so the user's phone hears it, then ask again",
        );
    }
    let review = questions
        .iter()
        .any(|question| question.header.trim().eq_ignore_ascii_case("review"));
    if review && facts.review_file == Precondition::Missing {
        return Decision::deny(
            RuleId::AskReviewFile,
            "send the thing under review with SendUserFile first, then ask again",
        );
    }
    Decision::Allow
}

fn means_changes(label: &str) -> bool {
    let lower = label.to_lowercase().replace(RECOMMENDED, "");
    let text = lower
        .trim()
        .trim_start_matches(|c: char| {
            c.is_ascii_digit() || c == '.' || c == ')' || c.is_whitespace()
        })
        .trim();
    CHANGES.iter().any(|phrase| {
        text.strip_prefix(phrase)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_phrases_are_whole_words() {
        assert!(means_changes("Needs changes"), "plain");
        assert!(means_changes("3. Other"), "numbered");
        assert!(means_changes("Revise: try again"), "with a tail");
        assert!(!means_changes("Otherwise fine, ship it"), "a longer word");
        assert!(!means_changes("Ship it (Recommended)"), "a choice");
    }
}
