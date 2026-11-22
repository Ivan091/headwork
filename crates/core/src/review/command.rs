//! The user's `review [<flow>] <level> [<focus>]` command (`docs/review-flows.md`, section 3).

use serde::Serialize;

use super::Level;
use super::flow::FlowName;

/// A review the user asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewCommand {
    /// The flow the user named; absent, it comes from the task's kind.
    pub flow: Option<FlowName>,
    /// How strong the review is.
    pub level: Level,
    /// What to look at most, in the user's words; empty when none.
    pub focus: String,
}

/// What a prompt is to the review trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedCommand {
    /// A review command.
    Review(ReviewCommand),
    /// Almost a review command; the hint says how to write one. No review starts.
    NearMiss(&'static str),
    /// Not a review command.
    Other,
}

/// The hint for a prompt that starts like a review command but is not one.
pub const NEAR_MISS_HINT: &str = "A review starts with `review <level>`, the level 1, 2 or 3, optionally a flow name before it \
     and a focus after it: `review 2`, `review [3]`, `review plan 1`, `review 2 focus on the park path`.";

enum LevelToken {
    Level(Level),
    Bad,
    NotALevel,
}

fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    text.find(char::is_whitespace).map_or((text, ""), |at| {
        let (word, rest) = text.split_at(at);
        (word, rest.trim_start())
    })
}

fn level_token(word: &str) -> LevelToken {
    let inner = word
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(word);
    if inner.is_empty() || !inner.bytes().all(|byte| byte.is_ascii_digit()) {
        return LevelToken::NotALevel;
    }
    inner
        .parse::<u8>()
        .ok()
        .and_then(Level::from_number)
        .map_or(LevelToken::Bad, LevelToken::Level)
}

/// Reads a prompt as a review command.
///
/// The prompt's first word is `review` (any case); then a level, or a flow name and a level; the rest is the focus.
/// A level outside 1 to 3, or a bare `review`, is a near miss; anything else, such as `review the docs`, is not a
/// command at all.
#[must_use]
pub fn parse_command(prompt: &str) -> ParsedCommand {
    let (word, rest) = split_word(prompt.trim());
    if !word.eq_ignore_ascii_case("review") {
        return ParsedCommand::Other;
    }
    if rest.is_empty() {
        return ParsedCommand::NearMiss(NEAR_MISS_HINT);
    }
    let (first, after_first) = split_word(rest);
    match level_token(first) {
        LevelToken::Level(level) => return review(None, level, after_first),
        LevelToken::Bad => return ParsedCommand::NearMiss(NEAR_MISS_HINT),
        LevelToken::NotALevel => {}
    }
    let Ok(flow) = FlowName::new(&first.to_ascii_lowercase()) else {
        return ParsedCommand::Other;
    };
    let (second, after_second) = split_word(after_first);
    match level_token(second) {
        LevelToken::Level(level) => review(Some(flow), level, after_second),
        LevelToken::Bad => ParsedCommand::NearMiss(NEAR_MISS_HINT),
        LevelToken::NotALevel => ParsedCommand::Other,
    }
}

fn review(flow: Option<FlowName>, level: Level, focus: &str) -> ParsedCommand {
    ParsedCommand::Review(ReviewCommand {
        flow,
        level,
        focus: focus.trim().to_owned(),
    })
}
