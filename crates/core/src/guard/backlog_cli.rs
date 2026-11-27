//! The `worker-backlog-cli` rule: a worker or an explorer changes no backlog item through the CLI.
//!
//! The CLI takes its backlog actor from `HEADWORK_ROLE`, which a worker could leave out; this rule backs that trust by
//! refusing every writing `headwork backlog` subcommand in a worker's session. Listing stays allowed, and an explorer
//! adds its picked items through its `backlog_add` tool.

use super::shell::SimpleCommand;
use super::{Decision, RuleId};

/// The `headwork backlog` subcommands that change the store or the file.
const WRITING: &[&str] = &[
    "add", "done", "start", "cancel", "block", "unblock", "reopen", "edit", "import", "export",
];

/// Refuses `headwork backlog <writing subcommand>` (also through its old name `todo`), passed as `args`.
pub(super) fn check_backlog_cli(args: &[String]) -> Option<Decision> {
    let mut words = args
        .iter()
        .map(String::as_str)
        .filter(|arg| !arg.starts_with('-'));
    if !matches!(words.next(), Some("backlog" | "todo")) {
        return None;
    }
    let sub = words.next()?;
    WRITING.contains(&sub).then(|| {
        Decision::deny(
            RuleId::WorkerBacklogCli,
            format!(
                "`headwork backlog {sub}` changes the backlog, which is the orchestrator's: report what should change, or \
                 use your backlog tools; `headwork backlog list` stays open to you"
            ),
        )
    })
}

/// The command's words after `headwork` when it is one.
pub(super) fn backlog_command(command: &SimpleCommand) -> Option<Decision> {
    super::headwork_args(command).and_then(check_backlog_cli)
}
