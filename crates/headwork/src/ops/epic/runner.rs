//! The epic runner (`docs/backlog.md`, section 13; `docs/decisions/2026-11-28-epic-runner.md`): after every queue pump it
//! starts the ready children of each epic in progress, re-plans an epic whose children are lost or all finished, and
//! blocks an epic that re-planned too often.
//!
//! Each start is a claim ([`Store::claim_epic_child`]), the ordinary [`spawn`], then the link; the pure decision is
//! [`epic_step`]. The runner is best effort: an error is logged as [`EVENT_RUNNER_ERROR`] and never fails the pump.

use anyhow::{Result, anyhow};
use headwork_core::backlog::{
    BacklogActor, BacklogDraft, BacklogItem, BacklogOwner, BacklogStatus, BlockReason, ChildBrief,
    EpicLimits, EpicStep, ItemKind, ItemStatus, ReplanBrief, ReplanTrigger, Target, child_brief,
    child_slug, epic_step, replan_brief, with_writes_line,
};
use headwork_core::flags::review_from_number;
use headwork_core::{
    CloseMode, Config, DecidedBy, EXPLORE_KIND, FlagDecision, MachineMode, Model, PermissionMode,
    ProjectName, TaskRequest, TaskState,
};
use headwork_store::{
    BacklogFilter, BacklogMove, EpicClaim, ItemPlan, ItemShape, NewReplan, ReplanRow, Store, Task,
    TaskId,
};

use super::{DEFAULT_CHILD_KIND, free_slug, plan_path, write_new};
use crate::ops::Env;
use crate::ops::backlog::export_after;
use crate::ops::spawn::{Pumped, SpawnRequest, spawn};

/// The event of a runner step that failed (`error`, and `epic` when one epic's step failed); the pump goes on.
pub const EVENT_RUNNER_ERROR: &str = "epic-runner-error";

/// The event of a ready child whose project headwork does not run (`item`, `epic`, `project`), once per child.
pub const EVENT_CHILD_SKIPPED: &str = "epic-child-skipped";

/// The event of a pass that started nothing because `[watch] many_sessions` left no room (`reason`, `live`, `limit`).
pub const EVENT_HELD: &str = "epic-held";

/// Why a review planned for a child was chosen, in its spawn's flag decision.
const PLANNED_REVIEW: &str = "planned by the epic explorer";

/// The longest block note the runner writes for a failed spawn.
const NOTE_CHARS: usize = 300;

/// Runs one pass over every epic in progress and returns the starts as queue lines. Does nothing unless the machine is
/// in `normal` mode; an error is logged as [`EVENT_RUNNER_ERROR`], never returned.
pub fn run(store: &mut Store, env: &Env<'_>) -> Vec<Pumped> {
    let mut out = Vec::new();
    if let Err(error) = pass(store, env, &mut out) {
        logged_error(store, env, None, &error);
    }
    out
}

fn logged_error(store: &Store, env: &Env<'_>, epic: Option<i64>, error: &anyhow::Error) {
    drop(store.append_event(
        env.actor,
        None,
        None,
        EVENT_RUNNER_ERROR,
        &serde_json::json!({ "epic": epic, "error": format!("{error:#}") }),
    ));
}

fn runs(config: &Config, project: &str) -> bool {
    ProjectName::new(project).is_ok_and(|name| config.project(&name).is_some())
}

/// What one pass carries from epic to epic.
#[derive(Debug)]
struct Pass {
    room: Option<usize>,
    held_logged: bool,
    live: usize,
}

fn pass(store: &mut Store, env: &Env<'_>, out: &mut Vec<Pumped>) -> Result<()> {
    if store.machine_mode()? != MachineMode::Normal {
        return Ok(());
    }
    let limit = env.config.watch.many_sessions;
    let live = store.live_session_pids()?.len();
    let mut state = Pass {
        room: (limit != 0).then(|| {
            usize::try_from(limit)
                .unwrap_or(usize::MAX)
                .saturating_sub(live)
        }),
        held_logged: false,
        live,
    };
    let mut epics: Vec<BacklogItem> = store
        .backlog_items(&BacklogFilter {
            statuses: vec![ItemStatus::InProgress],
            ..BacklogFilter::default()
        })?
        .into_iter()
        .filter(|item| item.kind == ItemKind::Epic)
        .collect();
    epics.sort_by_key(|item| item.id);
    for epic in &epics {
        if let Err(error) = run_epic(store, env, epic, &mut state, out) {
            logged_error(store, env, Some(epic.id), &error);
        }
    }
    Ok(())
}

fn run_epic(
    store: &mut Store,
    env: &Env<'_>,
    epic: &BacklogItem,
    state: &mut Pass,
    out: &mut Vec<Pumped>,
) -> Result<()> {
    let config = env.config;
    for _ in 0..2 {
        let facts = store.epic_facts(epic.id, |project| runs(config, project))?;
        let limits = EpicLimits {
            max_parallel: config.epics.max_parallel,
            max_replans: config.epics.max_replans,
            room: state.room,
        };
        match epic_step(&facts, &limits) {
            EpicStep::Hold => return Ok(()),
            EpicStep::Churn { replans } => {
                if store
                    .block_epic_churn(epic.id, replans, env.actor)?
                    .is_some()
                {
                    drop(export_after(store, env));
                }
                return Ok(());
            }
            EpicStep::Replan(trigger) => {
                if !add_replan(store, env, epic, &trigger)? {
                    return Ok(());
                }
            }
            EpicStep::Run {
                starts,
                skipped,
                held,
            } => {
                for id in skipped {
                    skip_once(store, env, epic.id, id)?;
                }
                if held && !state.held_logged {
                    state.held_logged = true;
                    store.append_event(
                        env.actor,
                        None,
                        None,
                        EVENT_HELD,
                        &serde_json::json!({
                            "reason": "many-sessions",
                            "live": state.live,
                            "limit": config.watch.many_sessions,
                        }),
                    )?;
                }
                for id in starts {
                    start_child(store, env, epic, id, out)?;
                    if let Some(left) = state.room.as_mut() {
                        *left = left.saturating_sub(1);
                    }
                }
                return Ok(());
            }
        }
    }
    Ok(())
}

fn skip_once(store: &Store, env: &Env<'_>, epic: i64, id: i64) -> Result<()> {
    if store.item_event_logged(EVENT_CHILD_SKIPPED, id)? {
        return Ok(());
    }
    let project = store.backlog_item(id)?.draft.project;
    store.append_event(
        env.actor,
        None,
        None,
        EVENT_CHILD_SKIPPED,
        &serde_json::json!({ "item": id, "epic": epic, "project": project }),
    )?;
    Ok(())
}

/// Adds the re-plan child for `trigger`; false when the store found an explore child already open.
fn add_replan(
    store: &mut Store,
    env: &Env<'_>,
    epic: &BacklogItem,
    trigger: &ReplanTrigger,
) -> Result<bool> {
    let plan = plan_path(env, epic)?;
    let brief = replan_brief(&ReplanBrief {
        epic: epic.id,
        epic_text: &epic.draft.text,
        project: &epic.draft.project,
        trigger,
        plan_path: &plan,
    });
    let draft = BacklogDraft {
        project: epic.draft.project.clone(),
        section: epic.draft.section.clone(),
        text: trigger.child_text(),
        notes: Vec::new(),
        status: BacklogStatus::Backlog,
        owner: BacklogOwner::Agent,
        added: Some(store.backlog_today()?),
        not_before: None,
        due: None,
        done_on: None,
        done_note: None,
    };
    let shape = ItemShape {
        kind: None,
        parent: Some(epic.id),
        after: Vec::new(),
        plan: ItemPlan {
            brief: Some(brief),
            task_kind: Some(EXPLORE_KIND.to_owned()),
            model: None,
            review: None,
        },
    };
    let causes: Vec<ReplanRow> = trigger
        .causes()
        .iter()
        .map(|cause| ReplanRow {
            item: cause.child,
            event: cause.event,
        })
        .collect();
    let added = store.add_epic_replan(
        &NewReplan {
            epic: epic.id,
            draft: &draft,
            shape: &shape,
            reason: &trigger.reason(),
            causes: &causes,
        },
        env.actor,
    )?;
    if added.is_some() {
        drop(export_after(store, env));
    }
    Ok(added.is_some())
}

/// Claims child `id`, spawns its task and links it; a failed spawn, or a task already failed, blocks the child
/// `task_lost` instead.
fn start_child(
    store: &mut Store,
    env: &Env<'_>,
    epic: &BacklogItem,
    id: i64,
    out: &mut Vec<Pumped>,
) -> Result<()> {
    let claim = store.claim_epic_child(id, env.config.epics.max_parallel, env.actor)?;
    let EpicClaim::Claimed(child) = claim else {
        return Ok(());
    };
    match spawn_child(store, env, epic, &child) {
        Ok((task, pumped)) => {
            out.extend(pumped);
            if task.state == TaskState::Failed {
                lost(
                    store,
                    env,
                    id,
                    &format!("task {} failed to start", task.name),
                )?;
            } else {
                let link = BacklogMove {
                    link: Some(TaskId(task.id.0)),
                    ..BacklogMove::new(Target::InProgress, BacklogActor::Headwork, env.actor, true)
                };
                store.move_backlog_item(id, &link)?;
            }
        }
        Err(error) => lost(store, env, id, &format!("spawn failed: {error:#}"))?,
    }
    drop(export_after(store, env));
    Ok(())
}

/// Blocks a claimed child `task_lost` with `note` cut to one line the status tag can hold.
fn lost(store: &mut Store, env: &Env<'_>, id: i64, note: &str) -> Result<()> {
    let flat = note
        .replace(['\n', '\r', '[', ']'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let note: String = flat.chars().take(NOTE_CHARS).collect();
    let block = BacklogMove::new(
        Target::Blocked {
            reason: BlockReason::TaskLost,
            note: Some(note),
        },
        BacklogActor::Headwork,
        env.actor,
        true,
    );
    store.move_backlog_item(id, &block)?;
    Ok(())
}

fn spawn_child(
    store: &mut Store,
    env: &Env<'_>,
    epic: &BacklogItem,
    child: &BacklogItem,
) -> Result<(Task, Vec<Pumped>)> {
    let plan = store.backlog_plan(child.id)?;
    let project = ProjectName::new(child.draft.project.clone())
        .ok()
        .filter(|project| env.config.project(project).is_some())
        .ok_or_else(|| anyhow!("`{}` is not a project headwork runs", child.draft.project))?;
    let kind = plan
        .task_kind
        .clone()
        .unwrap_or_else(|| DEFAULT_CHILD_KIND.to_owned());
    let explore = kind == EXPLORE_KIND;
    let model = plan
        .model
        .as_deref()
        .map(str::parse::<Model>)
        .transpose()
        .map_err(|error| anyhow!("the planned model: {error}"))?;
    let review = plan
        .review
        .map(review_from_number)
        .transpose()
        .map_err(|level| anyhow!("the planned review level {level} is not 0 to 3"))?
        .flatten();
    let decision = plan.review.map(|_| FlagDecision {
        decided_by: DecidedBy::Judged,
        reason: PLANNED_REVIEW.to_owned(),
    });
    let brief = plan
        .brief
        .filter(|brief| !brief.trim().is_empty())
        .unwrap_or_else(|| {
            child_brief(&ChildBrief {
                id: child.id,
                text: &child.draft.text,
                notes: &child.draft.notes,
                epic: epic.id,
                epic_text: &epic.draft.text,
                project: project.as_str(),
            })
        });
    let brief = if explore {
        with_writes_line(&brief, &plan_path(env, epic)?)
    } else {
        brief
    };
    let in_main = env
        .config
        .project(&project)
        .is_some_and(|config| config.is_main_only(&kind));
    let slug = free_slug(
        store,
        &project,
        &child_slug(epic.id, child.id, &child.draft.text),
    )?;
    let incoming = env.home.root().join("incoming");
    std::fs::create_dir_all(&incoming)?;
    let file = write_new(&incoming, &format!("{project}-{slug}"), &brief)?;
    let request = SpawnRequest {
        request: TaskRequest {
            project,
            slug,
            close: CloseMode::default_for(&kind),
            kind,
            model,
            effort: None,
            in_main,
            keep: false,
            review,
            decision,
        },
        brief: &file,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
        epic: explore.then_some(epic.id),
    };
    let spawned = spawn(store, env, &request);
    std::fs::remove_file(&file).ok();
    spawned
}
