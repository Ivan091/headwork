//! The exit ladder: what an end path types into a session's pane, step by step, so `claude` leaves on its own.
//!
//! Claude Code archives a Remote Control session at claude.ai only when `claude` exits on its own; a killed one stays
//! in the Claude app's list as "Can't reach your computer". So every path that ends a live session (a close, a park,
//! the tick, a resume closing a restored pane) climbs this ladder first and ends the process tree only after its last
//! step. Each step is typed, then `[watch] exit_grace_s` is waited for `claude` to be gone; a grace of 0 asks nothing.
//! See `docs/decisions/2026-11-27-exit-every-end-path.md`.

use serde::Serialize;

/// What is typed to have `claude` leave.
pub const EXIT_COMMAND: &str = "/exit";

/// Who asks a session to exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExitMode {
    /// The session closing itself, once its own tool call has returned: a plain `/exit` first, which waits for a turn
    /// still running to end instead of cutting it off.
    Own,
    /// Anyone else: the session may be mid-turn or in an open dialog, so every `/exit` comes after Escape and a
    /// cleared line.
    Outside,
}

impl ExitMode {
    /// The word logged for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Outside => "outside",
        }
    }
}

/// One input typed into a pane, or a pause between two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitInput {
    /// Escape: ends a running turn or dismisses an open dialog.
    Escape,
    /// A pause long enough for an interrupted turn to stop; a key typed right after Escape reads as Alt and that key.
    Settle,
    /// Ctrl+U: clears what an interrupt put back into the input line.
    ClearLine,
    /// The text [`EXIT_COMMAND`].
    Command,
    /// Enter.
    Enter,
    /// Ctrl+C: interrupts a turn; a second one at an empty prompt makes Claude Code exit gracefully.
    Interrupt,
    /// A short pause between the two Ctrl+C, well inside the window Claude Code gives the second one.
    Beat,
}

/// One rung of the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExitStep {
    /// `/exit` and Enter.
    Exit,
    /// Escape, a pause, Ctrl+U, then `/exit` and Enter.
    ClearAndExit,
    /// Ctrl+C twice.
    CtrlC,
}

impl ExitStep {
    /// What the step types, in order.
    #[must_use]
    pub const fn inputs(self) -> &'static [ExitInput] {
        match self {
            Self::Exit => &[ExitInput::Command, ExitInput::Enter],
            Self::ClearAndExit => &[
                ExitInput::Escape,
                ExitInput::Settle,
                ExitInput::ClearLine,
                ExitInput::Command,
                ExitInput::Enter,
            ],
            Self::CtrlC => &[ExitInput::Interrupt, ExitInput::Beat, ExitInput::Interrupt],
        }
    }

    /// The word logged for it in `exit-asked`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::ClearAndExit => "clear-and-exit",
            Self::CtrlC => "ctrl-c",
        }
    }
}

/// The steps an end path climbs for `mode`, in order; none when the grace is 0, which turns asking off.
#[must_use]
pub const fn ladder(mode: ExitMode, grace_s: u32) -> &'static [ExitStep] {
    if grace_s == 0 {
        return &[];
    }
    match mode {
        ExitMode::Own => &[ExitStep::Exit, ExitStep::ClearAndExit, ExitStep::CtrlC],
        ExitMode::Outside => &[
            ExitStep::ClearAndExit,
            ExitStep::ClearAndExit,
            ExitStep::CtrlC,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_mode_climbs_its_own_ladder() {
        let table: [(ExitMode, u32, &[ExitStep]); 4] = [
            (
                ExitMode::Own,
                15,
                &[ExitStep::Exit, ExitStep::ClearAndExit, ExitStep::CtrlC],
            ),
            (
                ExitMode::Outside,
                15,
                &[
                    ExitStep::ClearAndExit,
                    ExitStep::ClearAndExit,
                    ExitStep::CtrlC,
                ],
            ),
            (ExitMode::Own, 0, &[]),
            (ExitMode::Outside, 0, &[]),
        ];
        for (mode, grace, steps) in table {
            assert_eq!(
                ladder(mode, grace),
                steps,
                "{mode:?} with a grace of {grace}"
            );
        }
    }

    #[test]
    fn each_step_types_its_keys_in_order() {
        use ExitInput::{Beat, ClearLine, Command, Enter, Escape, Interrupt, Settle};
        let table: [(ExitStep, &[ExitInput], &str); 3] = [
            (ExitStep::Exit, &[Command, Enter], "exit"),
            (
                ExitStep::ClearAndExit,
                &[Escape, Settle, ClearLine, Command, Enter],
                "clear-and-exit",
            ),
            (ExitStep::CtrlC, &[Interrupt, Beat, Interrupt], "ctrl-c"),
        ];
        for (step, inputs, word) in table {
            assert_eq!(step.inputs(), inputs, "{step:?}");
            assert_eq!(step.as_str(), word, "{step:?}");
        }
    }

    #[test]
    fn only_an_own_ladder_starts_without_escape_and_every_ladder_ends_on_ctrl_c() {
        for mode in [ExitMode::Own, ExitMode::Outside] {
            let steps = ladder(mode, 1);
            let first_escapes = steps
                .first()
                .is_some_and(|step| step.inputs().first() == Some(&ExitInput::Escape));
            assert_eq!(first_escapes, mode == ExitMode::Outside, "{mode:?}");
            assert_eq!(steps.last(), Some(&ExitStep::CtrlC), "{mode:?}");
        }
    }
}
