//! The backlog list in the store: adding, moving through the status guard, importing by text, the link to a task and what
//! its end does.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::backlog::{
    BacklogActor, BacklogDraft, BacklogItem, BacklogOwner, BacklogStatus, BlockReason, ItemStatus,
    ReturnTo, Target, parse,
};
use headwork_core::lifecycle::{CleanupFacts, MergePolicy, MergeSource, SessionFacts, WorktreeEnd};
use headwork_core::{
    Actor, Config, PermissionMode, ProjectName, Slug, TaskRequest, TaskState, Trigger,
};
use headwork_store::{
    BacklogEdit, BacklogFilter, BacklogMove, EVENT_BACKLOG_ADDED, EVENT_BACKLOG_EXPORTED,
    EVENT_BACKLOG_IMPORTED, EVENT_BACKLOG_MOVED, EVENT_BACKLOG_PICKED, EventFilter, NewTask,
    PrSeen, Store, StoreError, Task, TaskId,
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
        &BacklogMove::new(target, BacklogActor::User, Actor::User).with_reason(reason),
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

#[test]
fn a_merged_task_finishes_its_item_at_once_and_its_close_changes_nothing() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "merge", "Do it");
    store.start_task(&config, worker.id, Actor::Tick).unwrap();
    merged_pr(&mut store, &worker, 3, "merged");
    for trigger in [
        Trigger::SessionStarted(SessionFacts {
            session_matches: true,
            launcher_alive: true,
        }),
        Trigger::ReviewOpened { repo_allowed: true },
        Trigger::Merged {
            source: MergeSource::GitHost,
            policy: MergePolicy::Deliver,
        },
    ] {
        store.transition(worker.id, &trigger, Actor::Tick).unwrap();
    }
    assert_eq!(store.task(worker.id).unwrap().state, TaskState::Merged);
    let done = store.backlog_item(id).unwrap();
    assert_eq!(
        (
            done.draft.status,
            done.draft.done_note.as_deref(),
            done.task
        ),
        (
            BacklogStatus::Done,
            Some("task me-sim-merge merged https://github.com/Ivan091/me-sim/pull/3"),
            None
        )
    );
    let before = moves(&store).len();
    close(&mut store, &worker, &[Trigger::ForceClose]);
    assert_eq!(
        moves(&store).len(),
        before,
        "the close finds no item in progress"
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
        .add_backlog_item_for(&draft("before the pick"), Actor::Worker, Some(task.id))
        .unwrap();
    pick(&store, 2);
    assert_eq!(left(&store, task.id), 2);
    store
        .add_backlog_item_for(&draft("first"), Actor::Worker, Some(task.id))
        .unwrap();
    store
        .add_backlog_item(&draft("another task's"), Actor::Orchestrator)
        .unwrap();
    assert_eq!(left(&store, task.id), 1);
    store
        .add_backlog_item_for(&draft("second"), Actor::Worker, Some(task.id))
        .unwrap();
    assert_eq!(left(&store, task.id), 0);
    pick(&store, 1);
    assert_eq!(left(&store, task.id), 1, "a later pick replaces the count");
    pick(&store, 0);
    assert_eq!(left(&store, task.id), 0);
}
