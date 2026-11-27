//! The backlog list in the store: adding, finishing, importing by text, the link to a task and what its end does.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::backlog::{BacklogDraft, BacklogOwner, BacklogState, parse};
use headwork_core::lifecycle::{CleanupFacts, WorktreeEnd};
use headwork_core::{
    Actor, Config, PermissionMode, ProjectName, Slug, TaskRequest, TaskState, Trigger,
};
use headwork_store::{
    BacklogEdit, BacklogFilter, EVENT_BACKLOG_ADDED, EVENT_BACKLOG_DONE, EVENT_BACKLOG_DROPPED,
    EVENT_BACKLOG_EDITED, EVENT_BACKLOG_EXPORTED, EVENT_BACKLOG_IMPORTED, EVENT_BACKLOG_PICKED,
    EventFilter, NewTask, PrSeen, Store, StoreError, Task,
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
    draft.state = BacklogState::Open;
    draft
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
fn finishing_dropping_and_reopening_follow_the_state() {
    let (mut store, _) = store();
    let one = store.add_backlog_item(&draft("one"), Actor::User).unwrap();
    let done = store
        .finish_backlog_item(one.id, Some("by hand"), false, Actor::User)
        .unwrap();
    assert_eq!(done.draft.state, BacklogState::Done);
    assert_eq!(done.draft.done_note.as_deref(), Some("by hand"));
    assert_eq!(done.draft.done_on, Some(store.backlog_today().unwrap()));
    let again = store.finish_backlog_item(one.id, None, false, Actor::User);
    assert!(
        matches!(again, Err(StoreError::BacklogRefused { .. })),
        "{again:?}"
    );
    let reopened = store.reopen_backlog_item(one.id, Actor::User).unwrap();
    assert_eq!(
        (reopened.draft.state, reopened.draft.done_on),
        (BacklogState::Open, None)
    );
    let dropped = store
        .finish_backlog_item(one.id, None, true, Actor::User)
        .unwrap();
    assert_eq!(dropped.draft.state, BacklogState::Dropped);
    let kinds = kinds(&store);
    for kind in [
        EVENT_BACKLOG_DONE,
        EVENT_BACKLOG_DROPPED,
        EVENT_BACKLOG_EDITED,
    ] {
        assert!(kinds.contains(&kind.to_owned()), "{kind} in {kinds:?}");
    }
    assert!(matches!(
        store.backlog_item(999),
        Err(StoreError::UnknownBacklogItem(999))
    ));
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
    store
        .finish_backlog_item(known.id, None, false, Actor::User)
        .unwrap();
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
        rows.first().map(|t| t.draft.state),
        Some(BacklogState::Done),
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
fn a_linked_item_is_refused_to_a_second_live_task_and_freed_when_the_first_drops() {
    let (mut store, config) = store();
    let item = store
        .add_backlog_item(&draft("Do it"), Actor::Orchestrator)
        .unwrap();
    let first = task(&mut store, &config, "first");
    store
        .link_backlog_item(item.id, first.id, Actor::Orchestrator)
        .unwrap();
    assert_eq!(
        store.backlog_item(item.id).unwrap().task.map(|t| t.name),
        Some("me-sim-first".to_owned())
    );
    let second = task(&mut store, &config, "second");
    let refused = store.link_backlog_item(item.id, second.id, Actor::Orchestrator);
    assert!(
        matches!(refused, Err(StoreError::BacklogRefused { .. })),
        "{refused:?}"
    );
    store.drop_task(first.id, Actor::Orchestrator).unwrap();
    let freed = store.backlog_item(item.id).unwrap();
    assert_eq!(
        (freed.draft.state, freed.task),
        (BacklogState::Open, None),
        "a dropped task frees the item"
    );
    store
        .link_backlog_item(item.id, second.id, Actor::Orchestrator)
        .unwrap();
    close(&mut store, &first, &[Trigger::BeginClosing]);
    let still = store.backlog_item(item.id).unwrap();
    assert_eq!(
        still.draft.state,
        BacklogState::Open,
        "the first task closing changes nothing"
    );
    assert_eq!(still.task.map(|t| t.name), Some("me-sim-second".to_owned()));
}

#[test]
fn a_closed_task_finishes_its_item_with_its_name_and_merged_pull_requests() {
    let (mut store, config) = store();
    let item = store
        .add_backlog_item(&draft("Do it"), Actor::Orchestrator)
        .unwrap();
    let worker = task(&mut store, &config, "worker");
    store
        .link_backlog_item(item.id, worker.id, Actor::Orchestrator)
        .unwrap();
    for (number, state) in [(7, "merged"), (8, "closed")] {
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
    close(&mut store, &worker, &[Trigger::ForceClose]);
    assert_eq!(store.task(worker.id).unwrap().state, TaskState::Closed);
    let done = store.backlog_item(item.id).unwrap();
    assert_eq!(done.draft.state, BacklogState::Done);
    assert_eq!(done.draft.done_on, Some(store.backlog_today().unwrap()));
    assert_eq!(
        done.draft.done_note.as_deref(),
        Some("task me-sim-worker merged https://github.com/Ivan091/me-sim/pull/7")
    );
    assert!(kinds(&store).contains(&EVENT_BACKLOG_DONE.to_owned()));
}

#[test]
fn an_item_that_is_not_open_cannot_be_linked() {
    let (mut store, config) = store();
    let item = store
        .add_backlog_item(&draft("Done already"), Actor::User)
        .unwrap();
    store
        .finish_backlog_item(item.id, None, false, Actor::User)
        .unwrap();
    let worker = task(&mut store, &config, "late");
    let refused = store.link_backlog_item(item.id, worker.id, Actor::Orchestrator);
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
    store
        .link_backlog_item(item.id, worker.id, Actor::Orchestrator)
        .unwrap();
    (worker, item.id)
}

#[test]
fn a_failed_task_frees_its_item_and_leaves_it_open() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "broken", "Do it");
    store.start_task(&config, worker.id, Actor::Tick).unwrap();
    store
        .transition(worker.id, &Trigger::LaunchFailed, Actor::Tick)
        .unwrap();
    assert_eq!(store.task(worker.id).unwrap().state, TaskState::Failed);
    let freed = store.backlog_item(id).unwrap();
    assert_eq!((freed.draft.state, freed.task), (BacklogState::Open, None));
    assert!(kinds(&store).contains(&EVENT_BACKLOG_EDITED.to_owned()));
}

#[test]
fn a_closing_task_leaves_an_item_that_is_already_done_or_dropped_as_it_was() {
    let (mut store, config) = store();
    let (done_worker, done_id) = linked(&mut store, &config, "first", "Finished by hand");
    store
        .finish_backlog_item(done_id, Some("by hand"), false, Actor::User)
        .unwrap();
    let (dropped_worker, dropped_id) = linked(&mut store, &config, "second", "Dropped by hand");
    store
        .finish_backlog_item(dropped_id, Some("not needed"), true, Actor::User)
        .unwrap();
    close(&mut store, &done_worker, &[Trigger::ForceClose]);
    close(&mut store, &dropped_worker, &[Trigger::ForceClose]);
    let done = store.backlog_item(done_id).unwrap();
    assert_eq!(
        (done.draft.state, done.draft.done_note.as_deref()),
        (BacklogState::Done, Some("by hand"))
    );
    let dropped = store.backlog_item(dropped_id).unwrap();
    assert_eq!(
        (dropped.draft.state, dropped.draft.done_note.as_deref()),
        (BacklogState::Dropped, Some("not needed"))
    );
}

#[test]
fn the_done_note_lists_every_merged_pull_request_in_order() {
    let (mut store, config) = store();
    let (worker, id) = linked(&mut store, &config, "two", "Do both");
    for number in [9, 4] {
        store
            .record_pr(
                worker.id,
                &PrSeen {
                    repo: "Ivan091/me-sim",
                    number,
                    url: &format!("https://github.com/Ivan091/me-sim/pull/{number}"),
                    state: "merged",
                },
                Actor::Tick,
            )
            .unwrap();
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
        (|d| d.project = "Done".to_owned(), "Done or Dropped"),
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
