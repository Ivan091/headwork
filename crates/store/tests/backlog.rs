//! The backlog list in the store: adding, moving through the status guard, importing by text, the link to a task and what
//! its end does.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::backlog::{
    BacklogActor, BacklogDraft, BacklogItem, BacklogOwner, BacklogStatus, BlockReason, ItemKind,
    ItemStatus, ReturnTo, Target, parse,
};
use headwork_core::lifecycle::{CleanupFacts, MergePolicy, MergeSource, SessionFacts, WorktreeEnd};
use headwork_core::{
    Actor, Config, PermissionMode, ProjectName, Slug, TaskRequest, TaskState, Trigger,
};
use headwork_store::{
    BacklogEdit, BacklogFilter, BacklogMove, EVENT_BACKLOG_ADDED, EVENT_BACKLOG_EXPORTED,
    EVENT_BACKLOG_IMPORTED, EVENT_BACKLOG_MOVED, EVENT_BACKLOG_PICKED, EventFilter, ItemShape,
    NewTask, PrSeen, Store, StoreError, Task, TaskId,
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

fn by_user(
    store: &mut Store,
    id: i64,
    target: Target,
    reason: Option<&str>,
) -> Result<BacklogItem, StoreError> {
    store.move_backlog_item(
        id,
        &BacklogMove::new(target, BacklogActor::User, Actor::User, true).with_reason(reason),
    )
}

fn link(store: &mut Store, id: i64, task: TaskId) -> Result<BacklogItem, StoreError> {
    store.move_backlog_item(
        id,
        &BacklogMove {
            link: Some(task),
            ..BacklogMove::new(
                Target::InProgress,
                BacklogActor::Orchestrator,
                Actor::Orchestrator,
                true,
            )
        },
    )
}

fn moves(store: &Store) -> Vec<serde_json::Value> {
    store
        .events(EventFilter {
            task: None,
            limit: 200,
        })
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == EVENT_BACKLOG_MOVED)
        .map(|event| event.data)
        .collect()
}

fn lost(note: &str) -> BacklogStatus {
    BacklogStatus::Blocked(headwork_core::backlog::Block {
        reason: BlockReason::TaskLost,
        note: Some(note.to_owned()),
        before: ReturnTo::InProgress,
    })
}

fn task(store: &mut Store, config: &Config, slug: &str) -> Task {
    store
        .create_task(
            config,
            &NewTask {
                request: TaskRequest {
                    project: ProjectName::new("me-sim").unwrap(),
                    slug: Slug::new(slug).unwrap(),
                    kind: "scene".to_owned(),
                    model: None,
                    effort: None,
                    in_main: false,
                    keep: false,
                    close: headwork_core::CloseMode::Confirm,
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

fn kinds(store: &Store) -> Vec<String> {
    store
        .events(EventFilter {
            task: None,
            limit: 200,
        })
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .filter(|kind| kind.starts_with("backlog-"))
        .collect()
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

#[test]
fn an_added_item_goes_last_and_is_listed_by_filter() {
    let (mut store, _) = store();
    let first = store
        .add_backlog_item(&draft("one"), Actor::Orchestrator)
        .unwrap();
    let mut mine = draft("For you: two");
    mine.owner = BacklogOwner::User;
    mine.project = "headwork".to_owned();
    let second = store.add_backlog_item(&mine, Actor::Orchestrator).unwrap();
    assert!(second.position > first.position, "new items go last");
    let all = store.backlog_items(&BacklogFilter::default()).unwrap();
    assert_eq!(
        all.iter().map(|t| t.id).collect::<Vec<_>>(),
        [first.id, second.id]
    );
    let user = store
        .backlog_items(&BacklogFilter {
            owner: Some(BacklogOwner::User),
            ..BacklogFilter::default()
        })
        .unwrap();
    assert_eq!(user.len(), 1, "owner filter");
    let project = store
        .backlog_items(&BacklogFilter {
            project: Some("me-sim".to_owned()),
            ..BacklogFilter::default()
        })
        .unwrap();
    assert_eq!(project.len(), 1, "project filter");
    assert!(
        kinds(&store).contains(&EVENT_BACKLOG_ADDED.to_owned()),
        "event"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one walk through every command move, checked against its event"
)]
fn finishing_cancelling_and_reopening_go_through_the_guard() {
    let (mut store, _) = store();
    let one = store.add_backlog_item(&draft("one"), Actor::User).unwrap();
    let done = by_user(&mut store, one.id, Target::Done, Some("by hand")).unwrap();
    assert_eq!(done.draft.status, BacklogStatus::Done);
    assert_eq!(done.draft.done_note.as_deref(), Some("by hand"));
    assert_eq!(done.draft.done_on, Some(store.backlog_today().unwrap()));
    let again = by_user(&mut store, one.id, Target::Done, None);
    assert!(
        matches!(&again, Err(StoreError::BacklogRefused { reason, .. }) if reason == "it is already done"),
        "{again:?}"
    );
    let cancel_done = by_user(&mut store, one.id, Target::Cancelled, None);
    assert!(
        matches!(&cancel_done, Err(StoreError::BacklogRefused { reason, .. }) if reason.contains("never cancelled")),
        "{cancel_done:?}"
    );
    let reopened = by_user(&mut store, one.id, Target::Backlog, None).unwrap();
    assert_eq!(
        (
            reopened.draft.status,
            reopened.draft.done_on,
            reopened.draft.done_note
        ),
        (BacklogStatus::Backlog, None, None)
    );
    let started = by_user(&mut store, one.id, Target::InProgress, None).unwrap();
    assert_eq!(
        (started.draft.status, started.task),
        (BacklogStatus::InProgress, None)
    );
    let blocked = by_user(
        &mut store,
        one.id,
        Target::Blocked {
            reason: BlockReason::Event,
            note: Some("the release".to_owned()),
        },
        None,
    )
    .unwrap();
    assert_eq!(
        blocked.draft.status,
        BacklogStatus::Blocked(headwork_core::backlog::Block {
            reason: BlockReason::Event,
            note: Some("the release".to_owned()),
            before: ReturnTo::InProgress,
        })
    );
    let unblocked = by_user(&mut store, one.id, Target::Unblock, None).unwrap();
    assert_eq!(unblocked.draft.status, BacklogStatus::InProgress);
    let orchestrator = store.move_backlog_item(
        one.id,
        &BacklogMove::new(
            Target::Cancelled,
            BacklogActor::Orchestrator,
            Actor::Orchestrator,
            true,
        ),
    );
    assert!(
        matches!(&orchestrator, Err(StoreError::BacklogRefused { reason, .. }) if reason.contains("with a reason")),
        "{orchestrator:?}"
    );
    let cancelled = store
        .move_backlog_item(
            one.id,
            &BacklogMove::new(
                Target::Cancelled,
                BacklogActor::Orchestrator,
                Actor::Orchestrator,
                true,
            )
            .with_reason(Some("the user said so")),
        )
        .unwrap();
    assert_eq!(cancelled.draft.status, BacklogStatus::Cancelled);
    assert_eq!(
        cancelled.draft.done_note.as_deref(),
        Some("the user said so")
    );
    let logged: Vec<(String, String, String)> = moves(&store)
        .iter()
        .map(|data| {
            (
                data["from"].as_str().unwrap_or_default().to_owned(),
                data["to"].as_str().unwrap_or_default().to_owned(),
                data["actor"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let pair =
        |from: &str, to: &str, actor: &str| (from.to_owned(), to.to_owned(), actor.to_owned());
    assert_eq!(
        logged,
        [
            pair("backlog", "done", "user"),
            pair("done", "backlog", "user"),
            pair("backlog", "in_progress", "user"),
            pair("in_progress", "blocked", "user"),
            pair("blocked", "in_progress", "user"),
            pair("in_progress", "cancelled", "orchestrator"),
        ],
        "each move logs backlog-moved, in order"
    );
    assert_eq!(
        moves(&store).last().map(|data| data["reason"].clone()),
        Some(serde_json::json!("the user said so"))
    );
    assert!(matches!(
        store.backlog_item(999),
        Err(StoreError::UnknownBacklogItem(999))
    ));
}

#[test]
fn a_status_filter_lists_only_those_statuses() {
    let (mut store, _) = store();
    let one = store.add_backlog_item(&draft("one"), Actor::User).unwrap();
    let two = store.add_backlog_item(&draft("two"), Actor::User).unwrap();
    by_user(&mut store, two.id, Target::Done, None).unwrap();
    let ids = |store: &Store, statuses: &[ItemStatus]| {
        store
            .backlog_items(&BacklogFilter {
                statuses: statuses.to_vec(),
                ..BacklogFilter::default()
            })
            .unwrap()
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&store, ItemStatus::OPEN), [one.id]);
    assert_eq!(ids(&store, &[ItemStatus::Done]), [two.id]);
    assert_eq!(ids(&store, &[]), [one.id, two.id]);
}

#[test]
fn an_edit_changes_only_what_it_names_and_appends_notes() {
    let (mut store, _) = store();
    let one = store.add_backlog_item(&draft("one"), Actor::User).unwrap();
    let edited = store
        .edit_backlog_item(
            one.id,
            &BacklogEdit {
                text: Some("uno".to_owned()),
                section: Some(Some("Audit".to_owned())),
                due: Some(Some("2026-12-24".parse().unwrap())),
                append: Some("a note".to_owned()),
                ..BacklogEdit::default()
            },
            Actor::User,
        )
        .unwrap();
    assert_eq!(edited.draft.text, "uno");
    assert_eq!(edited.draft.project, "me-sim", "project untouched");
    assert_eq!(edited.draft.section.as_deref(), Some("Audit"));
    assert_eq!(
        edited.draft.due.map(|d| d.to_string()).as_deref(),
        Some("2026-12-24")
    );
    assert_eq!(edited.draft.notes, ["a note"]);
    let cleared = store
        .edit_backlog_item(
            one.id,
            &BacklogEdit {
                due: Some(None),
                section: Some(None),
                ..BacklogEdit::default()
            },
            Actor::User,
        )
        .unwrap();
    assert_eq!((cleared.draft.due, cleared.draft.section), (None, None));
}

#[test]
fn import_counts_new_known_and_repeated_items_and_changes_nothing_else() {
    let (mut store, _) = store();
    let known = store
        .add_backlog_item(&draft("Already here"), Actor::User)
        .unwrap();
    by_user(&mut store, known.id, Target::Done, None).unwrap();
    let drafts = parse(
        "## p\n\n- [ ] Brand new\n- [ ] For you: Already here\n- [ ] Brand  new\n- [ ] Another\n",
    )
    .items;
    let plan = store.backlog_import_plan(&drafts).unwrap();
    assert_eq!((plan.added, plan.already, plan.duplicate), (2, 1, 1));
    assert_eq!(
        store
            .backlog_items(&BacklogFilter::default())
            .unwrap()
            .len(),
        1,
        "a plan writes nothing"
    );
    let counts = store
        .import_backlog_items("S:/x/backlog.md", &drafts, Actor::User)
        .unwrap();
    assert_eq!(counts, plan);
    let rows = store.backlog_items(&BacklogFilter::default()).unwrap();
    assert_eq!(
        rows.iter()
            .map(|t| t.draft.text.as_str())
            .collect::<Vec<_>>(),
        ["Already here", "Brand new", "Another"],
        "file order after the old rows"
    );
    assert_eq!(
        rows.first().map(|t| t.draft.status.clone()),
        Some(BacklogStatus::Done),
        "an old row is not changed"
    );
    let again = store
        .import_backlog_items("S:/x/backlog.md", &drafts, Actor::User)
        .unwrap();
    assert_eq!(
        (again.added, again.already, again.duplicate),
        (0, 3, 1),
        "idempotent"
    );
    assert!(
        store
            .backlog_file("S:/x/backlog.md")
            .unwrap()
            .unwrap()
            .imported_at
            .is_some()
    );
    assert!(kinds(&store).contains(&EVENT_BACKLOG_IMPORTED.to_owned()));
}

#[test]
fn the_export_record_keeps_the_last_hash() {
    let (mut store, _) = store();
    assert!(store.backlog_file("p").unwrap().is_none());
    store
        .record_backlog_export("p", "abc", true, Actor::Tick)
        .unwrap();
    let file = store.backlog_file("p").unwrap().unwrap();
    assert_eq!(
        (file.export_hash.as_deref(), file.imported_at),
        (Some("abc"), None)
    );
    store.import_backlog_items("p", &[], Actor::User).unwrap();
    let file = store.backlog_file("p").unwrap().unwrap();
    assert_eq!(
        file.export_hash.as_deref(),
        Some("abc"),
        "an import keeps the hash"
    );
    assert!(file.imported_at.is_some());
    assert!(kinds(&store).contains(&EVENT_BACKLOG_EXPORTED.to_owned()));
}

#[test]
fn a_linked_item_is_refused_to_a_second_live_task_and_blocked_when_the_first_drops() {
    let (mut store, config) = store();
    let item = store
        .add_backlog_item(&draft("Do it"), Actor::Orchestrator)
        .unwrap();
    let first = task(&mut store, &config, "first");
    let linked = link(&mut store, item.id, first.id).unwrap();
    assert_eq!(linked.draft.status, BacklogStatus::InProgress);
    assert_eq!(linked.task.map(|t| t.name), Some("me-sim-first".to_owned()));
    let second = task(&mut store, &config, "second");
    let refused = link(&mut store, item.id, second.id);
    assert!(
        matches!(&refused, Err(StoreError::BacklogRefused { reason, .. }) if reason == "task me-sim-first already works on it"),
        "{refused:?}"
    );
    store.drop_task(first.id, Actor::Orchestrator).unwrap();
    let blocked = store.backlog_item(item.id).unwrap();
    assert_eq!(
        (blocked.draft.status, blocked.task),
        (lost("task me-sim-first dropped"), None),
        "a dropped task blocks the item as task_lost and ends the link"
    );
    let respawned = link(&mut store, item.id, second.id).unwrap();
    assert_eq!(
        respawned.draft.status,
        BacklogStatus::InProgress,
        "a respawn"
    );
    close(&mut store, &first, &[Trigger::BeginClosing]);
    let still = store.backlog_item(item.id).unwrap();
    assert_eq!(
        still.draft.status,
        BacklogStatus::InProgress,
        "the first task closing changes nothing"
    );
    assert_eq!(still.task.map(|t| t.name), Some("me-sim-second".to_owned()));
}

fn merged_pr(store: &mut Store, worker: &Task, number: i64, state: &str) {
    store
        .record_pr(
            worker.id,
            &PrSeen {
                repo: "Ivan091/me-sim",
                number,
                url: &format!("https://github.com/Ivan091/me-sim/pull/{number}"),
                state,
            },
            Actor::Tick,
        )
        .unwrap();
}

#[test]
fn a_closed_task_finishes_its_item_with_its_name_and_merged_pull_requests() {
    let (mut store, config) = store();
    let item = store
        .add_backlog_item(&draft("Do it"), Actor::Orchestrator)
        .unwrap();
    let worker = task(&mut store, &config, "worker");
    link(&mut store, item.id, worker.id).unwrap();
    merged_pr(&mut store, &worker, 7, "merged");
    merged_pr(&mut store, &worker, 8, "closed");
    close(&mut store, &worker, &[Trigger::ForceClose]);
    assert_eq!(store.task(worker.id).unwrap().state, TaskState::Closed);
    let done = store.backlog_item(item.id).unwrap();
    assert_eq!(done.draft.status, BacklogStatus::Done);
    assert_eq!(done.draft.done_on, Some(store.backlog_today().unwrap()));
    assert_eq!(
        done.draft.done_note.as_deref(),
        Some("task me-sim-worker merged https://github.com/Ivan091/me-sim/pull/7")
    );
    let last = moves(&store).into_iter().last().unwrap();
    assert_eq!(
        (
            last["to"].as_str(),
            last["actor"].as_str(),
            last["item"].as_i64()
        ),
        (Some("done"), Some("headwork"), Some(item.id))
    );
}

#[test]
fn a_closed_task_with_no_pull_request_says_so() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "plain", "Do it");
    close(&mut store, &worker, &[Trigger::ForceClose]);
    let done = store.backlog_item(id).unwrap();
    assert_eq!(
        (
            done.draft.status,
            done.draft.done_note.as_deref(),
            done.task
        ),
        (
            BacklogStatus::Done,
            Some("task me-sim-plain, no pull request"),
            None
        )
    );
}

const MERGED: Trigger = Trigger::Merged {
    source: MergeSource::GitHost,
    policy: MergePolicy::Deliver,
};

fn to_merged(store: &mut Store, config: &Config, worker: &Task) {
    store.start_task(config, worker.id, Actor::Tick).unwrap();
    for trigger in [
        Trigger::SessionStarted(SessionFacts {
            session_matches: true,
            launcher_alive: true,
        }),
        Trigger::ReviewOpened { repo_allowed: true },
        MERGED,
    ] {
        store.transition(worker.id, &trigger, Actor::Tick).unwrap();
    }
    assert_eq!(
        store.task(worker.id).unwrap().state,
        TaskState::Merged,
        "merged"
    );
}

#[test]
fn a_merged_task_finishes_its_item_and_keeps_the_link_until_it_closes() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "merge", "Do it");
    merged_pr(&mut store, &worker, 3, "merged");
    to_merged(&mut store, &config, &worker);
    let done = store.backlog_item(id).unwrap();
    assert_eq!(
        (
            done.draft.status,
            done.draft.done_note.as_deref(),
            done.task.map(|task| task.name)
        ),
        (
            BacklogStatus::Done,
            Some("task me-sim-merge merged https://github.com/Ivan091/me-sim/pull/3"),
            Some("me-sim-merge".to_owned())
        ),
        "done at the merge, still linked"
    );
    close(&mut store, &worker, &[Trigger::ForceClose]);
    let closed = store.backlog_item(id).unwrap();
    assert_eq!(
        (closed.draft.status, closed.task),
        (BacklogStatus::Done, None),
        "the close ends the link"
    );
}

#[test]
fn a_merged_task_that_opens_another_pull_request_takes_its_item_back_and_the_close_names_every_merge()
 {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "twice", "Do it in two");
    merged_pr(&mut store, &worker, 3, "merged");
    to_merged(&mut store, &config, &worker);
    store
        .transition(
            worker.id,
            &Trigger::ReviewOpened { repo_allowed: true },
            Actor::Tick,
        )
        .unwrap();
    let back = store.backlog_item(id).unwrap();
    assert_eq!(
        (
            back.draft.status,
            back.draft.done_on,
            back.draft.done_note,
            back.task.map(|task| task.name)
        ),
        (
            BacklogStatus::InProgress,
            None,
            None,
            Some("me-sim-twice".to_owned())
        ),
        "in progress again, still linked, the done date cleared"
    );
    let last = moves(&store).into_iter().last().unwrap();
    assert_eq!(
        (
            last["from"].as_str(),
            last["to"].as_str(),
            last["actor"].as_str()
        ),
        (Some("done"), Some("in_progress"), Some("headwork"))
    );
    merged_pr(&mut store, &worker, 5, "merged");
    store.transition(worker.id, &MERGED, Actor::Tick).unwrap();
    assert_eq!(
        store.backlog_item(id).unwrap().draft.status,
        BacklogStatus::Done
    );
    close(&mut store, &worker, &[Trigger::ForceClose]);
    let closed = store.backlog_item(id).unwrap();
    assert_eq!(
        (closed.draft.done_note.as_deref(), closed.task),
        (
            Some(
                "task me-sim-twice merged https://github.com/Ivan091/me-sim/pull/3, https://github.com/Ivan091/me-sim/pull/5"
            ),
            None
        ),
        "the close rewrites the note with every merged pull request and ends the link"
    );
}

#[test]
fn a_blocked_item_loses_its_link_and_keeps_its_block_when_the_task_ends() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "held", "Wait for it");
    let blocked = by_user(
        &mut store,
        id,
        Target::Blocked {
            reason: BlockReason::Event,
            note: Some("the release".to_owned()),
        },
        None,
    )
    .unwrap();
    assert!(blocked.task.is_some(), "a block keeps the live link");
    store.drop_task(worker.id, Actor::Orchestrator).unwrap();
    let after = store.backlog_item(id).unwrap();
    assert_eq!(after.task, None, "the link ends");
    assert_eq!(after.draft.status, blocked.draft.status, "the block stays");
    let unblocked = by_user(&mut store, id, Target::Unblock, None).unwrap();
    assert_eq!(
        (unblocked.draft.status, unblocked.task),
        (BacklogStatus::InProgress, None)
    );
    let next = task(&mut store, &config, "next");
    let relinked = link(&mut store, id, next.id).unwrap();
    assert_eq!(
        relinked.task.map(|task| task.name),
        Some("me-sim-next".to_owned()),
        "spawn --backlog takes an in-progress item with no task"
    );
}

#[test]
fn every_item_linked_to_an_ending_task_moves() {
    let (mut store, config) = store();
    let worker = task(&mut store, &config, "both");
    let mut ids = Vec::new();
    for text in ["First", "Second"] {
        let item = store.add_backlog_item(&draft(text), Actor::User).unwrap();
        link(&mut store, item.id, worker.id).unwrap();
        ids.push(item.id);
    }
    let waiting = store
        .add_backlog_item(&draft("Third"), Actor::User)
        .unwrap();
    link(&mut store, waiting.id, worker.id).unwrap();
    by_user(
        &mut store,
        waiting.id,
        Target::Blocked {
            reason: BlockReason::Decision,
            note: None,
        },
        None,
    )
    .unwrap();
    close(&mut store, &worker, &[Trigger::ForceClose]);
    for id in ids {
        assert_eq!(
            store.backlog_item(id).unwrap().draft.status,
            BacklogStatus::Done,
            "item {id}"
        );
    }
    assert!(
        matches!(
            store.backlog_item(waiting.id).unwrap().draft.status,
            BacklogStatus::Blocked(_)
        ),
        "an item not in progress is left alone"
    );
}

#[test]
fn an_item_that_is_done_cannot_be_linked() {
    let (mut store, config) = store();
    let item = store
        .add_backlog_item(&draft("Done already"), Actor::User)
        .unwrap();
    by_user(&mut store, item.id, Target::Done, None).unwrap();
    let worker = task(&mut store, &config, "late");
    let refused = link(&mut store, item.id, worker.id);
    assert!(
        matches!(refused, Err(StoreError::BacklogRefused { .. })),
        "{refused:?}"
    );
}

fn linked(store: &mut Store, config: &Config, slug: &str, text: &str) -> (Task, i64) {
    let item = store
        .add_backlog_item(&draft(text), Actor::Orchestrator)
        .unwrap();
    let worker = task(store, config, slug);
    link(store, item.id, worker.id).unwrap();
    (worker, item.id)
}

#[test]
fn a_failed_task_blocks_its_item_as_task_lost() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "broken", "Do it");
    store.start_task(&config, worker.id, Actor::Tick).unwrap();
    store
        .transition(worker.id, &Trigger::LaunchFailed, Actor::Tick)
        .unwrap();
    assert_eq!(store.task(worker.id).unwrap().state, TaskState::Failed);
    let blocked = store.backlog_item(id).unwrap();
    assert_eq!(
        (blocked.draft.status, blocked.task),
        (lost("task me-sim-broken failed"), None)
    );
}

#[test]
fn a_hand_finish_or_cancel_of_an_item_a_live_task_works_on_is_refused() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "busy", "Busy");
    let finish = by_user(&mut store, id, Target::Done, Some("by hand"));
    assert!(
        matches!(&finish, Err(StoreError::BacklogRefused { reason, .. }) if reason.contains("only headwork finishes it")),
        "{finish:?}"
    );
    let cancel = by_user(&mut store, id, Target::Cancelled, None);
    assert!(
        matches!(&cancel, Err(StoreError::BacklogRefused { reason, .. }) if reason.contains("drop the task first")),
        "{cancel:?}"
    );
    store.drop_task(worker.id, Actor::Orchestrator).unwrap();
    let cancelled = by_user(&mut store, id, Target::Cancelled, Some("not needed")).unwrap();
    assert_eq!(
        (cancelled.draft.status, cancelled.draft.done_note.as_deref()),
        (BacklogStatus::Cancelled, Some("not needed"))
    );
    close(&mut store, &worker, &[Trigger::BeginClosing]);
    assert_eq!(
        store.backlog_item(id).unwrap().draft.status,
        BacklogStatus::Cancelled,
        "a cancelled item stays cancelled"
    );
}

#[test]
fn the_done_note_lists_every_merged_pull_request_in_order() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "two", "Do both");
    for number in [9, 4] {
        merged_pr(&mut store, &worker, number, "merged");
    }
    close(&mut store, &worker, &[Trigger::ForceClose]);
    let done = store.backlog_item(id).unwrap();
    assert_eq!(
        done.draft.done_note.as_deref(),
        Some(
            "task me-sim-two merged https://github.com/Ivan091/me-sim/pull/4, https://github.com/Ivan091/me-sim/pull/9"
        )
    );
    assert_eq!(done.task, None, "a closed task ends the link");
}

#[test]
fn an_item_that_cannot_be_written_to_the_file_is_refused() {
    type Change = fn(&mut BacklogDraft);
    let (mut store, _) = store();
    let cases: [(Change, &str); 6] = [
        (|d| d.text = "  ".to_owned(), "the text is empty"),
        (|d| d.text = "a\nb".to_owned(), "the text must be one line"),
        (
            |d| d.project = "# x".to_owned(),
            "the project must not start with `#`",
        ),
        (
            |d| d.project = "Done".to_owned(),
            "Done, Cancelled or Dropped",
        ),
        (
            |d| d.section = Some("#s".to_owned()),
            "the section must not start",
        ),
        (
            |d| d.notes = vec!["a\nb".to_owned()],
            "the note must be one line",
        ),
    ];
    for (change, message) in cases {
        let mut bad = draft("fine");
        change(&mut bad);
        let refused = store.add_backlog_item(&bad, Actor::User);
        assert!(
            matches!(&refused, Err(StoreError::InvalidBacklogItem(_))),
            "{refused:?}"
        );
        assert!(
            refused.unwrap_err().to_string().contains(message),
            "{message}"
        );
    }
    assert_eq!(
        store
            .backlog_items(&BacklogFilter::default())
            .unwrap()
            .len(),
        0
    );
    let one = store.add_backlog_item(&draft("one"), Actor::User).unwrap();
    let refused = store.edit_backlog_item(
        one.id,
        &BacklogEdit {
            project: Some("Dropped items".to_owned()),
            ..BacklogEdit::default()
        },
        Actor::User,
    );
    assert!(
        matches!(refused, Err(StoreError::InvalidBacklogItem(_))),
        "{refused:?}"
    );
    assert_eq!(store.backlog_item(one.id).unwrap().draft.project, "me-sim");
}

#[test]
fn pick_counts_are_the_newest_pick_and_the_adds_after_it() {
    let (mut store, config) = store();
    let task = task(&mut store, &config, "explore");
    let left = |store: &Store, id| {
        let (picked, added) = store.backlog_pick_counts(id).unwrap();
        headwork_core::backlog::adds_left(picked, added)
    };
    assert_eq!(left(&store, task.id), 0, "no pick yet");
    let pick = |store: &Store, count: u64| {
        store
            .append_event(
                Actor::Hook,
                Some(task.id),
                Some("s1"),
                EVENT_BACKLOG_PICKED,
                &serde_json::json!({ "count": count, "picks": [] }),
            )
            .unwrap();
    };
    store
        .add_backlog_item_for(
            &draft("before the pick"),
            Actor::Worker,
            BacklogActor::Explorer,
            Some(task.id),
        )
        .unwrap();
    pick(&store, 2);
    assert_eq!(left(&store, task.id), 2);
    store
        .add_backlog_item_for(
            &draft("first"),
            Actor::Worker,
            BacklogActor::Explorer,
            Some(task.id),
        )
        .unwrap();
    store
        .add_backlog_item(&draft("another task's"), Actor::Orchestrator)
        .unwrap();
    assert_eq!(left(&store, task.id), 1);
    store
        .add_backlog_item_for(
            &draft("second"),
            Actor::Worker,
            BacklogActor::Explorer,
            Some(task.id),
        )
        .unwrap();
    assert_eq!(left(&store, task.id), 0);
    pick(&store, 1);
    assert_eq!(left(&store, task.id), 1, "a later pick replaces the count");
    pick(&store, 0);
    assert_eq!(left(&store, task.id), 0);
}

#[test]
fn an_add_passes_the_guard_as_its_mover_and_must_be_a_backlog_item() {
    let (mut store, config) = store();
    let mut done = draft("Written as done");
    done.status = BacklogStatus::Done;
    let refused_done = store.add_backlog_item(&done, Actor::User);
    assert!(
        matches!(&refused_done, Err(StoreError::BacklogAddRefused(reason)) if reason.contains("not as done")),
        "{refused_done:?}"
    );
    let worker_as_worker =
        store.add_backlog_item_for(&draft("Worker"), Actor::Worker, BacklogActor::Worker, None);
    assert!(
        matches!(worker_as_worker, Err(StoreError::BacklogAddRefused(_))),
        "the mover is the one passed"
    );
    let refused = store.add_backlog_item(&draft("From a worker"), Actor::Worker);
    assert!(
        matches!(&refused, Err(StoreError::BacklogAddRefused(reason)) if reason.contains("worker")),
        "{refused:?}"
    );
    let explorer = task(&mut store, &config, "explore");
    assert!(
        store
            .add_backlog_item_for(
                &draft("A pick"),
                Actor::Worker,
                BacklogActor::Explorer,
                Some(explorer.id),
            )
            .is_ok(),
        "an exploration adds its picks"
    );
}

#[test]
fn a_note_or_reason_over_several_lines_is_refused() {
    let (mut store, _) = store();
    let one = store.add_backlog_item(&draft("one"), Actor::User).unwrap();
    for (target, reason) in [
        (Target::Done, Some("a\nb")),
        (Target::Cancelled, Some("a\r\nb")),
        (
            Target::Blocked {
                reason: BlockReason::Event,
                note: Some("a\nb".to_owned()),
            },
            None,
        ),
    ] {
        let refused = by_user(&mut store, one.id, target, reason);
        assert!(
            matches!(&refused, Err(StoreError::BacklogRefused { reason, .. }) if reason.contains("one line")),
            "{refused:?}"
        );
    }
    assert_eq!(
        store.backlog_item(one.id).unwrap().draft.status,
        BacklogStatus::Backlog
    );
}

#[test]
fn a_merged_item_stays_done_when_its_task_is_dropped_and_loses_the_link() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "dropped", "Merged then dropped");
    merged_pr(&mut store, &worker, 3, "merged");
    to_merged(&mut store, &config, &worker);
    store.drop_task(worker.id, Actor::Orchestrator).unwrap();
    let item = store.backlog_item(id).unwrap();
    assert_eq!(
        (
            item.draft.status,
            item.task,
            item.draft.done_note.as_deref()
        ),
        (
            BacklogStatus::Done,
            None,
            Some("task me-sim-dropped merged https://github.com/Ivan091/me-sim/pull/3")
        ),
        "the work merged, so it stays done and the link ends; a merged task never fails, failed comes only from spawning"
    );
}

#[test]
fn a_reopen_of_a_done_item_still_linked_to_its_merged_task_ends_the_link() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "again", "Do it again");
    merged_pr(&mut store, &worker, 3, "merged");
    to_merged(&mut store, &config, &worker);
    assert!(
        store.backlog_item(id).unwrap().task.is_some(),
        "linked at the merge"
    );
    let reopened = by_user(&mut store, id, Target::Backlog, None).unwrap();
    assert_eq!(
        (reopened.draft.status, reopened.task),
        (BacklogStatus::Backlog, None)
    );
}

#[test]
fn a_task_dropped_after_it_took_its_merged_item_back_finishes_the_item() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "back", "Merged, back, dropped");
    merged_pr(&mut store, &worker, 3, "merged");
    to_merged(&mut store, &config, &worker);
    store
        .transition(
            worker.id,
            &Trigger::ReviewOpened { repo_allowed: true },
            Actor::Tick,
        )
        .unwrap();
    assert_eq!(
        store.backlog_item(id).unwrap().draft.status,
        BacklogStatus::InProgress,
        "taken back"
    );
    store.drop_task(worker.id, Actor::Orchestrator).unwrap();
    let item = store.backlog_item(id).unwrap();
    assert_eq!(
        (
            item.draft.status,
            item.task,
            item.draft.done_note.as_deref()
        ),
        (
            BacklogStatus::Done,
            None,
            Some("task me-sim-back merged https://github.com/Ivan091/me-sim/pull/3")
        ),
        "the work merged, so the item is done, not task_lost"
    );
}

#[test]
fn an_unlink_at_a_task_end_is_a_logged_move() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "logged", "Merged then closed");
    merged_pr(&mut store, &worker, 3, "merged");
    to_merged(&mut store, &config, &worker);
    close(&mut store, &worker, &[Trigger::ForceClose]);
    let last = moves(&store).into_iter().last().unwrap();
    assert_eq!(
        (
            last["item"].as_i64(),
            last["from"].as_str(),
            last["to"].as_str(),
            last["kind"].as_str(),
            last["actor"].as_str()
        ),
        (
            Some(id),
            Some("done"),
            Some("done"),
            Some("unlink"),
            Some("headwork")
        )
    );
}

fn add(store: &mut Store, text: &str) -> i64 {
    store
        .add_backlog_item(&draft(text), Actor::Orchestrator)
        .unwrap()
        .id
}

fn depend(store: &mut Store, item: i64, dep: i64) -> Result<BacklogItem, StoreError> {
    store.add_backlog_dependency(item, dep, Actor::Orchestrator, None)
}

fn status_of(store: &Store, id: i64) -> BacklogStatus {
    store.backlog_item(id).unwrap().draft.status
}

fn blocked_for(reason: BlockReason, note: &str, before: ReturnTo) -> BacklogStatus {
    BacklogStatus::Blocked(headwork_core::backlog::Block {
        reason,
        note: Some(note.to_owned()),
        before,
    })
}

fn event_kinds(store: &Store) -> Vec<String> {
    store
        .events(EventFilter {
            task: None,
            limit: 200,
        })
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect()
}

fn shaped_add(store: &mut Store, text: &str, shape: &ItemShape) -> Result<BacklogItem, StoreError> {
    store.add_backlog_item_shaped(
        &draft(text),
        Actor::Orchestrator,
        BacklogActor::Orchestrator,
        None,
        shape,
    )
}

#[test]
fn a_dependency_blocks_an_item_in_the_backlog_and_done_frees_it() {
    let (mut store, _) = store();
    let (a, b) = (add(&mut store, "a"), add(&mut store, "b"));
    let item = depend(&mut store, b, a).unwrap();
    assert_eq!(item.deps, [a]);
    assert_eq!(
        item.draft.status,
        blocked_for(BlockReason::Depends, &format!("#{a}"), ReturnTo::Backlog)
    );
    by_user(&mut store, a, Target::Done, Some("shipped")).unwrap();
    assert_eq!(status_of(&store, b), BacklogStatus::Backlog);
    let kinds = event_kinds(&store);
    assert!(kinds.contains(&"backlog-depended".to_owned()), "{kinds:?}");
}

#[test]
fn an_item_waits_until_every_dependency_is_done() {
    let (mut store, _) = store();
    let (a, b, c) = (
        add(&mut store, "a"),
        add(&mut store, "b"),
        add(&mut store, "c"),
    );
    depend(&mut store, c, a).unwrap();
    depend(&mut store, c, b).unwrap();
    assert_eq!(
        status_of(&store, c),
        blocked_for(BlockReason::Depends, &format!("#{a}"), ReturnTo::Backlog),
        "the first block stays"
    );
    by_user(&mut store, a, Target::Done, None).unwrap();
    assert_eq!(
        status_of(&store, c).kind(),
        ItemStatus::Blocked,
        "b is still open"
    );
    by_user(&mut store, b, Target::Done, None).unwrap();
    assert_eq!(status_of(&store, c), BacklogStatus::Backlog);
}

#[test]
fn a_done_dependency_blocks_nothing_and_a_blocked_item_keeps_its_own_block() {
    let (mut store, _) = store();
    let (a, b, c) = (
        add(&mut store, "a"),
        add(&mut store, "b"),
        add(&mut store, "c"),
    );
    by_user(&mut store, a, Target::Done, None).unwrap();
    assert_eq!(
        depend(&mut store, b, a).unwrap().draft.status,
        BacklogStatus::Backlog
    );
    by_user(
        &mut store,
        c,
        Target::Blocked {
            reason: BlockReason::Decision,
            note: Some("pick".to_owned()),
        },
        None,
    )
    .unwrap();
    let d = add(&mut store, "d");
    depend(&mut store, c, d).unwrap();
    by_user(&mut store, d, Target::Done, None).unwrap();
    assert_eq!(
        status_of(&store, c),
        blocked_for(BlockReason::Decision, "pick", ReturnTo::Backlog),
        "a decision block is lifted by the user only"
    );
}

#[test]
fn a_cycle_a_self_dependency_and_an_item_in_progress_are_refused() {
    let (mut store, _) = store();
    let (a, b, c) = (
        add(&mut store, "a"),
        add(&mut store, "b"),
        add(&mut store, "c"),
    );
    depend(&mut store, b, a).unwrap();
    depend(&mut store, c, b).unwrap();
    let error = depend(&mut store, a, c).unwrap_err().to_string();
    assert!(error.contains("cycle"), "{error}");
    assert!(
        error.contains(&format!("#{a} -> #{c} -> #{b} -> #{a}")),
        "{error}"
    );
    assert!(
        depend(&mut store, a, a)
            .unwrap_err()
            .to_string()
            .contains("itself")
    );
    by_user(&mut store, a, Target::InProgress, None).unwrap();
    let d = add(&mut store, "d");
    let error = depend(&mut store, a, d).unwrap_err().to_string();
    assert!(error.contains("in_progress"), "{error}");
    assert!(
        store.backlog_item(a).unwrap().deps.is_empty(),
        "nothing was written"
    );
}

#[test]
fn a_done_or_cancelled_item_takes_no_dependency_and_a_cancelled_one_is_no_dependency() {
    let (mut store, _) = store();
    let (a, b, c) = (
        add(&mut store, "a"),
        add(&mut store, "b"),
        add(&mut store, "c"),
    );
    by_user(&mut store, a, Target::Done, None).unwrap();
    by_user(&mut store, b, Target::Cancelled, Some("no")).unwrap();
    assert!(depend(&mut store, a, c).is_err(), "done item");
    assert!(depend(&mut store, b, c).is_err(), "cancelled item");
    let error = depend(&mut store, c, b).unwrap_err().to_string();
    assert!(error.contains("cancelled"), "{error}");
}

#[test]
fn cancelling_a_dependency_blocks_a_backlog_dependant_and_swaps_a_depends_block() {
    let (mut store, _) = store();
    let (a, b) = (add(&mut store, "a"), add(&mut store, "b"));
    let child = shaped_add(
        &mut store,
        "child",
        &ItemShape {
            after: vec![a],
            ..ItemShape::default()
        },
    )
    .unwrap();
    assert_eq!(child.draft.status.kind(), ItemStatus::Blocked);
    let loose = add(&mut store, "loose");
    by_user(&mut store, loose, Target::InProgress, None).unwrap();
    store
        .add_backlog_dependency(b, a, Actor::User, None)
        .unwrap();
    by_user(&mut store, a, Target::Cancelled, Some("no")).unwrap();
    for id in [child.id, b] {
        assert_eq!(
            status_of(&store, id),
            blocked_for(
                BlockReason::DepCancelled,
                &format!("#{a} cancelled"),
                ReturnTo::Backlog
            ),
            "#{id}"
        );
    }
    assert_eq!(status_of(&store, loose), BacklogStatus::InProgress);
}

#[test]
fn unblocking_a_dependant_by_hand_blocks_it_again_while_a_dependency_is_open() {
    let (mut store, _) = store();
    let (a, b) = (add(&mut store, "a"), add(&mut store, "b"));
    depend(&mut store, b, a).unwrap();
    by_user(&mut store, b, Target::Unblock, None).unwrap();
    assert_eq!(
        status_of(&store, b),
        blocked_for(BlockReason::Depends, &format!("#{a}"), ReturnTo::Backlog)
    );
    by_user(&mut store, a, Target::Done, None).unwrap();
    assert_eq!(status_of(&store, b), BacklogStatus::Backlog);
}

#[test]
fn unblocking_a_decision_block_while_a_dependency_is_open_blocks_it_for_depends() {
    let (mut store, _) = store();
    let (a, b) = (add(&mut store, "a"), add(&mut store, "b"));
    by_user(
        &mut store,
        b,
        Target::Blocked {
            reason: BlockReason::Decision,
            note: Some("pick".to_owned()),
        },
        None,
    )
    .unwrap();
    depend(&mut store, b, a).unwrap();
    assert_eq!(
        status_of(&store, b).block().unwrap().reason,
        BlockReason::Decision
    );
    by_user(&mut store, b, Target::Unblock, None).unwrap();
    assert_eq!(
        status_of(&store, b),
        blocked_for(BlockReason::Depends, &format!("#{a}"), ReturnTo::Backlog)
    );
    assert!(by_user(&mut store, b, Target::InProgress, None).is_err());
}

#[test]
fn an_item_waiting_for_dependencies_cannot_be_started_by_anyone() {
    let (mut store, _) = store();
    let (a, b) = (add(&mut store, "a"), add(&mut store, "b"));
    depend(&mut store, b, a).unwrap();
    let request = BacklogMove::new(
        Target::InProgress,
        BacklogActor::Headwork,
        Actor::Tick,
        true,
    );
    assert!(store.move_backlog_item(b, &request).is_err());
    by_user(&mut store, a, Target::Done, None).unwrap();
    by_user(&mut store, b, Target::InProgress, None).unwrap();
}

#[test]
fn a_dependant_stays_gated_through_a_reopen_of_its_cancelled_dependency() {
    let (mut store, _) = store();
    let (a, b, c) = (
        add(&mut store, "a"),
        add(&mut store, "b"),
        add(&mut store, "c"),
    );
    depend(&mut store, c, a).unwrap();
    depend(&mut store, c, b).unwrap();
    by_user(&mut store, a, Target::Cancelled, Some("no")).unwrap();
    assert_eq!(
        status_of(&store, c),
        blocked_for(
            BlockReason::DepCancelled,
            &format!("#{a} cancelled"),
            ReturnTo::Backlog
        )
    );
    by_user(&mut store, a, Target::Backlog, None).unwrap();
    assert_eq!(
        status_of(&store, c),
        blocked_for(BlockReason::Depends, &format!("#{a}"), ReturnTo::Backlog),
        "reopened: open again"
    );
    by_user(&mut store, a, Target::Done, None).unwrap();
    assert_eq!(
        status_of(&store, c).block().unwrap().reason,
        BlockReason::Depends
    );
    by_user(&mut store, b, Target::Done, None).unwrap();
    assert_eq!(status_of(&store, c), BacklogStatus::Backlog);
}

#[test]
fn removing_the_cancelled_edge_with_an_open_one_left_swaps_back_to_depends() {
    let (mut store, _) = store();
    let (x, y, z) = (
        add(&mut store, "x"),
        add(&mut store, "y"),
        add(&mut store, "z"),
    );
    depend(&mut store, z, x).unwrap();
    depend(&mut store, z, y).unwrap();
    by_user(&mut store, x, Target::Cancelled, Some("no")).unwrap();
    assert_eq!(
        status_of(&store, z).block().unwrap().reason,
        BlockReason::DepCancelled
    );
    let item = store
        .remove_backlog_dependency(z, x, Actor::User, None)
        .unwrap();
    assert_eq!(
        item.draft.status,
        blocked_for(BlockReason::Depends, &format!("#{y}"), ReturnTo::Backlog)
    );
    by_user(&mut store, y, Target::Done, None).unwrap();
    assert_eq!(status_of(&store, z), BacklogStatus::Backlog);
}

#[test]
fn a_cancelled_dependency_reopened_and_finished_with_the_other_done_frees_the_dependant() {
    let (mut store, _) = store();
    let (n3, n4) = (add(&mut store, "n3"), add(&mut store, "n4"));
    let n5 = add(&mut store, "n5");
    depend(&mut store, n5, n3).unwrap();
    depend(&mut store, n5, n4).unwrap();
    by_user(&mut store, n3, Target::Cancelled, Some("no")).unwrap();
    by_user(&mut store, n3, Target::Backlog, None).unwrap();
    by_user(&mut store, n3, Target::Done, None).unwrap();
    by_user(&mut store, n4, Target::Done, None).unwrap();
    assert_eq!(status_of(&store, n5), BacklogStatus::Backlog);
}

#[test]
fn mixed_done_and_cancelled_dependencies_in_both_orders_stay_blocked_until_the_edge_goes() {
    for done_first in [true, false] {
        let (mut store, _) = store();
        let (a, b, c) = (
            add(&mut store, "a"),
            add(&mut store, "b"),
            add(&mut store, "c"),
        );
        depend(&mut store, c, a).unwrap();
        depend(&mut store, c, b).unwrap();
        if done_first {
            by_user(&mut store, a, Target::Done, None).unwrap();
            by_user(&mut store, b, Target::Cancelled, Some("no")).unwrap();
        } else {
            by_user(&mut store, b, Target::Cancelled, Some("no")).unwrap();
            by_user(&mut store, a, Target::Done, None).unwrap();
        }
        assert_eq!(
            status_of(&store, c),
            blocked_for(
                BlockReason::DepCancelled,
                &format!("#{b} cancelled"),
                ReturnTo::Backlog
            ),
            "done_first {done_first}"
        );
        let item = store
            .remove_backlog_dependency(c, b, Actor::User, None)
            .unwrap();
        assert_eq!(item.draft.status, BacklogStatus::Backlog);
    }
}

#[test]
fn reopening_a_done_dependency_blocks_its_backlog_dependants_again() {
    let (mut store, _) = store();
    let (a, b) = (add(&mut store, "a"), add(&mut store, "b"));
    depend(&mut store, b, a).unwrap();
    by_user(&mut store, a, Target::Done, None).unwrap();
    assert_eq!(status_of(&store, b), BacklogStatus::Backlog);
    by_user(&mut store, a, Target::Backlog, None).unwrap();
    assert_eq!(
        status_of(&store, b),
        blocked_for(BlockReason::Depends, &format!("#{a}"), ReturnTo::Backlog)
    );
}

#[test]
fn started_and_finished_dependants_are_left_alone_when_their_dependency_changes() {
    let (mut store, _) = store();
    let (a, b, c) = (
        add(&mut store, "a"),
        add(&mut store, "b"),
        add(&mut store, "c"),
    );
    by_user(&mut store, a, Target::Done, None).unwrap();
    depend(&mut store, b, a).unwrap();
    depend(&mut store, c, a).unwrap();
    by_user(&mut store, b, Target::InProgress, None).unwrap();
    by_user(&mut store, c, Target::Done, None).unwrap();
    by_user(&mut store, a, Target::Backlog, None).unwrap();
    by_user(&mut store, a, Target::Cancelled, Some("no")).unwrap();
    assert_eq!(status_of(&store, b), BacklogStatus::InProgress);
    assert_eq!(status_of(&store, c), BacklogStatus::Done);
}

#[test]
fn an_item_blocked_out_of_progress_takes_no_dependency() {
    let (mut store, _) = store();
    let (a, b) = (add(&mut store, "a"), add(&mut store, "b"));
    by_user(&mut store, b, Target::InProgress, None).unwrap();
    by_user(
        &mut store,
        b,
        Target::Blocked {
            reason: BlockReason::Decision,
            note: Some("pick".to_owned()),
        },
        None,
    )
    .unwrap();
    let error = depend(&mut store, b, a).unwrap_err().to_string();
    assert!(error.contains("in progress"), "{error}");
}

#[test]
fn several_dependencies_are_added_all_or_nothing() {
    let (mut store, _) = store();
    let (a, b, c) = (
        add(&mut store, "a"),
        add(&mut store, "b"),
        add(&mut store, "c"),
    );
    by_user(&mut store, b, Target::Cancelled, Some("no")).unwrap();
    let error = store
        .add_backlog_dependencies(c, &[a, b], Actor::User, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("cancelled"), "{error}");
    let item = store.backlog_item(c).unwrap();
    assert_eq!(item.deps, Vec::<i64>::new());
    assert_eq!(item.draft.status, BacklogStatus::Backlog);
    let item = store
        .add_backlog_dependencies(c, &[a], Actor::User, None)
        .unwrap();
    assert_eq!(item.deps, [a]);
}

#[test]
fn a_merged_task_frees_the_dependants_of_its_item_and_a_dropped_one_leaves_them_blocked() {
    let (mut store, config) = store();
    let (worker, merged_item) = linked(&mut store, &config, "dep-merged", "merged item");
    let (dropped, dropped_item) = linked(&mut store, &config, "dep-dropped", "dropped item");
    let after_merged = add(&mut store, "after merged");
    let after_dropped = add(&mut store, "after dropped");
    depend(&mut store, after_merged, merged_item).unwrap();
    depend(&mut store, after_dropped, dropped_item).unwrap();
    to_merged(&mut store, &config, &worker);
    assert_eq!(status_of(&store, merged_item), BacklogStatus::Done);
    assert_eq!(status_of(&store, after_merged), BacklogStatus::Backlog);
    store.drop_task(dropped.id, Actor::Orchestrator).unwrap();
    assert_eq!(
        status_of(&store, dropped_item).block().unwrap().reason,
        BlockReason::TaskLost
    );
    assert_eq!(
        status_of(&store, after_dropped).block().unwrap().reason,
        BlockReason::Depends
    );
}

#[test]
fn removing_the_last_open_dependency_unblocks_the_item() {
    let (mut store, _) = store();
    let (a, b, c) = (
        add(&mut store, "a"),
        add(&mut store, "b"),
        add(&mut store, "c"),
    );
    depend(&mut store, c, a).unwrap();
    depend(&mut store, c, b).unwrap();
    let item = store
        .remove_backlog_dependency(c, a, Actor::User, None)
        .unwrap();
    assert_eq!(item.deps, [b]);
    assert_eq!(
        item.draft.status.kind(),
        ItemStatus::Blocked,
        "b is still open"
    );
    let item = store
        .remove_backlog_dependency(c, b, Actor::User, None)
        .unwrap();
    assert_eq!(item.draft.status, BacklogStatus::Backlog);
    let error = store
        .remove_backlog_dependency(c, b, Actor::User, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not depend"), "{error}");
    assert!(event_kinds(&store).contains(&"backlog-undepended".to_owned()));
}

#[test]
fn a_dependant_of_a_cancelled_item_is_freed_when_that_dependency_is_removed() {
    let (mut store, _) = store();
    let (a, b) = (add(&mut store, "a"), add(&mut store, "b"));
    depend(&mut store, b, a).unwrap();
    by_user(&mut store, a, Target::Cancelled, Some("no")).unwrap();
    let item = store
        .remove_backlog_dependency(b, a, Actor::User, None)
        .unwrap();
    assert_eq!(item.draft.status, BacklogStatus::Backlog);
}

#[test]
fn epics_hold_children_one_level_deep() {
    let (mut store, _) = store();
    let kind = |kind, parent| ItemShape {
        kind,
        parent,
        after: Vec::new(),
    };
    let epic = shaped_add(&mut store, "epic", &kind(Some(ItemKind::Epic), None)).unwrap();
    assert_eq!(epic.kind, ItemKind::Epic);
    let child = shaped_add(&mut store, "child", &kind(None, Some(epic.id))).unwrap();
    assert_eq!((child.kind, child.parent), (ItemKind::Item, Some(epic.id)));
    shaped_add(&mut store, "other", &kind(None, None)).unwrap();
    assert!(
        shaped_add(&mut store, "grandchild", &kind(None, Some(child.id))).is_err(),
        "the parent is an item"
    );
    assert!(
        shaped_add(
            &mut store,
            "sub epic",
            &kind(Some(ItemKind::Epic), Some(epic.id))
        )
        .is_err(),
        "an epic has no parent"
    );
    assert!(
        shaped_add(&mut store, "orphan", &kind(None, Some(999))).is_err(),
        "unknown parent"
    );
    let children = store
        .backlog_items(&BacklogFilter {
            epic: Some(epic.id),
            ..BacklogFilter::default()
        })
        .unwrap();
    assert_eq!(
        children.iter().map(|item| item.id).collect::<Vec<_>>(),
        [child.id]
    );
}

#[test]
fn a_refused_dependency_at_add_writes_nothing() {
    let (mut store, _) = store();
    let a = add(&mut store, "a");
    by_user(&mut store, a, Target::Cancelled, Some("no")).unwrap();
    let result = shaped_add(
        &mut store,
        "child",
        &ItemShape {
            after: vec![a],
            ..ItemShape::default()
        },
    );
    assert!(result.is_err());
    assert_eq!(
        store
            .backlog_items(&BacklogFilter::default())
            .unwrap()
            .len(),
        1,
        "the new item was rolled back"
    );
}
