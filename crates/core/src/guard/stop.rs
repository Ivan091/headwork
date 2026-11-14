//! The `Stop` gate: a child does not end its turn silently while its task is unfinished, and no session ends a turn
//! with a plain-text question.

use super::Decision;
use crate::{Role, RuleId, TaskState};

/// Consecutive blocked stops after which the gate lets a session stop, well under Claude's own limit of 8; the tick is
/// the backstop.
pub const MAX_BLOCKS: u32 = 3;

/// What the gate knows about the turn that is ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent fact the gate reads"
)]
pub struct StopFacts<'a> {
    /// The role.
    pub role: Role,
    /// The task's state; `None` for the root.
    pub task: Option<TaskState>,
    /// Stops already blocked in a row since the last prompt.
    pub blocked_in_row: u32,
    /// Since the last prompt the session asked the user with `AskUserQuestion`.
    pub asked: bool,
    /// Since the last prompt it sent a message to another session (its report).
    pub messaged: bool,
    /// Since the last prompt it reported `done` or `blocked` through the MCP `report` tool.
    pub reported: bool,
    /// The task is a standing role that does not close when idle.
    pub keep: bool,
    /// The last assistant message of the turn.
    pub last_message: Option<&'a str>,
}

/// Decides a stop.
#[must_use]
pub fn check_stop(facts: &StopFacts<'_>) -> Decision {
    if facts.blocked_in_row >= MAX_BLOCKS {
        return Decision::Allow;
    }
    let question = facts.last_message.is_some_and(|message| {
        message
            .trim_end()
            .trim_end_matches(['*', '_', '`'])
            .ends_with('?')
    });
    if question {
        return Decision::deny(
            RuleId::StopPlainQuestion,
            "your turn ends with a question in plain text, which notifies nobody: ask it with AskUserQuestion \
             (concrete numbered options, recommendation first)",
        );
    }
    let unfinished = facts.role == Role::Child
        && !facts.keep
        && facts.task == Some(TaskState::Working)
        && !facts.asked
        && !facts.messaged
        && !facts.reported;
    if unfinished {
        return Decision::deny(
            RuleId::StopUnfinished,
            "your task is still working: carry on, ask the user with AskUserQuestion if you are blocked, call the headwork \
             `report` tool with `blocked` or `done`, or close yourself when it is finished",
        );
    }
    Decision::Allow
}
