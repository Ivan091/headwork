//! The `heavy-ungated` rule: cargo build, test, clippy and bench, and `check.sh`, run inside a gate.
//!
//! The shell reader makes `bash scripts/check.sh` a command whose program is `scripts/check.sh`, and `headwork gate run
//! cpu -- cargo test` one command whose program is `headwork`, so a gated command never shows up as a command of its
//! own: the rule only has to refuse the heavy commands it sees and let the gate commands through.

use super::shell::SimpleCommand;
use super::{Decision, RuleId};

/// The cargo subcommands that keep the machine busy.
const HEAVY_CARGO: &[&str] = &["build", "test", "clippy", "bench"];

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

/// `scripts/heavy.sh <command>`, the older wrapper.
fn is_heavy_wrapper(command: &SimpleCommand) -> bool {
    ends_with_script(command, "heavy.sh")
}

fn is_heavy(command: &SimpleCommand) -> bool {
    ends_with_script(command, "check.sh") || is_heavy_cargo(command)
}

fn ends_with_script(command: &SimpleCommand, script: &str) -> bool {
    command.program().replace('\\', "/").ends_with(script)
}

fn is_heavy_cargo(command: &SimpleCommand) -> bool {
    if command.program_name() != "cargo" {
        return false;
    }
    command
        .args()
        .iter()
        .find(|word| !word.starts_with('+') && !word.starts_with('-'))
        .is_some_and(|sub| HEAVY_CARGO.contains(&sub.as_str()))
}
