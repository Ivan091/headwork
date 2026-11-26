//! What a round hands the implementing lead (`docs/review-flows.md`, section 13): the severity floor, the caps on
//! delivered findings, and repeats across rounds.
//!
//! A finding below the floor or over a cap is listed, not delivered: the implementing lead sees it for information and
//! answers nothing. A finding a later round raises again is not recorded at all.

use std::cmp::Ordering;

use serde::Serialize;

use crate::vocabulary::{FindingVerdict, Severity};

/// How heavy a severity is: 0 for the heaviest. The order is important, then doubt, then nit.
#[must_use]
pub const fn weight(severity: Severity) -> u8 {
    match severity {
        Severity::Important => 0,
        Severity::Doubt => 1,
        Severity::Nit => 2,
    }
}

/// Whether a finding of `severity` is at or above `floor`, so it may be delivered.
#[must_use]
pub const fn meets_floor(severity: Severity, floor: Severity) -> bool {
    weight(severity) <= weight(floor)
}

const fn verdict_weight(verdict: Option<FindingVerdict>) -> u8 {
    match verdict {
        Some(FindingVerdict::Confirmed) => 0,
        Some(FindingVerdict::Plausible) => 1,
        None => 2,
        Some(FindingVerdict::Rejected) => 3,
    }
}

/// A summary as repeats compare it: lowercase, every run of characters that are not letters or digits one space,
/// trimmed.
#[must_use]
pub fn normalize(summary: &str) -> String {
    let mut out = String::with_capacity(summary.len());
    let mut gap = false;
    for c in summary.chars() {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.extend(c.to_lowercase());
        } else {
            gap = true;
        }
    }
    out
}

/// A finding as repeats compare it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen<'a> {
    /// The finding's number; 0 for one not recorded yet.
    pub id: i64,
    /// The node that raised it.
    pub node: &'a str,
    /// The file, if it names one.
    pub file: Option<&'a str>,
    /// The line, if it names one.
    pub line: Option<u32>,
    /// What is wrong.
    pub summary: &'a str,
}

/// Whether `new` raises `earlier` again: the same normalized summary (not empty), or the same file, line and node.
#[must_use]
pub fn is_repeat(new: &Seen<'_>, earlier: &Seen<'_>) -> bool {
    let same_place = new.file.is_some()
        && new.line.is_some()
        && new.file == earlier.file
        && new.line == earlier.line
        && new.node == earlier.node;
    if same_place {
        return true;
    }
    let summary = normalize(new.summary);
    !summary.is_empty() && summary == normalize(earlier.summary)
}

/// The first of `earlier` that `new` repeats, by its number.
#[must_use]
pub fn repeat_of(new: &Seen<'_>, earlier: &[Seen<'_>]) -> Option<i64> {
    earlier
        .iter()
        .find(|old| is_repeat(new, old))
        .map(|old| old.id)
}

/// Why a finding is listed instead of delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListReason {
    /// Its severity is below `[reviews] floor`.
    BelowFloor,
    /// The round delivered `[reviews] max_round_findings` already.
    OverRoundCap,
    /// The reviewed task was delivered `[reviews] max_task_findings` over all its reviews already.
    OverTaskCap,
}

impl ListReason {
    /// The reason in words, for the reviewer lead's message.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::BelowFloor => "below the severity floor",
            Self::OverRoundCap => "over the round's cap",
            Self::OverTaskCap => "over the task's cap",
        }
    }
}

/// A finding that may go on: it passed the verify stage, or none ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// The finding's number.
    pub id: i64,
    /// How much it weighs.
    pub severity: Severity,
    /// What the verify stage concluded, if it ran.
    pub verdict: Option<FindingVerdict>,
}

/// The limits a round's delivery keeps to; a cap of 0 is no cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The lightest severity delivered.
    pub floor: Severity,
    /// At most this many findings per round.
    pub round_cap: u32,
    /// At most this many findings over every review of the task.
    pub task_cap: u32,
    /// Findings the task's reviews delivered before this round.
    pub task_delivered: u32,
}

/// What a round delivers and what it only lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// The numbers of the findings delivered, heaviest first.
    pub delivered: Vec<i64>,
    /// The findings listed for information, heaviest first, each with its reason.
    pub listed: Vec<(i64, ListReason)>,
}

fn order(a: &Candidate, b: &Candidate) -> Ordering {
    weight(a.severity)
        .cmp(&weight(b.severity))
        .then(verdict_weight(a.verdict).cmp(&verdict_weight(b.verdict)))
        .then(a.id.cmp(&b.id))
}

/// Splits a round's candidates into delivered and listed.
///
/// Candidates go by severity (important, doubt, nit), then verdict (confirmed, plausible, none), then number. One below
/// the floor is listed; the rest are delivered up to the tighter cap (the round's, or what is left of the task's), and
/// the overflow is listed with that cap as its reason (the round's when both are equal).
#[must_use]
pub fn select(candidates: &[Candidate], limits: Limits) -> Selection {
    let mut sorted = candidates.to_vec();
    sorted.sort_by(order);
    let round = (limits.round_cap > 0).then_some((limits.round_cap, ListReason::OverRoundCap));
    let task = (limits.task_cap > 0).then(|| {
        (
            limits.task_cap.saturating_sub(limits.task_delivered),
            ListReason::OverTaskCap,
        )
    });
    let cap = match (round, task) {
        (Some(round), Some(task)) => Some(if task.0 < round.0 { task } else { round }),
        (Some(one), None) | (None, Some(one)) => Some(one),
        (None, None) => None,
    };
    let mut selection = Selection::default();
    for candidate in sorted {
        if !meets_floor(candidate.severity, limits.floor) {
            selection
                .listed
                .push((candidate.id, ListReason::BelowFloor));
            continue;
        }
        match cap {
            Some((room, reason))
                if u32::try_from(selection.delivered.len()).unwrap_or(u32::MAX) >= room =>
            {
                selection.listed.push((candidate.id, reason));
            }
            Some(_) | None => selection.delivered.push(candidate.id),
        }
    }
    selection
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: i64, severity: Severity, verdict: Option<FindingVerdict>) -> Candidate {
        Candidate {
            id,
            severity,
            verdict,
        }
    }

    const fn limits(floor: Severity, round_cap: u32, task_cap: u32, task_delivered: u32) -> Limits {
        Limits {
            floor,
            round_cap,
            task_cap,
            task_delivered,
        }
    }

    #[test]
    fn the_floor_orders_important_over_doubt_over_nit() {
        for (severity, floor, meets) in [
            (Severity::Important, Severity::Important, true),
            (Severity::Doubt, Severity::Important, false),
            (Severity::Nit, Severity::Important, false),
            (Severity::Important, Severity::Doubt, true),
            (Severity::Doubt, Severity::Doubt, true),
            (Severity::Nit, Severity::Doubt, false),
            (Severity::Important, Severity::Nit, true),
            (Severity::Doubt, Severity::Nit, true),
            (Severity::Nit, Severity::Nit, true),
        ] {
            assert_eq!(
                meets_floor(severity, floor),
                meets,
                "{severity} at floor {floor}"
            );
        }
    }

    #[test]
    fn normalize_lowercases_and_collapses_what_is_not_a_letter_or_digit() {
        for (summary, normal) in [
            ("Off by one", "off by one"),
            ("  Off-by-one!! ", "off by one"),
            ("off_by__one in `parse()`", "off by one in parse"),
            ("Über 2 Fälle", "über 2 fälle"),
            ("...", ""),
            ("", ""),
        ] {
            assert_eq!(normalize(summary), normal, "{summary:?}");
        }
    }

    #[test]
    fn a_repeat_has_the_same_summary_or_the_same_place_and_node() {
        let earlier = Seen {
            id: 7,
            node: "correctness",
            file: Some("a.rs"),
            line: Some(3),
            summary: "Off by one",
        };
        let new = |node, file, line, summary| Seen {
            id: 0,
            node,
            file,
            line,
            summary,
        };
        for (case, seen, repeats) in [
            (
                "same summary elsewhere",
                new("tests", Some("b.rs"), None, "off-by-one"),
                true,
            ),
            (
                "same place and node",
                new("correctness", Some("a.rs"), Some(3), "a new thing"),
                true,
            ),
            (
                "same place, other node",
                new("errors", Some("a.rs"), Some(3), "a new thing"),
                false,
            ),
            (
                "same file, no line",
                new("correctness", Some("a.rs"), None, "a new thing"),
                false,
            ),
            (
                "other line",
                new("correctness", Some("a.rs"), Some(4), "a new thing"),
                false,
            ),
            ("empty summaries", new("tests", None, None, "!!"), false),
        ] {
            assert_eq!(is_repeat(&seen, &earlier), repeats, "{case}");
        }
        let blank = Seen {
            summary: "?",
            ..earlier
        };
        assert!(
            !is_repeat(&new("tests", None, None, "!"), &blank),
            "two summaries with no words are not one"
        );
        assert_eq!(
            repeat_of(&new("tests", None, None, "OFF BY ONE"), &[blank, earlier]),
            Some(7),
            "the earlier finding's number"
        );
        assert_eq!(
            repeat_of(&new("tests", None, None, "new"), &[earlier]),
            None,
            "no repeat"
        );
    }

    type Case<'a> = (&'a str, Limits, &'a [i64], &'a [(i64, ListReason)]);

    #[test]
    fn select_lists_below_the_floor_and_over_the_tighter_cap_in_order() {
        use FindingVerdict::{Confirmed, Plausible};
        use ListReason::{BelowFloor, OverRoundCap, OverTaskCap};
        use Severity::{Doubt, Important, Nit};
        let found = [
            candidate(1, Nit, None),
            candidate(2, Doubt, Some(Plausible)),
            candidate(3, Important, None),
            candidate(4, Important, Some(Confirmed)),
            candidate(5, Doubt, Some(Confirmed)),
            candidate(6, Important, Some(Plausible)),
        ];
        let cases: [Case<'_>; 6] = [
            (
                "nit floor, no caps: everything",
                limits(Nit, 0, 0, 0),
                &[4, 6, 3, 5, 2, 1],
                &[],
            ),
            (
                "doubt floor lists the nit",
                limits(Doubt, 0, 0, 0),
                &[4, 6, 3, 5, 2],
                &[(1, BelowFloor)],
            ),
            (
                "important floor",
                limits(Important, 0, 0, 0),
                &[4, 6, 3],
                &[(5, BelowFloor), (2, BelowFloor), (1, BelowFloor)],
            ),
            (
                "round cap 2",
                limits(Doubt, 2, 0, 0),
                &[4, 6],
                &[
                    (3, OverRoundCap),
                    (5, OverRoundCap),
                    (2, OverRoundCap),
                    (1, BelowFloor),
                ],
            ),
            (
                "task cap tighter than the round's",
                limits(Doubt, 4, 10, 9),
                &[4],
                &[
                    (6, OverTaskCap),
                    (3, OverTaskCap),
                    (5, OverTaskCap),
                    (2, OverTaskCap),
                    (1, BelowFloor),
                ],
            ),
            (
                "task cap spent",
                limits(Nit, 0, 3, 5),
                &[],
                &[
                    (4, OverTaskCap),
                    (6, OverTaskCap),
                    (3, OverTaskCap),
                    (5, OverTaskCap),
                    (2, OverTaskCap),
                    (1, OverTaskCap),
                ],
            ),
        ];
        for (case, limits, delivered, listed) in cases {
            let selection = select(&found, limits);
            assert_eq!(selection.delivered, delivered, "{case}: delivered");
            assert_eq!(selection.listed, listed, "{case}: listed");
        }
    }

    #[test]
    fn equal_caps_name_the_round() {
        let found = [
            candidate(1, Severity::Important, None),
            candidate(2, Severity::Important, None),
        ];
        let selection = select(&found, limits(Severity::Doubt, 1, 1, 0));
        assert_eq!(
            selection.listed,
            [(2, ListReason::OverRoundCap)],
            "the round's cap"
        );
    }
}
