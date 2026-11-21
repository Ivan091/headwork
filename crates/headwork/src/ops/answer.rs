//! `answer`: the user's answer to an ask the tick raised, such as keeping or deleting a worktree the remove command
//! refused.
//!
//! An ask a session put through its `AskUserQuestion` dialog is answered in that dialog and recorded by its hook. An
//! ask the tick raised has no dialog: its session may be gone, as when a dropped task's worktree holds changes. The
//! user answers it with `headwork answer <id> <choice>`, or tells the orchestrator, which asks with its own dialog and
//! passes on only the user's pick through its `answer` tool. Either way the answer names one of the ask's options, is
//! recorded with who gave it, and is carried out at once.

use anyhow::{Context, Result, bail};
use headwork_core::{Actor, TaskState};
use headwork_store::{AskId, Store, StoredAsk};
use serde::Serialize;

use super::Env;
use super::close::{CloseReport, finish};
use super::tick::{WORKTREE_TOPIC, carry_out_worktree_answer};

/// What an answer did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnswerReport {
    /// The ask.
    pub ask: i64,
    /// Its task, as `<project>-<slug>`.
    pub task: Option<String>,
    /// The option chosen, in full.
    pub answer: String,
    /// Who answered: the user at the CLI, or the orchestrator passing on the user's pick.
    pub by: Actor,
    /// The close the answer let finish, when it was a closing task's worktree ask.
    pub close: Option<CloseReport>,
}

/// Answers an ask the tick raised with one of its options and carries the answer out.
///
/// `choice` is an option's number, from 1, or its text, matched whole and without regard to case.
///
/// # Errors
///
/// Fails, changing nothing, when the actor is a worker, the ask is unknown, belongs to a session's own dialog, no
/// longer waits, or `choice` names none of its options; also when the store or the close fails.
pub fn answer(store: &mut Store, env: &Env<'_>, id: AskId, choice: &str) -> Result<AnswerReport> {
    if env.actor == Actor::Worker {
        bail!("a worker does not answer the tick's asks: they wait on the user");
    }
    let ask = store
        .ask(id)?
        .with_context(|| format!("no ask with id {}", id.0))?;
    if let Some(session) = &ask.session {
        bail!(
            "ask {} was asked in session {session}'s own dialog; the user answers it there",
            id.0
        );
    }
    if !ask.state.is_waiting() {
        bail!(
            "ask {} is {}: it no longer waits on an answer",
            id.0,
            ask.state
        );
    }
    let Some(option) = chosen(&ask, choice) else {
        bail!(
            "`{choice}` is none of the options of ask {}: {}",
            id.0,
            numbered(&ask.options)
        );
    };
    store.answer_ask(id, &option, env.actor)?;
    let task = ask.task.map(|task| store.task(task)).transpose()?;
    let close = match &task {
        Some(task) if ask.topic.as_deref() == Some(WORKTREE_TOPIC) => {
            carry_out_worktree_answer(store, env, task.id)?;
            if store.task(task.id)?.state == TaskState::Closing {
                Some(finish(store, env, task.id)?)
            } else {
                None
            }
        }
        Some(_) | None => None,
    };
    Ok(AnswerReport {
        ask: id.0,
        task: task.map(|task| task.name.to_string()),
        answer: option,
        by: env.actor,
        close,
    })
}

/// The option `choice` names: its number from 1, or its whole text without regard to case.
fn chosen(ask: &StoredAsk, choice: &str) -> Option<String> {
    let choice = choice.trim();
    let by_number = choice
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| ask.options.get(i));
    by_number
        .or_else(|| {
            ask.options
                .iter()
                .find(|option| option.eq_ignore_ascii_case(choice))
        })
        .cloned()
}

/// The options as `1 "…", 2 "…"`.
fn numbered(options: &[String]) -> String {
    options
        .iter()
        .enumerate()
        .map(|(i, option)| format!("{} \"{option}\"", i + 1))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use headwork_core::AskState;

    fn ask() -> StoredAsk {
        StoredAsk {
            id: AskId(3),
            task: None,
            session: None,
            topic: Some(WORKTREE_TOPIC.to_owned()),
            question: "Keep or delete?".to_owned(),
            options: vec![
                "Keep the worktree (Recommended)".to_owned(),
                "Delete it".to_owned(),
            ],
            state: AskState::Pending,
        }
    }

    #[test]
    fn a_choice_is_a_number_or_the_whole_text() {
        let ask = ask();
        let keep = Some("Keep the worktree (Recommended)".to_owned());
        let delete = Some("Delete it".to_owned());
        assert_eq!(chosen(&ask, "1"), keep, "by number");
        assert_eq!(chosen(&ask, " 2 "), delete, "by number, trimmed");
        assert_eq!(chosen(&ask, "delete it"), delete, "by text, any case");
        assert_eq!(chosen(&ask, "Delete"), None, "a prefix names nothing");
        assert_eq!(chosen(&ask, "0"), None, "numbers start at 1");
        assert_eq!(chosen(&ask, "3"), None, "past the last option");
    }

    #[test]
    fn a_refusal_lists_the_options() {
        assert_eq!(
            numbered(&ask().options),
            "1 \"Keep the worktree (Recommended)\", 2 \"Delete it\"",
            "numbered"
        );
    }
}
