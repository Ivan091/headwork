//! The `heavy-ungated` rule: cargo build, test, clippy and bench, and `check.sh`, run inside a gate.
//!
//! The shell reader makes `bash scripts/check.sh` a command whose program is `scripts/check.sh`, and `headwork gate run
//! cpu -- cargo test` one command whose program is `headwork`, so a gated command never shows up as a command of its
//! own: the rule only has to refuse the heavy commands it sees and let the gate commands through.

use super::shell::SimpleCommand;
use super::{Decision, RuleId};

/// The cargo subcommands that keep the machine busy.
const HEAVY_CARGO: &[&str] = &["build", "test", "clippy", "bench"];

/// Cargo options before the subcommand whose value is the next word.
const CARGO_VALUE_OPTIONS: &[&str] = &[
    "--manifest-path",
    "--config",
    "--color",
    "--explain",
    "-C",
    "-Z",
];

/// The refusal's text.
const HINT: &str = "heavy work outside a gate: run it as `headwork gate run cpu -- <the command>`; a gated check may wait \
                    for a free slot, so run long ones in the background (`job_start` with `gate: \"cpu\"`)";

/// Refuses a heavy command that does not run under `headwork gate run <class> --` or a `heavy.sh` wrapper.
pub(super) fn check_heavy(command: &SimpleCommand) -> Option<Decision> {
    if is_gate_command(command) || is_heavy_wrapper(command) {
        return None;
    }
    is_heavy(command).then(|| Decision::deny(RuleId::HeavyUngated, HINT))
}

/// `headwork gate run <class> -- <command>`, as `headwork` under any path or extension.
fn is_gate_command(command: &SimpleCommand) -> bool {
    let args = command.args();
    command.program_name() == "headwork"
        && args.first().is_some_and(|word| word == "gate")
        && args.get(1).is_some_and(|word| word == "run")
        && args.iter().skip(2).any(|word| word == "--")
}

/// `scripts/heavy.sh <command>`, the older wrapper, by its file name.
fn is_heavy_wrapper(command: &SimpleCommand) -> bool {
    command.program_name() == "heavy.sh"
}

/// `check.sh` by its file name (`precheck.sh` is not it), or a heavy cargo subcommand.
fn is_heavy(command: &SimpleCommand) -> bool {
    command.program_name() == "check.sh" || is_heavy_cargo(command)
}

fn is_heavy_cargo(command: &SimpleCommand) -> bool {
    command.program_name() == "cargo"
        && cargo_subcommand(command.args()).is_some_and(|sub| HEAVY_CARGO.contains(&sub))
}

/// The first word after cargo's `+toolchain` and its global options, the value of an option in
/// [`CARGO_VALUE_OPTIONS`] skipped with it.
fn cargo_subcommand(args: &[String]) -> Option<&str> {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if CARGO_VALUE_OPTIONS.contains(&word.as_str()) {
            words.next();
        } else if !word.starts_with('+') && !word.starts_with('-') {
            return Some(word);
        }
    }
    None
}
