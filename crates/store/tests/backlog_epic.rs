//! The epic explorer in the store (`docs/backlog.md`, section 12): the guard facts a move of an explorer gathers, the epic
//! a task explores, the plan stored for a child, and what the end of the explorer's task does to its epic.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::backlog::{
    BacklogActor, BacklogDraft, BacklogItem, BacklogStatus, Block, BlockReason, ItemKind, ReturnTo,
    Target, parse,
};
use headwork_core::lifecycle::{CleanupFacts, WorktreeEnd};
use headwork_core::{
    Actor, Config, PermissionMode, ProjectName, Slug, TaskRequest, TaskState, Trigger,
};
use headwork_store::{
    BacklogEdit, BacklogMove, EVENT_BACKLOG_MOVED, EventFilter, ItemPlan, ItemShape, NewTask,
    Store, StoreError, Task, TaskId,
};

fn store() -> (Store, Config) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    let config = Config::parse(&std::fs::read_to_string(path).unwrap(), "config.toml").unwrap();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_projects(&config).unwrap();
    (store, config)
}

fn draft(text: &str) -> BacklogDraft {
    let mut draft = parse(&format!("## me-sim\n\n- [ ] {text}\n"))
        .items
        .remove(0);
    draft.status = BacklogStatus::Backlog;
    draft
}

fn shaped(store: &mut Store, text: &str, shape: &ItemShape) -> BacklogItem {
    store
        .add_backlog_item_shaped(
            &draft(text),
            Actor::Orchestrator,
            BacklogActor::Orchestrator,
            None,
            shape,
        )
        .unwrap()
}

fn epic(store: &mut Store, text: &str) -> i64 {
    shaped(
        store,
        text,
        &ItemShape {
            kind: Some(ItemKind::Epic),
            ..ItemShape::default()
        },
    )
    .id
}

fn child(store: &mut Store, epic: i64, text: &str) -> i64 {
    shaped(
        store,
        text,
        &ItemShape {
            parent: Some(epic),
            ..ItemShape::default()
        },
    )
    .id
}

fn task(store: &mut Store, config: &Config, slug: &str) -> Task {
    store
        .create_task(
            config,
            &NewTask {
                request: TaskRequest {
                    project: ProjectName::new("me-sim").unwrap(),
                    slug: Slug::new(slug).unwrap(),
                    kind: "explore".to_owned(),
                    model: None,
                    effort: None,
                    in_main: false,
                    keep: false,
                    close: headwork_core::CloseMode::Silent,
                    review: None,
                    decision: None,
                },
                brief_path: format!("briefs/me-sim-{slug}.md"),
                depends_on: None,
                permission_mode: PermissionMode::Auto,
                shape: headwork_core::TaskShape::Plain,
            },
            Actor::Orchestrator,
        )
        .unwrap()
}

fn link(store: &mut Store, id: i64, task: TaskId) {
    store
        .move_backlog_item(
            id,
            &BacklogMove {
                link: Some(task),
                ..BacklogMove::new(Target::InProgress, BacklogActor::User, Actor::User, true)
            },
        )
        .unwrap();
}

fn by_explorer(
    store: &mut Store,
    explorer: &Task,
    epic: i64,
    id: i64,
    target: Target,
) -> Result<BacklogItem, StoreError> {
    store.move_backlog_item(
        id,
        &BacklogMove {
            actor_task: Some(explorer.id),
            actor_epic: Some(epic),
            ..BacklogMove::new(target, BacklogActor::Explorer, Actor::Worker, true)
                .with_reason(Some("planned"))
        },
    )
}

fn close(store: &mut Store, task: &Task, first: &[Trigger]) {
    for trigger in first {
        store
            .transition(task.id, trigger, Actor::Orchestrator)
            .unwrap();
    }
    store
        .transition(
            task.id,
            &Trigger::Cleaned(CleanupFacts {
                pane_gone: true,
                processes_gone: true,
                worktree: WorktreeEnd::Removed,
            }),
            Actor::Tick,
        )
        .unwrap();
}

fn refusal(result: Result<BacklogItem, StoreError>) -> String {
    match result {
        Err(StoreError::BacklogRefused { reason, .. }) => reason,
        other => format!("not a refusal: {other:?}"),
    }
}

fn moves(store: &Store) -> Vec<(Option<i64>, serde_json::Value)> {
    store
        .events(EventFilter {
            task: None,
            limit: 200,
        })
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == EVENT_BACKLOG_MOVED)
        .map(|event| (event.task_id.map(|id| id.0), event.data))
        .collect()
}

#[test]
fn the_epic_a_task_explores_is_recorded_on_the_task_and_survives_the_epic_s_moves() {
    let (mut store, config) = store();
    let big = epic(&mut store, "Big epic");
    let replan = child(&mut store, big, "Re-plan the epic");
    let explorer = task(&mut store, &config, "epic-explorer");
    let replanner = task(&mut store, &config, "replanner");
    assert_eq!(
        store.task(explorer.id).unwrap().epic,
        None,
        "nothing recorded yet"
    );
    store.set_task_epic(explorer.id, big, Actor::User).unwrap();
    link(&mut store, replan, replanner.id);
    assert_eq!(store.task(explorer.id).unwrap().epic, Some(big));
    assert_eq!(
        store.task(replanner.id).unwrap().epic,
        None,
        "a link to a child records nothing: the role comes only from the spawn"
    );
    let refused = store.set_task_epic(replanner.id, replan, Actor::User);
    assert!(
        matches!(&refused, Err(StoreError::BacklogRefused { reason, .. }) if reason.contains("not an epic")),
        "{refused:?}"
    );
    link(&mut store, big, explorer.id);
    close(&mut store, &explorer, &[Trigger::ForceClose]);
    assert_eq!(
        store.task(explorer.id).unwrap().epic,
        Some(big),
        "the role stays after the epic is released"
    );
}

#[test]
fn an_explorer_moves_only_its_epic_s_children_and_finishes_the_epic_once_none_is_open() {
    let (mut store, config) = store();
    let big = epic(&mut store, "Big epic");
    let first = child(&mut store, big, "First child");
    let second = child(&mut store, big, "Second child");
    let other_epic = epic(&mut store, "Other epic");
    let stranger = child(&mut store, other_epic, "Stranger");
    let explorer = task(&mut store, &config, "explorer");
    link(&mut store, big, explorer.id);
    let outside = by_explorer(&mut store, &explorer, big, stranger, Target::Cancelled);
    assert!(
        refusal(outside).contains("only its own epic's children"),
        "a child of another epic"
    );
    let early = by_explorer(&mut store, &explorer, big, big, Target::Done);
    assert_eq!(
        refusal(early),
        "the epic has 2 open children: finish or cancel them first"
    );
    by_explorer(&mut store, &explorer, big, first, Target::Cancelled).unwrap();
    let waits = store
        .add_backlog_dependency(second, stranger, Actor::Worker, Some(explorer.id))
        .unwrap();
    assert_eq!(
        waits.draft.status,
        BacklogStatus::Blocked(Block {
            reason: BlockReason::Depends,
            note: Some("#".to_owned() + &stranger.to_string()),
            before: ReturnTo::Backlog,
        }),
        "a child may wait for an item outside its epic"
    );
    let still = by_explorer(&mut store, &explorer, big, big, Target::Done);
    assert_eq!(
        refusal(still),
        "the epic has 1 open children: finish or cancel them first"
    );
    by_explorer(&mut store, &explorer, big, second, Target::Cancelled).unwrap();
    let cancel_epic = by_explorer(&mut store, &explorer, big, big, Target::Cancelled);
    assert!(
        refusal(cancel_epic).contains("may not move"),
        "the explorer only finishes its epic"
    );
    let done = by_explorer(&mut store, &explorer, big, big, Target::Done).unwrap();
    assert_eq!(
        (
            done.draft.status,
            done.task,
            done.draft.done_note.as_deref()
        ),
        (BacklogStatus::Done, None, Some("planned")),
        "done, and the link to the explorer ended"
    );
    let logged: Vec<Option<i64>> = moves(&store)
        .into_iter()
        .filter(|(_, data)| data["actor"] == "explorer")
        .map(|(task, _)| task)
        .collect();
    assert!(
        !logged.is_empty() && logged.iter().all(|task| *task == Some(explorer.id.0)),
        "every explorer move is logged with its task: {logged:?}"
    );
}

#[test]
fn the_end_of_the_explorer_s_task_leaves_its_epic_in_progress_and_unlinked() {
    let (mut store, config) = store();
    let big = epic(&mut store, "Big epic");
    let explorer = task(&mut store, &config, "explorer");
    link(&mut store, big, explorer.id);
    close(&mut store, &explorer, &[Trigger::ForceClose]);
    assert_eq!(store.task(explorer.id).unwrap().state, TaskState::Closed);
    let item = store.backlog_item(big).unwrap();
    assert_eq!(
        (item.draft.status, item.task, item.draft.done_on),
        (BacklogStatus::InProgress, None, None)
    );
    let (logged_task, last) = moves(&store).into_iter().last().unwrap();
    assert_eq!(
        (
            logged_task,
            last["from"].as_str(),
            last["to"].as_str(),
            last["kind"].as_str(),
            last["actor"].as_str()
        ),
        (
            Some(explorer.id.0),
            Some("in_progress"),
            Some("in_progress"),
            Some("unlink"),
            Some("headwork")
        )
    );
    let again = task(&mut store, &config, "explorer-two");
    link(&mut store, big, again.id);
    assert_eq!(
        store.backlog_item(big).unwrap().task.map(|task| task.id),
        Some(again.id.0),
        "a later explorer relinks the epic"
    );
    store.drop_task(again.id, Actor::Orchestrator).unwrap();
    assert_eq!(
        store.backlog_item(big).unwrap().draft.status,
        BacklogStatus::Blocked(Block {
            reason: BlockReason::TaskLost,
            note: Some("task me-sim-explorer-two dropped".to_owned()),
            before: ReturnTo::InProgress,
        }),
        "a dropped explorer blocks its epic as task_lost"
    );
}

#[test]
fn a_partial_outcome_never_blocks_the_epic() {
    let (mut store, config) = store();
    let big = epic(&mut store, "Big epic");
    let plain = store
        .add_backlog_item(&draft("Plain item"), Actor::Orchestrator)
        .unwrap()
        .id;
    let explorer = task(&mut store, &config, "explorer");
    link(&mut store, big, explorer.id);
    link(&mut store, plain, explorer.id);
    let blocked = store
        .record_partial_outcome(&explorer, "session", Some("half"), Actor::Worker)
        .unwrap();
    assert_eq!(blocked, [plain]);
    assert_eq!(
        store.backlog_item(big).unwrap().draft.status,
        BacklogStatus::InProgress
    );
}

#[test]
fn a_child_s_plan_is_stored_at_add_and_changed_field_by_field() {
    let (mut store, _) = store();
    let big = epic(&mut store, "Big epic");
    let planned = shaped(
        &mut store,
        "Planned child",
        &ItemShape {
            parent: Some(big),
            plan: ItemPlan {
                brief: Some("Goal: do it".to_owned()),
                task_kind: Some("code".to_owned()),
                model: Some("sonnet".to_owned()),
                review: Some(2),
            },
            ..ItemShape::default()
        },
    );
    assert_eq!(
        store.backlog_plan(planned.id).unwrap(),
        ItemPlan {
            brief: Some("Goal: do it".to_owned()),
            task_kind: Some("code".to_owned()),
            model: Some("sonnet".to_owned()),
            review: Some(2),
        }
    );
    store
        .edit_backlog_item_for(
            planned.id,
            &BacklogEdit {
                plan: ItemPlan {
                    model: Some("opus".to_owned()),
                    review: Some(0),
                    ..ItemPlan::default()
                },
                ..BacklogEdit::default()
            },
            Actor::Worker,
            None,
        )
        .unwrap();
    let plan = store.backlog_plan(planned.id).unwrap();
    assert_eq!(
        (plan.brief.as_deref(), plan.model.as_deref(), plan.review),
        (Some("Goal: do it"), Some("opus"), Some(0))
    );
    assert_eq!(store.backlog_open_children(big).unwrap(), [planned.id]);
    assert_eq!(store.backlog_plan(planned.id + 100).ok(), None);
}
