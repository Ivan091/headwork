//! The to-do list in the store: adding, finishing, importing by text, the link to a task and what its end does.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::lifecycle::{CleanupFacts, WorktreeEnd};
use headwork_core::todo::{TodoDraft, TodoOwner, TodoState, parse};
use headwork_core::{
    Actor, Config, PermissionMode, ProjectName, Slug, TaskRequest, TaskState, Trigger,
};
use headwork_store::{
    EVENT_TODO_ADDED, EVENT_TODO_DONE, EVENT_TODO_DROPPED, EVENT_TODO_EDITED, EVENT_TODO_EXPORTED,
    EVENT_TODO_IMPORTED, EventFilter, NewTask, PrSeen, Store, StoreError, Task, TodoEdit,
    TodoFilter,
};

fn store() -> (Store, Config) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    let config = Config::parse(&std::fs::read_to_string(path).unwrap(), "config.toml").unwrap();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_projects(&config).unwrap();
    (store, config)
}

fn draft(text: &str) -> TodoDraft {
    let mut draft = parse(&format!("## me-sim\n\n- [ ] {text}\n"))
        .items
        .remove(0);
    draft.state = TodoState::Open;
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
        .filter(|kind| kind.starts_with("todo-"))
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
    let first = store.add_todo(&draft("one"), Actor::Orchestrator).unwrap();
    let mut mine = draft("For you: two");
    mine.owner = TodoOwner::User;
    mine.project = "headwork".to_owned();
    let second = store.add_todo(&mine, Actor::Orchestrator).unwrap();
    assert!(second.position > first.position, "new items go last");
    let all = store.todos(&TodoFilter::default()).unwrap();
    assert_eq!(
        all.iter().map(|t| t.id).collect::<Vec<_>>(),
        [first.id, second.id]
    );
    let user = store
        .todos(&TodoFilter {
            owner: Some(TodoOwner::User),
            ..TodoFilter::default()
        })
        .unwrap();
    assert_eq!(user.len(), 1, "owner filter");
    let project = store
        .todos(&TodoFilter {
            project: Some("me-sim".to_owned()),
            ..TodoFilter::default()
        })
        .unwrap();
    assert_eq!(project.len(), 1, "project filter");
    assert!(
        kinds(&store).contains(&EVENT_TODO_ADDED.to_owned()),
        "event"
    );
}

#[test]
fn finishing_dropping_and_reopening_follow_the_state() {
    let (mut store, _) = store();
    let one = store.add_todo(&draft("one"), Actor::User).unwrap();
    let done = store
        .finish_todo(one.id, Some("by hand"), false, Actor::User)
        .unwrap();
    assert_eq!(done.state, TodoState::Done);
    assert_eq!(done.done_note.as_deref(), Some("by hand"));
    assert_eq!(done.done_on, Some(store.todo_today().unwrap()));
    let again = store.finish_todo(one.id, None, false, Actor::User);
    assert!(
        matches!(again, Err(StoreError::TodoRefused { .. })),
        "{again:?}"
    );
    let reopened = store.reopen_todo(one.id, Actor::User).unwrap();
    assert_eq!((reopened.state, reopened.done_on), (TodoState::Open, None));
    let dropped = store.finish_todo(one.id, None, true, Actor::User).unwrap();
    assert_eq!(dropped.state, TodoState::Dropped);
    let kinds = kinds(&store);
    for kind in [EVENT_TODO_DONE, EVENT_TODO_DROPPED, EVENT_TODO_EDITED] {
        assert!(kinds.contains(&kind.to_owned()), "{kind} in {kinds:?}");
    }
    assert!(matches!(store.todo(999), Err(StoreError::UnknownTodo(999))));
}

#[test]
fn an_edit_changes_only_what_it_names_and_appends_notes() {
    let (mut store, _) = store();
    let one = store.add_todo(&draft("one"), Actor::User).unwrap();
    let edited = store
        .edit_todo(
            one.id,
            &TodoEdit {
                text: Some("uno".to_owned()),
                section: Some(Some("Audit".to_owned())),
                due: Some(Some("2026-12-24".parse().unwrap())),
                append: Some("a note".to_owned()),
                ..TodoEdit::default()
            },
            Actor::User,
        )
        .unwrap();
    assert_eq!(edited.text, "uno");
    assert_eq!(edited.project, "me-sim", "project untouched");
    assert_eq!(edited.section.as_deref(), Some("Audit"));
    assert_eq!(
        edited.due.map(|d| d.to_string()).as_deref(),
        Some("2026-12-24")
    );
    assert_eq!(edited.notes, ["a note"]);
    let cleared = store
        .edit_todo(
            one.id,
            &TodoEdit {
                due: Some(None),
                section: Some(None),
                ..TodoEdit::default()
            },
            Actor::User,
        )
        .unwrap();
    assert_eq!((cleared.due, cleared.section), (None, None));
}

#[test]
fn import_counts_new_known_and_repeated_items_and_changes_nothing_else() {
    let (mut store, _) = store();
    let known = store.add_todo(&draft("Already here"), Actor::User).unwrap();
    store
        .finish_todo(known.id, None, false, Actor::User)
        .unwrap();
    let drafts = parse(
        "## p\n\n- [ ] Brand new\n- [ ] For you: Already here\n- [ ] Brand  new\n- [ ] Another\n",
    )
    .items;
    let plan = store.todo_import_plan(&drafts).unwrap();
    assert_eq!((plan.added, plan.already, plan.duplicate), (2, 1, 1));
    assert_eq!(
        store.todos(&TodoFilter::default()).unwrap().len(),
        1,
        "a plan writes nothing"
    );
    let counts = store
        .import_todos("S:/x/todo.md", &drafts, Actor::User)
        .unwrap();
    assert_eq!(counts, plan);
    let rows = store.todos(&TodoFilter::default()).unwrap();
    assert_eq!(
        rows.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
        ["Already here", "Brand new", "Another"],
        "file order after the old rows"
    );
    assert_eq!(
        rows.first().map(|t| t.state),
        Some(TodoState::Done),
        "an old row is not changed"
    );
    let again = store
        .import_todos("S:/x/todo.md", &drafts, Actor::User)
        .unwrap();
    assert_eq!(
        (again.added, again.already, again.duplicate),
        (0, 3, 1),
        "idempotent"
    );
    assert!(
        store
            .todo_file("S:/x/todo.md")
            .unwrap()
            .unwrap()
            .imported_at
            .is_some()
    );
    assert!(kinds(&store).contains(&EVENT_TODO_IMPORTED.to_owned()));
}

#[test]
fn the_export_record_keeps_the_last_hash() {
    let (mut store, _) = store();
    assert!(store.todo_file("p").unwrap().is_none());
    store.record_todo_export("p", "abc", Actor::Tick).unwrap();
    let file = store.todo_file("p").unwrap().unwrap();
    assert_eq!(
        (file.export_hash.as_deref(), file.imported_at),
        (Some("abc"), None)
    );
    store.import_todos("p", &[], Actor::User).unwrap();
    let file = store.todo_file("p").unwrap().unwrap();
    assert_eq!(
        file.export_hash.as_deref(),
        Some("abc"),
        "an import keeps the hash"
    );
    assert!(file.imported_at.is_some());
    assert!(kinds(&store).contains(&EVENT_TODO_EXPORTED.to_owned()));
}

#[test]
fn a_linked_item_is_refused_to_a_second_live_task_and_freed_when_the_first_drops() {
    let (mut store, config) = store();
    let item = store
        .add_todo(&draft("Do it"), Actor::Orchestrator)
        .unwrap();
    let first = task(&mut store, &config, "first");
    store
        .link_todo(item.id, first.id, Actor::Orchestrator)
        .unwrap();
    assert_eq!(
        store.todo(item.id).unwrap().task.map(|t| t.name),
        Some("me-sim-first".to_owned())
    );
    let second = task(&mut store, &config, "second");
    let refused = store.link_todo(item.id, second.id, Actor::Orchestrator);
    assert!(
        matches!(refused, Err(StoreError::TodoRefused { .. })),
        "{refused:?}"
    );
    store.drop_task(first.id, Actor::Orchestrator).unwrap();
    let freed = store.todo(item.id).unwrap();
    assert_eq!(
        (freed.state, freed.task),
        (TodoState::Open, None),
        "a dropped task frees the item"
    );
    store
        .link_todo(item.id, second.id, Actor::Orchestrator)
        .unwrap();
    close(&mut store, &first, &[Trigger::BeginClosing]);
    let still = store.todo(item.id).unwrap();
    assert_eq!(
        still.state,
        TodoState::Open,
        "the first task closing changes nothing"
    );
    assert_eq!(still.task.map(|t| t.name), Some("me-sim-second".to_owned()));
}

#[test]
fn a_closed_task_finishes_its_item_with_its_name_and_merged_pull_requests() {
    let (mut store, config) = store();
    let item = store
        .add_todo(&draft("Do it"), Actor::Orchestrator)
        .unwrap();
    let worker = task(&mut store, &config, "worker");
    store
        .link_todo(item.id, worker.id, Actor::Orchestrator)
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
    let done = store.todo(item.id).unwrap();
    assert_eq!(done.state, TodoState::Done);
    assert_eq!(done.done_on, Some(store.todo_today().unwrap()));
    assert_eq!(
        done.done_note.as_deref(),
        Some("task me-sim-worker merged https://github.com/Ivan091/me-sim/pull/7")
    );
    assert!(kinds(&store).contains(&EVENT_TODO_DONE.to_owned()));
}

#[test]
fn an_item_that_is_not_open_cannot_be_linked() {
    let (mut store, config) = store();
    let item = store.add_todo(&draft("Done already"), Actor::User).unwrap();
    store
        .finish_todo(item.id, None, false, Actor::User)
        .unwrap();
    let worker = task(&mut store, &config, "late");
    let refused = store.link_todo(item.id, worker.id, Actor::Orchestrator);
    assert!(
        matches!(refused, Err(StoreError::TodoRefused { .. })),
        "{refused:?}"
    );
}
