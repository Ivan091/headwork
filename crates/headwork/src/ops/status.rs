//! `status`: one line per top-level task, rolled up from its parts (`docs/leads.md`, section 4).
//!
//! The words are `working`, `needs you`, `in review <PR URLs>`, `finished`, `failed` and the rest of [`TaskStatus`].
//! It is what the orchestrator tells the user unless asked for more. The roll-up itself is [`headwork_core::status`].

use anyhow::Result;
use headwork_core::usage::UsageConfig;
use headwork_core::{StatusFacts, TaskShape, TaskState, TaskStatus};
use headwork_store::{Board, BoardTask, ByModel, Store};
use serde::Serialize;

/// One top-level task's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusLine {
    /// The task's name.
    pub task: String,
    /// The rolled-up status.
    pub status: TaskStatus,
    /// The open pull requests of the task and its parts.
    pub prs: Vec<String>,
    /// The parts that wait on the user.
    pub asking_parts: Vec<String>,
    /// The line as the orchestrator says it.
    pub line: String,
    /// The tokens the task and its parts spent (`docs/usage.md`).
    pub tokens: u64,
    /// Their estimate at API list prices, when `[usage.prices]` holds any.
    pub estimate: Option<String>,
    /// The review running on the task, if one is open.
    pub review: Option<StatusReview>,
}

/// The open review of a task, as `status --json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusReview {
    /// The level: 1 light, 2 standard, 3 strict.
    pub level: u8,
    /// The flow's name.
    pub flow: String,
    /// The round, from 1.
    pub round: u8,
    /// The review's state word (`requested`, `running`, `delivered`, `rechecking`, `asking`).
    pub state: String,
    /// The user's focus; empty when none.
    pub focus: String,
    /// Findings waiting on the implementing lead.
    pub findings_awaiting_lead: u32,
}

/// The status of every top-level task on the board: not closed ones, or all with `all`.
///
/// # Errors
///
/// Returns the store's failure.
pub fn status(store: &Store, all: bool, prices: &UsageConfig) -> Result<Vec<StatusLine>> {
    lines(store, &store.board(all)?, prices)
}

/// The status lines of a board's tasks. A part rolls up into its lead; a part whose lead is not on the board gets a
/// line of its own, so nothing open hides.
///
/// # Errors
///
/// Returns the store's failure.
pub fn lines(store: &Store, board: &Board, prices: &UsageConfig) -> Result<Vec<StatusLine>> {
    let shown = |entry: &BoardTask| match entry.task.shape {
        TaskShape::Part { parent } => board.tasks.iter().all(|other| other.task.id != parent),
        TaskShape::Lead | TaskShape::Plain => true,
    };
    let mut out = Vec::new();
    for top in board.tasks.iter().filter(|entry| shown(entry)) {
        let parts: Vec<&BoardTask> = board
            .tasks
            .iter()
            .filter(|entry| entry.task.shape.parent() == Some(top.task.id))
            .collect();
        let part_facts = parts
            .iter()
            .map(|part| facts(store, part))
            .collect::<Result<Vec<_>>>()?;
        let status = headwork_core::status(facts(store, top)?, &part_facts);
        let prs: Vec<String> = std::iter::once(top)
            .chain(parts.iter().copied())
            .filter_map(|entry| entry.pr.as_ref())
            .filter(|pr| pr.state == "open")
            .map(|pr| pr.url.clone())
            .collect();
        let asking_parts: Vec<String> = parts
            .iter()
            .zip(&part_facts)
            .filter(|(_, facts)| {
                !matches!(facts.state, TaskState::Closing | TaskState::Closed)
                    && (facts.asking || facts.state == TaskState::NeedsUser)
            })
            .map(|(part, _)| part.task.name.to_string())
            .collect();
        let task = top.task.name.to_string();
        let line = match status {
            TaskStatus::InReview if !prs.is_empty() => {
                format!("{task}: in review {}", prs.join(" "))
            }
            TaskStatus::NeedsYou if !asking_parts.is_empty() => {
                format!("{task}: needs you (in {})", asking_parts.join(", "))
            }
            TaskStatus::Queued
            | TaskStatus::Working
            | TaskStatus::NeedsYou
            | TaskStatus::InReview
            | TaskStatus::Finished
            | TaskStatus::Failed
            | TaskStatus::Interrupted
            | TaskStatus::Parked
            | TaskStatus::Dropped => format!("{task}: {status}"),
        };
        out.push(StatusLine {
            task,
            status,
            prs,
            asking_parts,
            line,
            tokens: top.tokens,
            estimate: estimate(prices, &top.models),
            review: review(store, top)?,
        });
    }
    Ok(out)
}

fn review(store: &Store, entry: &BoardTask) -> Result<Option<StatusReview>> {
    let Some(row) = store.open_review(entry.task.id)? else {
        return Ok(None);
    };
    let counts = store.finding_counts(&row)?;
    Ok(Some(StatusReview {
        level: row.level.number(),
        flow: row.flow,
        round: row.round,
        state: row.state.to_string(),
        focus: row.focus,
        findings_awaiting_lead: counts.awaiting_lead,
    }))
}

fn facts(store: &Store, entry: &BoardTask) -> Result<StatusFacts> {
    Ok(StatusFacts {
        state: entry.task.state,
        asking: !entry.asks.is_empty(),
        dropped: entry.task.state == TaskState::Closed && store.was_dropped(entry.task.id)?,
    })
}

/// A task's estimate as printed, when `[usage.prices]` holds any price and the task spent tokens.
#[must_use]
pub fn estimate(prices: &UsageConfig, models: &ByModel) -> Option<String> {
    if prices.prices.is_empty() || models.is_empty() {
        return None;
    }
    Some(
        prices
            .estimate(
                models
                    .iter()
                    .map(|(model, tokens)| (model.as_str(), tokens)),
            )
            .to_string(),
    )
}
