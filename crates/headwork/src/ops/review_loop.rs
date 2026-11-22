//! The review loop (`docs/review-flows.md`, section 8).
//!
//! The reviewer lead takes ready nodes and hands their results back, delivers the findings, re-checks the implementing
//! lead's fixes and disputes, and finishes; the implementing lead resolves every finding. headwork stores each step, so
//! the order holds and a park loses nothing.

use anyhow::{Result, bail};
use headwork_core::review::{
    DisputeCall, Each, FindingText, Instance, NodeId, ReviewTarget, ReviewTrigger, ReviewerView,
    compose, ready, round_complete, round_due, settle_dispute,
};
use headwork_core::{Actor, FindingVerdict, NodeState, Resolution, ReviewState, Severity};
use headwork_store::{FindingRow, NewFinding, ReviewRow, Store, Task};
use serde::{Deserialize, Serialize};

use super::flows::{LoadedFlow, load_flow};
use super::{Env, slashed};

/// The header of the reviewer lead's ask about disputes it could not settle.
pub const DISPUTE_HEADER: &str = "Dispute";

/// The option that keeps a disputed finding.
pub const KEEP_OPTION: &str = "Keep the finding";

/// The option that drops a disputed finding.
pub const DROP_OPTION: &str = "Drop it";

/// One node instance to run, with the exact prompt for its subagent.
#[derive(Debug, Clone, Serialize)]
pub struct NodeToRun {
    /// The node.
    pub node: String,
    /// 0, or the finding an `each` node checks.
    pub item: i64,
    /// The model to run it on.
    pub model: String,
    /// The prompt, to pass as it is.
    pub prompt: String,
}

/// What `review_next` hands out.
#[derive(Debug, Clone, Serialize)]
pub struct Next {
    /// The review.
    pub review: i64,
    /// The round.
    pub round: u8,
    /// Nodes to start now, each as its own fresh subagent.
    pub nodes: Vec<NodeToRun>,
    /// Nodes still running.
    pub running: usize,
    /// What to do next.
    pub next: String,
}

/// A finding a node returned, as the reviewer lead hands it back.
#[derive(Debug, Clone, Deserialize)]
pub struct FindingIn {
    /// The file.
    pub file: Option<String>,
    /// The line.
    pub line: Option<u32>,
    /// `important`, `nit` or `doubt`.
    pub severity: Severity,
    /// What is wrong.
    pub summary: String,
    /// The failing scenario.
    pub scenario: String,
}

/// A node instance's result.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeDone {
    /// The node.
    pub node: String,
    /// The finding an `each` node checked; 0 or absent for a plain node.
    #[serde(default)]
    pub item: i64,
    /// A plain node's findings; an empty list when it found nothing.
    #[serde(default)]
    pub findings: Option<Vec<FindingIn>>,
    /// An `each` node's verdict.
    #[serde(default)]
    pub verdict: Option<FindingVerdict>,
    /// Why the subagent failed, when it did; the round goes on without it.
    #[serde(default)]
    pub failed: Option<String>,
}

/// The implementing lead's answer to one finding.
#[derive(Debug, Clone, Deserialize)]
pub struct Resolve {
    /// The finding's number.
    pub id: i64,
    /// `fixed` or `disputed`.
    pub action: LeadAction,
    /// The commit for a fix, the reason (and evidence) for a dispute.
    pub note: String,
}

/// What the implementing lead did with a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeadAction {
    /// Fixed in a commit.
    Fixed,
    /// Disputed with a reason.
    Disputed,
}

/// The reviewer lead's re-check of one fixed or disputed finding.
#[derive(Debug, Clone, Deserialize)]
pub struct Verdict {
    /// The finding's number.
    pub id: i64,
    /// For a fixed finding: `accepted` or `rejected`.
    #[serde(default)]
    pub fix: Option<FixCall>,
    /// For a disputed finding: `accepted` or `kept`.
    #[serde(default)]
    pub dispute: Option<DisputeView>,
    /// Why, for the implementing lead.
    #[serde(default)]
    pub note: Option<String>,
}

/// The re-check of a dispute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisputeView {
    /// The dispute meets the level's bar.
    Accepted,
    /// The finding still stands.
    Kept,
}

impl From<DisputeView> for ReviewerView {
    fn from(view: DisputeView) -> Self {
        match view {
            DisputeView::Accepted => Self::Accepts,
            DisputeView::Kept => Self::Keeps,
        }
    }
}

/// The re-check of a fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixCall {
    /// The fix holds.
    Accepted,
    /// The fix does not hold; the finding is open again.
    Rejected,
}

/// A step's outcome: where the review stands and what to do next.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    /// The review.
    pub review: ReviewRow,
    /// The findings this step concerns, numbered.
    pub findings: Vec<FindingRow>,
    /// Whom to tell with `SendMessage`, if anyone.
    pub tell: Option<String>,
    /// What to do next.
    pub next: String,
}

fn open_of_part(store: &Store, part: &Task) -> Result<ReviewRow> {
    match store.open_review(part.id)? {
        Some(review) if review.part_id == Some(part.id) => Ok(review),
        _ => bail!("this session runs no open review"),
    }
}

fn open_of_lead(store: &Store, lead: &Task) -> Result<ReviewRow> {
    match store.open_review(lead.id)? {
        Some(review) if review.task_id == lead.id => Ok(review),
        _ => bail!("this task has no open review"),
    }
}

fn flow_of(env: &Env<'_>, task: &Task, review: &ReviewRow) -> Result<LoadedFlow> {
    let allowed = env.config.allowed_models(env.project(task)?);
    load_flow(env.home.root(), &review.flow, &allowed).map_err(anyhow::Error::msg)
}

fn target_of(
    env: &Env<'_>,
    part: &Task,
    review: &ReviewRow,
    loaded: &LoadedFlow,
) -> Result<ReviewTarget> {
    let dir = env.task_dir(part)?;
    let (Some(base), Some(head)) = (review.base_sha.clone(), review.head_sha.clone()) else {
        bail!("review {} has no pinned commits", review.id);
    };
    let changed = env
        .worktrees(part)?
        .changed_files(&dir, &base, &head)
        .unwrap_or_default();
    Ok(ReviewTarget {
        kind: loaded.flow.target(),
        base,
        head,
        worktree: slashed(&dir),
        changed,
    })
}

fn complete(store: &Store, loaded: &LoadedFlow, review: &ReviewRow) -> Result<bool> {
    Ok(round_complete(
        &loaded.flow,
        review.level,
        &store.review_progress(review)?,
    ))
}

fn due(store: &Store, loaded: &LoadedFlow, review: &ReviewRow) -> Result<bool> {
    let fixed = store.finding_counts(review)?.fixed > 0;
    Ok(round_due(
        loaded.flow.level(review.level),
        review.round,
        fixed,
    ))
}

fn place(finding: &FindingRow) -> Option<String> {
    finding.file.as_ref().map(|file| {
        finding
            .line
            .map_or_else(|| file.clone(), |line| format!("{file}:{line}"))
    })
}

/// The ready nodes of the reviewer lead's review, marked running, at most `[reviews] parallel` running at once.
///
/// # Errors
///
/// Fails when the session runs no review, the review is not running, the flow does not load, or the store fails.
pub fn next(store: &mut Store, env: &Env<'_>, part: &Task) -> Result<Next> {
    let review = open_of_part(store, part)?;
    if review.state != ReviewState::Running {
        bail!(
            "review {} is {}: {}",
            review.id,
            review.state,
            state_hint(review.state)
        );
    }
    let loaded = flow_of(env, part, &review)?;
    let level = review.level;
    let progress = store.review_progress(&review)?;
    let running = progress
        .instances
        .values()
        .filter(|state| **state == NodeState::Running)
        .count();
    let room = usize::try_from(env.config.reviews.parallel.get())
        .unwrap_or(usize::MAX)
        .saturating_sub(running);
    let take: Vec<Instance> = ready(&loaded.flow, level, &progress)
        .into_iter()
        .take(room)
        .collect();
    let target = target_of(env, part, &review, &loaded)?;
    let findings = store.review_findings(review.id)?;
    let mut nodes = Vec::with_capacity(take.len());
    for instance in &take {
        let Some(node) = loaded.flow.node(instance.node.as_str()) else {
            continue;
        };
        let finding = findings
            .iter()
            .find(|row| row.id == instance.item)
            .map(|row| FindingText {
                id: row.id,
                node: row.node.clone(),
                place: place(row),
                severity: row.severity.to_string(),
                summary: row.summary.clone(),
                scenario: row.scenario.clone(),
            });
        let prompt = loaded.prompt(node).map_err(anyhow::Error::msg)?;
        nodes.push(NodeToRun {
            node: node.id.to_string(),
            item: instance.item,
            model: node.model_at(level).map_or_else(
                || env.config.models.default.to_string(),
                |model| model.to_string(),
            ),
            prompt: compose(
                node,
                &prompt,
                level,
                loaded.flow.level(level),
                &target,
                &review.focus,
                finding.as_ref(),
            ),
        });
    }
    store.take_review_nodes(&review, &take)?;
    let after = store.review_progress(&review)?;
    let now_running = after
        .instances
        .values()
        .filter(|state| **state == NodeState::Running)
        .count();
    let next = if !nodes.is_empty() {
        "Start each node as its own fresh `general-purpose` Agent subagent on its model with exactly its prompt, all at \
         once; hand each result back with `review_node_done`, then call `review_next` again."
            .to_owned()
    } else if round_complete(&loaded.flow, level, &after) {
        "Every node of this round is over: call `review_deliver`.".to_owned()
    } else {
        format!(
            "{now_running} node(s) still run; hand their results back with `review_node_done` first."
        )
    };
    Ok(Next {
        review: review.id,
        round: review.round,
        nodes,
        running: now_running,
        next,
    })
}

/// Records one node instance's result.
///
/// # Errors
///
/// Refuses a node the flow does not run at this level, an `each` node without its finding or verdict, a plain node
/// without its findings, or a review that is not running.
pub fn node_done(store: &mut Store, env: &Env<'_>, part: &Task, done: &NodeDone) -> Result<String> {
    let review = open_of_part(store, part)?;
    if review.state != ReviewState::Running {
        bail!(
            "review {} is {}: {}",
            review.id,
            review.state,
            state_hint(review.state)
        );
    }
    let loaded = flow_of(env, part, &review)?;
    let Some(node) = loaded
        .flow
        .node(&done.node)
        .filter(|node| node.runs_at(review.level))
    else {
        bail!(
            "`{}` is not a node of flow `{}` at level {}",
            done.node,
            review.flow,
            review.level
        );
    };
    let instance = Instance {
        node: NodeId::new(&done.node)?,
        item: done.item,
    };
    if let Some(why) = &done.failed {
        store.finish_review_node(&review, &instance, NodeState::Failed, &[], env.actor)?;
        return Ok(format!(
            "`{}` recorded as failed ({why}); the round goes on without it",
            done.node
        ));
    }
    if node.each == Some(Each::Finding) {
        let Some(verdict) = done.verdict else {
            bail!(
                "`{}` runs once per finding: give `item` (the finding's number) and `verdict`",
                done.node
            );
        };
        let checked = done.item;
        if !store
            .review_findings(review.id)?
            .iter()
            .any(|row| row.round == review.round && row.id == checked)
        {
            bail!("finding {} is not a finding of this round", done.item);
        }
        store.set_finding_verdict(done.item, verdict)?;
        store.finish_review_node(&review, &instance, NodeState::Done, &[], env.actor)?;
        return Ok(format!(
            "verdict on finding {} recorded: {verdict}",
            done.item
        ));
    }
    let Some(findings) = &done.findings else {
        bail!(
            "give `findings`: a list of {{file, line, severity, summary, scenario}}, empty when the node found nothing"
        );
    };
    let new: Vec<NewFinding<'_>> = findings
        .iter()
        .map(|finding| NewFinding {
            file: finding.file.as_deref(),
            line: finding.line,
            severity: finding.severity,
            summary: &finding.summary,
            scenario: &finding.scenario,
        })
        .collect();
    let ids = store.finish_review_node(&review, &instance, NodeState::Done, &new, env.actor)?;
    Ok(format!(
        "`{}` recorded with {} finding(s): {ids:?}",
        done.node,
        ids.len()
    ))
}

/// Delivers the round's findings: those a verify stage passed, or every one where none ran.
///
/// # Errors
///
/// Refuses while a node is ready or running (`review-nodes-open`).
pub fn deliver(store: &mut Store, env: &Env<'_>, part: &Task) -> Result<Step> {
    let review = open_of_part(store, part)?;
    let loaded = flow_of(env, part, &review)?;
    if !complete(store, &loaded, &review)? {
        bail!(
            "review-nodes-open: a node of this round is still ready or running; call `review_next`"
        );
    }
    let delivered: Vec<FindingRow> = store
        .review_findings(review.id)?
        .into_iter()
        .filter(|row| row.round == review.round && row.verdict.is_none_or(FindingVerdict::passes))
        .collect();
    let review = store.move_review(review.id, ReviewTrigger::Deliver, true, false, env.actor)?;
    for finding in &delivered {
        store.resolve_finding(finding.id, Resolution::Open, None, false)?;
    }
    let lead = store.task(review.task_id)?.name.to_string();
    let findings = store
        .review_findings(review.id)?
        .into_iter()
        .filter(|row| delivered.iter().any(|d| d.id == row.id))
        .collect::<Vec<_>>();
    let next = if findings.is_empty() {
        "Nothing to deliver: call `review_finish`.".to_owned()
    } else {
        format!(
            "Send `{lead}` these findings with `SendMessage`, numbered by their ids, each with its place, severity, \
             summary and scenario; it answers each with `review_resolve`. Then wait for its message."
        )
    };
    Ok(Step {
        review,
        findings,
        tell: Some(lead),
        next,
    })
}

/// The implementing lead's fixes and disputes, one per finding that waits on it.
///
/// # Errors
///
/// Refuses while a finding is left out, with uncommitted changes (`review-dirty`), a dispute of a finding the user
/// kept, or a review not waiting on the lead.
pub fn resolve(store: &mut Store, env: &Env<'_>, lead: &Task, items: &[Resolve]) -> Result<Step> {
    let review = open_of_lead(store, lead)?;
    if review.state != ReviewState::Delivered {
        bail!(
            "review {} is {}: nothing waits on you",
            review.id,
            review.state
        );
    }
    let waiting: Vec<FindingRow> = store
        .review_findings(review.id)?
        .into_iter()
        .filter(|row| row.resolution.is_some_and(Resolution::awaits_lead))
        .collect();
    let missing: Vec<i64> = waiting
        .iter()
        .filter(|row| !items.iter().any(|item| item.id == row.id))
        .map(|row| row.id)
        .collect();
    if !missing.is_empty() {
        bail!("answer every finding that waits on you; missing: {missing:?}");
    }
    for item in items {
        let Some(row) = waiting.iter().find(|row| row.id == item.id) else {
            bail!("finding {} does not wait on you", item.id);
        };
        if row.resolution == Some(Resolution::UserKept) && item.action == LeadAction::Disputed {
            bail!("the user kept finding {}: fix it", item.id);
        }
        if item.note.trim().is_empty() {
            bail!(
                "finding {}: give a note (the commit of a fix, the reason of a dispute)",
                item.id
            );
        }
    }
    let dir = env.task_dir(lead)?;
    let worktrees = env.worktrees(lead)?;
    if worktrees.is_dirty(&dir)? {
        bail!(
            "review-dirty: {} holds uncommitted changes; commit the fixes first",
            dir.display()
        );
    }
    let head = worktrees.resolve(&dir, "HEAD")?;
    let base = review.base_sha.clone().unwrap_or_default();
    if let Some(part) = review.part_id.map(|id| store.task(id)).transpose()? {
        worktrees.move_detached(&slashed(&env.task_dir(&part)?), &head)?;
    }
    store.set_review_target(review.id, None, &base, &head)?;
    for item in items {
        let resolution = match item.action {
            LeadAction::Fixed => Resolution::Fixed,
            LeadAction::Disputed => Resolution::Disputed,
        };
        store.resolve_finding(item.id, resolution, Some(&item.note), false)?;
    }
    let review = store.move_review(review.id, ReviewTrigger::Resolve, true, false, env.actor)?;
    let reviewer = review
        .part_id
        .map(|id| store.task(id))
        .transpose()?
        .map(|part| part.name.to_string());
    let findings = store
        .review_findings(review.id)?
        .into_iter()
        .filter(|row| items.iter().any(|item| item.id == row.id))
        .collect();
    Ok(Step {
        review,
        findings,
        next: format!(
            "Tell the reviewer lead `{}` with `SendMessage` that your answers are recorded; it re-checks them.",
            reviewer.as_deref().unwrap_or("review")
        ),
        tell: reviewer,
    })
}

/// The reviewer lead's re-check of every fixed or disputed finding.
///
/// # Errors
///
/// Refuses while a fixed or disputed finding is left out, a verdict of the wrong kind, or a review not re-checking.
pub fn verdict(store: &mut Store, env: &Env<'_>, part: &Task, items: &[Verdict]) -> Result<Step> {
    let review = open_of_part(store, part)?;
    if review.state != ReviewState::Rechecking {
        bail!(
            "review {} is {}: {}",
            review.id,
            review.state,
            state_hint(review.state)
        );
    }
    let loaded = flow_of(env, part, &review)?;
    let spec = loaded.flow.level(review.level);
    let waiting: Vec<FindingRow> = store
        .review_findings(review.id)?
        .into_iter()
        .filter(|row| {
            matches!(
                row.resolution,
                Some(Resolution::Fixed | Resolution::Disputed)
            )
        })
        .collect();
    let missing: Vec<i64> = waiting
        .iter()
        .filter(|row| !items.iter().any(|item| item.id == row.id))
        .map(|row| row.id)
        .collect();
    if !missing.is_empty() {
        bail!("re-check every fixed or disputed finding; missing: {missing:?}");
    }
    let mut calls = Vec::new();
    for item in items {
        let Some(row) = waiting.iter().find(|row| row.id == item.id) else {
            bail!("finding {} waits on no verdict", item.id);
        };
        let call = match (row.resolution, item.fix, item.dispute) {
            (Some(Resolution::Fixed), Some(FixCall::Accepted), None) => {
                (Resolution::FixAccepted, false)
            }
            (Some(Resolution::Fixed), Some(FixCall::Rejected), None) => {
                (Resolution::FixRejected, false)
            }
            (Some(Resolution::Disputed), None, Some(view)) => {
                match settle_dispute(spec, view.into(), row.argued) {
                    DisputeCall::Accept => (Resolution::DisputeAccepted, false),
                    DisputeCall::ArgueBack => (Resolution::Open, true),
                    DisputeCall::AskUser => (Resolution::Asked, false),
                }
            }
            _ => bail!(
                "finding {}: give `fix` for a fixed finding, `dispute` for a disputed one",
                item.id
            ),
        };
        calls.push((item, call));
    }
    for (item, (resolution, argued)) in calls {
        store.resolve_finding(item.id, resolution, item.note.as_deref(), argued)?;
    }
    let round_due = due(store, &loaded, &review)?;
    let review = store.move_review(
        review.id,
        ReviewTrigger::Verdict,
        true,
        round_due,
        env.actor,
    )?;
    after_recheck(store, review)
}

fn after_recheck(store: &Store, review: ReviewRow) -> Result<Step> {
    let lead = store.task(review.task_id)?.name.to_string();
    let all = store.review_findings(review.id)?;
    let (findings, tell, next) = match review.state {
        ReviewState::Asking => (
            all.into_iter()
                .filter(|row| row.resolution == Some(Resolution::Asked))
                .collect(),
            None,
            format!(
                "Ask the user in one AskUserQuestion headed `{DISPUTE_HEADER}`, one question per finding below, each \
                 starting `Finding <id>:` with its summary, the dispute and your view, and the options `{KEEP_OPTION}` \
                 and `{DROP_OPTION}`. headwork reads the answers."
            ),
        ),
        ReviewState::Delivered => (
            all.into_iter()
                .filter(|row| row.resolution.is_some_and(Resolution::awaits_lead))
                .collect(),
            Some(lead.clone()),
            format!(
                "Send `{lead}` the findings below with `SendMessage`: rejected fixes and disputes you argue back on, \
                 with your notes; it answers with `review_resolve` again."
            ),
        ),
        ReviewState::Running => (
            Vec::new(),
            None,
            format!(
                "Round {} runs every node again on the fixed commit: call `review_next`.",
                review.round
            ),
        ),
        ReviewState::Requested
        | ReviewState::Rechecking
        | ReviewState::Done
        | ReviewState::Cancelled => (
            Vec::new(),
            None,
            "Everything is settled: call `review_finish`.".to_owned(),
        ),
    };
    Ok(Step {
        review,
        findings,
        tell,
        next,
    })
}

/// Finishes the review once nothing is left to fix, re-check, ask or run.
///
/// # Errors
///
/// Refuses with the state machine's reason.
pub fn finish(store: &mut Store, env: &Env<'_>, part: &Task) -> Result<Step> {
    let review = open_of_part(store, part)?;
    let loaded = flow_of(env, part, &review)?;
    let round_due = due(store, &loaded, &review)?;
    let review = store.move_review(review.id, ReviewTrigger::Finish, true, round_due, env.actor)?;
    let findings = store.review_findings(review.id)?;
    let lead = store.task(review.task_id)?.name.to_string();
    let count = |wanted: &[Resolution]| {
        findings
            .iter()
            .filter(|row| row.resolution.is_some_and(|r| wanted.contains(&r)))
            .count()
    };
    let next = format!(
        "Review {} is finished: {} fixed, {} disputes accepted, {} dropped by the user. Send `{lead}` this outcome in one \
         line with `SendMessage`, report `done`, then close yourself with `done`.",
        review.id,
        count(&[Resolution::FixAccepted]),
        count(&[Resolution::DisputeAccepted]),
        count(&[Resolution::UserDropped]),
    );
    Ok(Step {
        review,
        findings: Vec::new(),
        tell: Some(lead),
        next,
    })
}

/// Records the user's answers to a `Dispute` ask of a reviewer lead.
///
/// `Keep the finding` keeps it for the implementing lead to fix, `Drop it` drops it; a question names its finding as
/// `Finding <id>`. When no dispute waits any more the review moves on.
///
/// # Errors
///
/// Fails when the store fails.
pub fn answer_disputes(
    store: &mut Store,
    env: &Env<'_>,
    part: &Task,
    answers: &[(String, String)],
) -> Result<bool> {
    let Ok(review) = open_of_part(store, part) else {
        return Ok(false);
    };
    if review.state != ReviewState::Asking {
        return Ok(false);
    }
    let asked: Vec<i64> = store
        .review_findings(review.id)?
        .into_iter()
        .filter(|row| row.resolution == Some(Resolution::Asked))
        .map(|row| row.id)
        .collect();
    for (question, answer) in answers {
        let Some(id) = finding_number(question).filter(|id| asked.contains(id)) else {
            continue;
        };
        let answer = answer.trim().to_lowercase();
        let resolution = if answer.starts_with("keep") {
            Resolution::UserKept
        } else if answer.starts_with("drop") {
            Resolution::UserDropped
        } else {
            continue;
        };
        store.resolve_finding(id, resolution, None, false)?;
    }
    if store.finding_counts(&review)?.asked > 0 {
        return Ok(true);
    }
    let loaded = flow_of(env, part, &review)?;
    let round_due = due(store, &loaded, &review)?;
    store.move_review(
        review.id,
        ReviewTrigger::Answer,
        true,
        round_due,
        Actor::Hook,
    )?;
    Ok(true)
}

fn finding_number(question: &str) -> Option<i64> {
    let lower = question.to_lowercase();
    let at = lower.find("finding")?;
    let rest = lower
        .get(at + "finding".len()..)?
        .trim_start()
        .trim_start_matches('#');
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Cancels the open review a closing or dropped task belongs to, as its implementing lead or its reviewer lead.
///
/// # Errors
///
/// Fails when the store fails.
pub fn cancel_open(store: &mut Store, task: &Task, actor: Actor) -> Result<()> {
    if let Some(review) = store.open_review(task.id)? {
        store.move_review(review.id, ReviewTrigger::Cancel, false, false, actor)?;
    }
    Ok(())
}

/// Whether a reviewer lead may not stop yet: its review has a ready node, or a resolution waits on its verdict.
///
/// # Errors
///
/// Fails when the store fails or the flow does not load.
pub fn reviewer_busy(store: &Store, env: &Env<'_>, part: &Task) -> Result<bool> {
    let Ok(review) = open_of_part(store, part) else {
        return Ok(false);
    };
    match review.state {
        ReviewState::Running => {
            let loaded = flow_of(env, part, &review)?;
            Ok(!ready(&loaded.flow, review.level, &store.review_progress(&review)?).is_empty())
        }
        ReviewState::Rechecking => Ok(true),
        ReviewState::Requested
        | ReviewState::Delivered
        | ReviewState::Asking
        | ReviewState::Done
        | ReviewState::Cancelled => Ok(false),
    }
}

const fn state_hint(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Requested => "it has not started",
        ReviewState::Running => "call `review_next`",
        ReviewState::Delivered => "the implementing lead answers with `review_resolve`",
        ReviewState::Rechecking => "re-check the answers with `review_verdict`",
        ReviewState::Asking => "the user answers the dispute ask",
        ReviewState::Done | ReviewState::Cancelled => "it is over",
    }
}
