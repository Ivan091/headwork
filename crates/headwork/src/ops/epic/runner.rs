//! The epic runner (`docs/backlog.md`, section 13; `docs/decisions/2026-11-28-epic-runner.md`): after every queue pump it
//! starts the ready children of each epic in progress, re-plans an epic whose children are lost or all finished, and
//! blocks an epic that re-planned too often.
//!
//! Each start is a claim ([`Store::claim_epic_child`]), the ordinary [`spawn`], then the link; the pure decision is
//! [`epic_step`]. The runner is best effort: an error is logged as [`EVENT_RUNNER_ERROR`] and never fails the pump.

use anyhow::{Result, anyhow, bail};
use headwork_core::backlog::{
    BacklogActor, BacklogDraft, BacklogItem, BacklogOwner, BacklogStatus, BlockReason, ChildBrief,
    EpicLimits, EpicStep, ItemKind, ItemStatus, ReplanBrief, ReplanTrigger, Target, child_brief,
    child_kind_refusal, child_slug, epic_step, replan_brief, with_writes_line,
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

/// The event of a runner step that failed (`error`, `epic` and `child` when they are known); the pump goes on.
///
/// The same error is not logged again until the epic has a pass without one ([`headwork_store::EVENT_EPIC_RUNNER_OK`]).
pub const EVENT_RUNNER_ERROR: &str = headwork_store::EVENT_EPIC_RUNNER_ERROR;

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
        log_error(store, env, None, None, &format!("{error:#}"), &mut false);
    }
    out
}

/// Logs `epic-runner-error` with the epic and the child it concerns, unless the same error was logged since the
/// epic's last error-free pass; sets `failed` either way.
fn log_error(
    store: &Store,
    env: &Env<'_>,
    epic: Option<i64>,
    child: Option<i64>,
    error: &str,
    failed: &mut bool,
) {
    *failed = true;
    if store
        .runner_error_logged(epic, child, error)
        .unwrap_or(false)
    {
        return;
    }
    drop(store.append_event(
        env.actor,
        None,
        None,
        EVENT_RUNNER_ERROR,
        &serde_json::json!({ "epic": epic, "child": child, "error": error }),
    ));
}

/// Why the runner does not start a child of `project` and task `kind` (its stored kind, `code` when unset) in an epic of
/// `epic_project`, if it does not: a project headwork does not run, one other than the epic's, or a kind
/// [`child_kind_refusal`] refuses.
#[must_use]
fn child_skip(
    config: &Config,
    epic_project: &str,
    project: &str,
    kind: Option<&str>,
) -> Option<String> {
    let Some(project_config) = ProjectName::new(project)
        .ok()
        .and_then(|name| config.project(&name))
    else {
        return Some(format!("project `{project}` is not one headwork runs"));
    };
    if project != epic_project {
        return Some(format!(
            "project `{project}` is not the epic's project `{epic_project}`"
        ));
    }
    let kind = kind.unwrap_or(DEFAULT_CHILD_KIND);
    child_kind_refusal(
        kind,
        &config.epics.kinds,
        project_config.is_main_only(kind),
        project_config
            .extra_writable
            .iter()
            .any(|entry| !entry.kinds.is_empty() && entry.applies_to(kind)),
    )
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
        let mut failed = false;
        if let Err(error) = run_epic(store, env, epic, &mut state, out, &mut failed) {
            log_error(
                store,
                env,
                Some(epic.id),
                None,
                &format!("{error:#}"),
                &mut failed,
            );
        }
        if !failed && store.runner_failing(epic.id)?.is_some() {
            store.append_event(
                env.actor,
                None,
                None,
                headwork_store::EVENT_EPIC_RUNNER_OK,
                &serde_json::json!({ "epic": epic.id }),
            )?;
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
    failed: &mut bool,
) -> Result<()> {
    let config = env.config;
    reconcile(store, env, epic.id, failed)?;
    for _ in 0..2 {
        let facts = store.epic_facts(epic.id, |project, kind| {
            child_skip(config, &epic.draft.project, project, kind)
        })?;
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
                for (id, why) in skipped {
                    skip_once(store, env, epic.id, id, &why)?;
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
                    if start_child(store, env, epic, id, out, failed)?
                        && let Some(left) = state.room.as_mut()
                    {
                        *left = left.saturating_sub(1);
                    }
                }
                return Ok(());
            }
        }
    }
    Ok(())
}

fn skip_once(store: &Store, env: &Env<'_>, epic: i64, id: i64, why: &str) -> Result<()> {
    if store.item_event_logged(EVENT_CHILD_SKIPPED, id)? {
        return Ok(());
    }
    let project = store.backlog_item(id)?.draft.project;
    store.append_event(
        env.actor,
        None,
        None,
        EVENT_CHILD_SKIPPED,
        &serde_json::json!({ "item": id, "epic": epic, "project": project, "reason": why }),
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

/// A spawn of a child that did not come back with its task.
#[derive(Debug)]
struct SpawnFailure {
    /// `<project>/<slug>` once the runner chose the task's name: a task row of that name may exist.
    name: Option<String>,
    /// What failed.
    error: anyhow::Error,
}

/// Claims child `id`, spawns its task and links it. Returns whether a task now holds the child.
///
/// A spawn that fails after its task row exists (the brief written, the queue pumped) links the child to that task, so
/// the task's lifecycle settles it; with no row, or a task already over, the child is blocked `task_lost`. The spawn
/// error is logged with the child either way, and a link or block that fails is logged with it and left to
/// [`reconcile`].
fn start_child(
    store: &mut Store,
    env: &Env<'_>,
    epic: &BacklogItem,
    id: i64,
    out: &mut Vec<Pumped>,
    failed: &mut bool,
) -> Result<bool> {
    let claim = store.claim_epic_child(id, env.config.epics.max_parallel, env.actor)?;
    let EpicClaim::Claimed(child) = claim else {
        return Ok(false);
    };
    let held = match spawn_child(store, env, epic, &child) {
        Ok((task, pumped)) => {
            out.extend(pumped);
            settle(store, env, (epic.id, id), Some(task), None, failed)
        }
        Err(failure) => {
            let task = failure
                .name
                .as_deref()
                .and_then(|name| store.find_task(name).ok().flatten());
            settle(
                store,
                env,
                (epic.id, id),
                task,
                Some(&failure.error),
                failed,
            )
        }
    };
    drop(export_after(store, env));
    Ok(held)
}

const fn is_over(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Closed | TaskState::Dropped | TaskState::Failed
    )
}

/// Links claimed child `id` to `task` while it is live, else blocks it `task_lost`; logs `spawn_error` and any failure
/// of the move with the child. Returns whether a task holds the child.
fn settle(
    store: &mut Store,
    env: &Env<'_>,
    (epic, id): (i64, i64),
    task: Option<Task>,
    spawn_error: Option<&anyhow::Error>,
    failed: &mut bool,
) -> bool {
    let spawn_text = spawn_error.map(|error| format!("{error:#}"));
    if let Some(text) = &spawn_text {
        log_error(
            store,
            env,
            Some(epic),
            Some(id),
            &format!("spawn failed: {text}"),
            failed,
        );
    }
    let (moved, held) = match task {
        Some(task) if !is_over(task.state) => (link(store, env, id, task.id), true),
        Some(task) => (
            lost(
                store,
                env,
                id,
                &format!("task {} failed to start", task.name),
            ),
            false,
        ),
        None => (
            lost(
                store,
                env,
                id,
                &format!(
                    "spawn failed: {}",
                    spawn_text.as_deref().unwrap_or("no task")
                ),
            ),
            false,
        ),
    };
    if let Err(error) = moved {
        let text = spawn_text.as_ref().map_or_else(
            || format!("{error:#}"),
            |spawn| format!("{error:#} (after the spawn failed: {spawn})"),
        );
        log_error(store, env, Some(epic), Some(id), &text, failed);
    }
    held
}

/// Blocks a claimed child `task_lost` with `note` cut to one line the status tag can hold; the whole error is in the
/// `epic-runner-error` event.
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

fn link(store: &mut Store, env: &Env<'_>, id: i64, task: TaskId) -> Result<()> {
    let link = BacklogMove {
        link: Some(task),
        ..BacklogMove::new(Target::InProgress, BacklogActor::Headwork, env.actor, true)
    };
    store.move_backlog_item(id, &link)?;
    Ok(())
}

/// The grace a claim gets before [`reconcile`] settles it: a spawn takes seconds, so a claim older than this was cut
/// off (a crash, a killed tick, a failed link).
const CLAIM_GRACE_S: u32 = 600;

/// The note of a claim [`reconcile`] found with no task.
const NEVER_LINKED: &str = "claimed but never linked";

/// Settles each child of `epic` the runner claimed over [`CLAIM_GRACE_S`] ago that no task holds: linked to the live
/// task named for it (`e<epic>-<child>`, `-<words>`, `-2`), else blocked `task_lost` as [`NEVER_LINKED`].
fn reconcile(store: &mut Store, env: &Env<'_>, epic: i64, failed: &mut bool) -> Result<()> {
    for id in store.stale_epic_claims(epic, CLAIM_GRACE_S)? {
        let project = store.backlog_item(id)?.draft.project;
        let task = store.live_task_with_slug_prefix(&project, &format!("e{epic}-{id}"))?;
        let moved = match task {
            Some(task) => link(store, env, id, task),
            None => lost(store, env, id, NEVER_LINKED),
        };
        if let Err(error) = moved {
            log_error(
                store,
                env,
                Some(epic),
                Some(id),
                &format!("{error:#}"),
                failed,
            );
        }
    }
    Ok(())
}

/// What a child's spawn needs: the request's parts, its brief and its task's name.
#[derive(Debug)]
struct ChildSpawn {
    request: TaskRequest,
    brief: String,
    explore: bool,
}

fn spawn_child(
    store: &mut Store,
    env: &Env<'_>,
    epic: &BacklogItem,
    child: &BacklogItem,
) -> Result<(Task, Vec<Pumped>), SpawnFailure> {
    let unnamed = |error| SpawnFailure { name: None, error };
    let prepared = prepare(store, env, epic, child).map_err(unnamed)?;
    let name = format!("{}/{}", prepared.request.project, prepared.request.slug);
    let named = |error| SpawnFailure {
        name: Some(name.clone()),
        error,
    };
    let incoming = env.home.root().join("incoming");
    std::fs::create_dir_all(&incoming)
        .map_err(anyhow::Error::from)
        .map_err(unnamed)?;
    let stem = format!("{}-{}", prepared.request.project, prepared.request.slug);
    let file = write_new(&incoming, &stem, &prepared.brief).map_err(unnamed)?;
    let request = SpawnRequest {
        request: prepared.request,
        brief: &file,
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        allow_ask: &[],
        epic: prepared.explore.then_some(epic.id),
    };
    let spawned = spawn(store, env, &request);
    std::fs::remove_file(&file).ok();
    spawned.map_err(named)
}

fn prepare(
    store: &Store,
    env: &Env<'_>,
    epic: &BacklogItem,
    child: &BacklogItem,
) -> Result<ChildSpawn> {
    let plan = store.backlog_plan(child.id)?;
    if let Some(why) = child_skip(
        env.config,
        &epic.draft.project,
        &child.draft.project,
        plan.task_kind.as_deref(),
    ) {
        bail!("the runner does not start #{}: {why}", child.id);
    }
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
        let (roots, places) = crate::ops::explore::report_scope(env);
        with_writes_line(&brief, &plan_path(env, epic)?, &roots, &places)
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
    Ok(ChildSpawn {
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
        brief,
        explore,
    })
}
