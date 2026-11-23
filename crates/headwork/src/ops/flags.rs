//! The flags of a spawn (`close`, `review`, who chose them and why), checked the same way for the CLI and the MCP tools.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use headwork_core::flags::{check_decision, planned_review, review_from_number};
use headwork_core::review::Level;
use headwork_core::{Actor, CloseMode, Config, DecidedBy, FlagDecision, PlanMoment, PlannedReview};
use headwork_store::{ReviewError, Store, Task};

/// What a spawn's flag arguments resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnFlags {
    /// The close mode: the one named, else the kind's default.
    pub close: CloseMode,
    /// The planned review; none for level 0 or when absent.
    pub review: Option<Level>,
    /// How the flags were chosen, when the spawn named `close` or `review`.
    pub decision: Option<FlagDecision>,
}

/// Resolves and checks the flag arguments of a spawn.
///
/// # Errors
///
/// Fails when `review` is above 3, `decided_by` is not `user` or `judged`, or a judged choice has no reason.
pub fn resolve(
    kind: &str,
    close: Option<CloseMode>,
    review: Option<u8>,
    decided_by: Option<&str>,
    reason: Option<&str>,
) -> Result<SpawnFlags> {
    let planned = match review {
        Some(number) => review_from_number(number)
            .map_err(|bad| anyhow!("`review` must be 0, 1, 2 or 3, not {bad}"))?,
        None => None,
    };
    let decided_by = match decided_by {
        Some(word) => match DecidedBy::parse(word) {
            Some(by) => Some(by),
            None => bail!("`decided_by` must be `user` or `judged`, not `{word}`"),
        },
        None => None,
    };
    let decision = check_decision(close.is_some() || review.is_some(), decided_by, reason)?;
    Ok(SpawnFlags {
        close: close.unwrap_or_else(|| CloseMode::default_for(kind)),
        review: planned,
        decision,
    })
}

/// The event a planned review logs when it is requested.
pub const EVENT_REVIEW_PLANNED: &str = "review-planned";

/// What happened to a task's planned review at a moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedOutcome {
    /// Nothing was planned, or it is not due.
    None,
    /// A review was requested and the plan cleared.
    Requested {
        /// The review's id.
        review: i64,
        /// Its level.
        level: Level,
        /// Its flow.
        flow: String,
    },
    /// A review of the task was already open; the plan was cleared.
    AlreadyOpen,
    /// The flow did not load; the plan was cleared and the user must be told.
    FlowBroken(String),
}

impl PlannedOutcome {
    /// The line telling the lead what was planned, when a review was requested.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Requested {
                review,
                level,
                flow,
            } => Some(format!(
                "headwork: a level-{level} review (review {review}) with the flow {flow} was planned for this task. Commit your \
                 work, then call the headwork `review_start` tool now."
            )),
            Self::FlowBroken(problem) => Some(format!(
                "headwork: a review was planned for this task, but {problem}; no review started. Tell the user."
            )),
            Self::None | Self::AlreadyOpen => None,
        }
    }
}

/// Starts a task's planned review if it is due at `moment`.
///
/// It requests the review exactly as the user's `review N` prompt does,
/// clears the plan in the same store operation and logs `review-planned`. A review already open or a flow that does not
/// load only clears the plan.
///
/// # Errors
///
/// Fails when the store fails.
pub fn start_planned(
    store: &mut Store,
    config: &Config,
    home: &Path,
    task: &Task,
    moment: PlanMoment,
    actor: Actor,
) -> Result<PlannedOutcome> {
    let PlannedReview::Request(level) = planned_review(
        task.shape.is_lead(),
        config.reviews.enabled,
        task.review_plan,
        moment,
    ) else {
        return Ok(PlannedOutcome::None);
    };
    let flow = config.reviews.flow_for(&task.kind).to_owned();
    let allowed = config.project(task.project()).map_or_else(
        || config.models.allowed.clone(),
        |project| config.allowed_models(project),
    );
    if let Err(problem) = super::flows::load_flow(home, &flow, &allowed) {
        store.clear_review_plan(task.id)?;
        store.append_event(
            actor,
            Some(task.id),
            None,
            EVENT_REVIEW_PLANNED,
            &serde_json::json!({ "level": level.number(), "flow": flow, "error": problem }),
        )?;
        return Ok(PlannedOutcome::FlowBroken(problem));
    }
    match store.request_review(task.id, &flow, level, "", actor) {
        Ok(review) => {
            store.append_event(
                actor,
                Some(task.id),
                None,
                EVENT_REVIEW_PLANNED,
                &serde_json::json!({ "level": level.number(), "flow": flow, "review": review.id }),
            )?;
            Ok(PlannedOutcome::Requested {
                review: review.id,
                level,
                flow,
            })
        }
        Err(ReviewError::Running { .. }) => {
            store.clear_review_plan(task.id)?;
            Ok(PlannedOutcome::AlreadyOpen)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn nothing_named_keeps_the_kind_default() {
        let flags = resolve("silent", None, None, None, None).unwrap();
        assert_eq!(flags.close, CloseMode::Silent);
        assert_eq!(flags.review, None);
        assert_eq!(flags.decision, None);
        assert_eq!(
            resolve("task", None, None, None, None).unwrap().close,
            CloseMode::Confirm
        );
    }

    #[test]
    fn an_explicit_close_beats_the_kind() {
        let flags = resolve("silent", Some(CloseMode::Confirm), None, Some("user"), None).unwrap();
        assert_eq!(flags.close, CloseMode::Confirm);
    }

    #[test]
    fn judged_flags_need_a_reason() {
        assert!(resolve("task", Some(CloseMode::Silent), None, Some("judged"), None).is_err());
        assert!(resolve("task", None, Some(2), None, None).is_err());
        let flags = resolve("task", None, Some(2), Some("judged"), Some("new schema")).unwrap();
        assert_eq!(flags.review, Some(Level::Standard));
        assert!(resolve("task", None, Some(0), Some("user"), None).is_ok());
        assert!(resolve("task", None, Some(4), Some("user"), None).is_err());
        assert!(resolve("task", None, None, Some("sure"), None).is_err());
    }
}
