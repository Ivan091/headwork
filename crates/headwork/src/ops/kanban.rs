//! `/api/kanban`: the facts of the board read from the store, the pure build in [`headwork_core::kanban`].

use std::collections::BTreeMap;

use anyhow::Result;
use headwork_core::digest::waiting_items;
use headwork_core::gate::GateClass;
use headwork_core::kanban::{CardReview, Kanban, Options, TaskCard, build};
use headwork_core::usage::UsageConfig;
use headwork_store::{BacklogFilter, Store, TaskId, all_models};

use crate::ops::status::{StatusLine, lines_waiting};

/// The board: every item (an epic as a lane, its children's tokens summed), the top-level tasks not closed,
/// and the ids waiting on the user.
///
/// `waiting` holds the live gate waiters as `(task id, class)`; `prices` makes the estimate.
///
/// # Errors
///
/// Returns the store's failure.
pub fn kanban(
    store: &Store,
    prices: &UsageConfig,
    waiting: &[(i64, GateClass)],
    options: &Options,
) -> Result<Kanban> {
    let today = store.backlog_today()?;
    let items = store.backlog_items(&BacklogFilter::default())?;
    let open: Vec<_> = items
        .iter()
        .filter(|item| item.draft.status.kind().is_open())
        .cloned()
        .collect();
    let waiting_ids = waiting_items(&open, &today);
    let board = store.board(false)?;
    let tasks: Vec<TaskCard> = lines_waiting(store, &board, prices, waiting)?
        .into_iter()
        .map(card)
        .collect();
    let usage = store.usage_task_tokens()?;
    let spend: BTreeMap<i64, u64> = items
        .iter()
        .filter_map(|item| item.task.as_ref())
        .map(|task| (task.id, all_models(&usage.tree(TaskId(task.id))).total()))
        .collect();
    Ok(build(&items, &tasks, &waiting_ids, &spend, &today, options))
}

fn card(line: StatusLine) -> TaskCard {
    TaskCard {
        id: line.task_id,
        name: line.task,
        project: line.project,
        state: line.state,
        status: line.status,
        model: line.model,
        effort: line.effort,
        review_plan: line.review_plan,
        review: line.review.map(|review| CardReview {
            level: review.level,
            round: review.round,
            state: review.state,
            findings: review.findings_awaiting_lead,
        }),
        prs: line.prs,
        parts: line.parts,
        asking_parts: line.asking_parts,
        tokens: line.tokens,
        estimate: line.estimate,
        findings: u32::try_from(line.findings.len()).unwrap_or(u32::MAX),
        waiting_for: line.waiting_for.map(|class| class.to_string()),
        silent: line.close == headwork_core::CloseMode::Silent,
        dead_dependency: line.dead_dependency,
        updated_at: line.updated_at,
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "tests")]

    use headwork_core::gate::GateClass;
    use headwork_core::{CloseMode, TaskState, TaskStatus};

    use super::*;
    use crate::ops::status::StatusReview;

    #[test]
    fn a_status_line_maps_field_by_field_onto_its_card() {
        let line = StatusLine {
            task: "demo-plan".to_owned(),
            task_id: 7,
            project: "demo".to_owned(),
            model: "opus".to_owned(),
            effort: "high".to_owned(),
            parts: 3,
            state: TaskState::InReview,
            updated_at: "2026-11-28 10:00:00".to_owned(),
            status: TaskStatus::InReview,
            prs: vec!["https://github.com/o/r/pull/9".to_owned()],
            asking_parts: vec!["demo-plan-a".to_owned(), "demo-plan-b".to_owned()],
            dead_dependency: Some("demo-gone closed".to_owned()),
            line: String::new(),
            tokens: 1234,
            estimate: Some("$1.50".to_owned()),
            findings: vec!["stuck (open 1h)".to_owned(), "idle (open 2h)".to_owned()],
            review: Some(StatusReview {
                level: 2,
                flow: "code".to_owned(),
                round: 3,
                state: "running".to_owned(),
                focus: String::new(),
                findings_awaiting_lead: 5,
            }),
            review_tokens: 0,
            close: CloseMode::Silent,
            review_plan: Some(3),
            waiting_for: Some(GateClass::Gpu),
        };
        let card = card(line);
        assert_eq!(
            (card.id, card.name.as_str(), card.project.as_str()),
            (7, "demo-plan", "demo")
        );
        assert_eq!(
            (card.model.as_str(), card.effort.as_str()),
            ("opus", "high")
        );
        assert_eq!(
            (card.state, card.status),
            (TaskState::InReview, TaskStatus::InReview)
        );
        assert_eq!(
            card.prs,
            vec!["https://github.com/o/r/pull/9".to_owned()],
            "prs"
        );
        assert_eq!(
            (card.parts, card.asking_parts.len()),
            (3, 2),
            "parts and asking parts"
        );
        assert_eq!(
            card.dead_dependency.as_deref(),
            Some("demo-gone closed"),
            "dead dependency"
        );
        assert_eq!(
            (card.tokens, card.estimate.as_deref()),
            (1234, Some("$1.50")),
            "tokens"
        );
        assert_eq!(card.findings, 2, "findings are counted");
        let review = card.review.unwrap();
        assert_eq!(
            (
                review.level,
                review.round,
                review.state.as_str(),
                review.findings
            ),
            (2, 3, "running", 5),
            "review"
        );
        assert_eq!(card.review_plan, Some(3), "review plan");
        assert_eq!(card.waiting_for.as_deref(), Some("gpu"), "gate class");
        assert!(card.silent, "silent close");
        assert_eq!(card.updated_at, "2026-11-28 10:00:00", "updated_at");
    }
}
