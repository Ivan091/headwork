//! Reviews on the user's word (`docs/review-flows.md`): the `review N` command, the flow graphs, which nodes are
//! ready, the review's state machine, the dispute rule and what a round delivers.
//!
//! A review is run by a reviewer lead, a part of kind `review`, that walks a flow: a named graph of small specialist
//! reviewers, each run as a fresh subagent. Everything here is pure; the store keeps the progress and the binary acts.

mod command;
mod flow;
mod policy;
mod prompt;
mod schedule;
mod state;

use std::fmt;

use serde::{Deserialize, Serialize};

pub use command::{NEAR_MISS_HINT, ParsedCommand, ReviewCommand, parse_command};
pub use flow::{
    After, DisputeRule, Each, EachChoice, Flow, FlowError, FlowName, LevelSpec, ModelChoice, Node,
    NodeId, Target, Unsettled,
};
pub use policy::{
    Candidate, Limits, ListReason, Seen, Selection, is_repeat, meets_floor, normalize, repeat_of,
    select, weight,
};
pub use prompt::{
    FINDINGS_SHAPE, FindingText, ReviewTarget, VERDICT_SHAPE, VERDICTS_SHAPE, compose,
};
pub use schedule::{Instance, Progress, members, ready, round_complete, round_scope};
pub use state::{
    DisputeCall, ReviewFacts, ReviewRefusal, ReviewTrigger, ReviewerView, apply, round_due,
    settle_dispute,
};

/// How strong a review and its pushback are: 1 light, 2 standard, 3 strict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum Level {
    /// Few reviewers, defects only, any reasoned dispute accepted.
    Light,
    /// More reviewers, a verify stage, disputes need evidence.
    Standard,
    /// Every reviewer, challengers of the approach and the scope, two rounds.
    Strict,
}

impl Level {
    /// Every level, lightest first.
    pub const ALL: [Self; 3] = [Self::Light, Self::Standard, Self::Strict];

    /// The number the user types.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Light => 1,
            Self::Standard => 2,
            Self::Strict => 3,
        }
    }

    /// The level a number names, if any.
    #[must_use]
    pub const fn from_number(number: u8) -> Option<Self> {
        match number {
            1 => Some(Self::Light),
            2 => Some(Self::Standard),
            3 => Some(Self::Strict),
            _ => None,
        }
    }
}

/// A number that is not a level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("review level {0} is not 1, 2 or 3")]
pub struct BadLevel(pub u8);

impl TryFrom<u8> for Level {
    type Error = BadLevel;

    fn try_from(number: u8) -> Result<Self, Self::Error> {
        Self::from_number(number).ok_or(BadLevel(number))
    }
}

impl From<Level> for u8 {
    fn from(level: Level) -> Self {
        level.number()
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}
