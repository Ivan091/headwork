//! `/api/kanban`: the facts of the board read from the store, the pure build in [`headwork_core::kanban`].

use anyhow::Result;
use headwork_core::digest::waiting_items;
use headwork_core::gate::GateClass;
use headwork_core::kanban::{CardReview, Kanban, Options, TaskCard, build};
use headwork_core::usage::UsageConfig;
use headwork_store::{BacklogFilter, Store};

use crate::ops::status::{StatusLine, lines_waiting};

/// The board: every item, the top-level tasks not closed, and the ids waiting on the user.
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
    Ok(build(&items, &tasks, &waiting_ids, &today, options))
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
