//! The `worker-backlog-cli` rule: a worker or an explorer changes no backlog item through the CLI.
//!
//! The CLI takes its backlog actor from `HEADWORK_ROLE`, which a worker could leave out; this rule backs that trust by
//! refusing every `headwork backlog` subcommand but `list` in a worker's session (fail closed: an unknown subcommand is
//! refused too). Listing stays allowed, and an explorer adds its picked items through its `backlog_add` tool.

use super::shell::SimpleCommand;
use super::{Decision, RuleId};

/// The options `headwork backlog` (and `backlog list`) take a value after, as clap reads them: `--opt value` or
/// `--opt=value`.
const VALUE_OPTIONS: &[&str] = &["--project", "--owner", "--status", "--state"];

/// Refuses `headwork backlog` (also through its old name `todo`), passed as `args`, unless it only lists.
///
/// Fail closed: the option values of [`VALUE_OPTIONS`] are skipped as clap skips them, and the first other word must be
/// `list`, or there must be none (the bare `backlog` lists); any other subcommand word is denied.
pub(super) fn check_backlog_cli(args: &[String]) -> Option<Decision> {
    let mut words = args.iter().map(String::as_str);
    let mut name = words.next();
    while name.is_some_and(|word| word.starts_with('-')) {
        name = words.next();
    }
    if !matches!(name, Some("backlog" | "todo")) {
        return None;
    }
    let sub = loop {
        match words.next() {
            None => return None,
            Some(word) if VALUE_OPTIONS.contains(&word) => {
                words.next();
            }
            Some(word) if word.starts_with('-') => {}
            Some(word) => break word,
        }
    };
    (sub != "list").then(|| {
        Decision::deny(
            RuleId::WorkerBacklogCli,
            format!(
                "`headwork backlog {sub}` is not a listing: the backlog is the orchestrator's, so report what should change, \
                 or use your backlog tools; `headwork backlog list` stays open to you"
            ),
        )
    })
}

/// The command's words after `headwork` when it is one.
pub(super) fn backlog_command(command: &SimpleCommand) -> Option<Decision> {
    super::headwork_args(command).and_then(check_backlog_cli)
}
