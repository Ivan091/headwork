//! What the "Needs you" digest reads: waiting asks, standing findings, pending notes, pull requests that wait, and the
//! daily push mark.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::lifecycle::SessionFacts;
use headwork_core::{
    Actor, AskKind, Config, FindingId, PermissionMode, ProjectName, Role, Slug, TaskRequest,
    Trigger,
};
use headwork_store::{EVENT_DIGEST_PUSHED, NewAsk, NewSession, NewTask, PrSeen, Store, Task};

fn store() -> (Store, Config) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    let config = Config::parse(&std::fs::read_to_string(path).unwrap(), "config.toml").unwrap();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_projects(&config).unwrap();
    (store, config)
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

fn in_review(store: &mut Store, config: &Config, slug: &str) -> Task {
    let task = task(store, config, slug);
    store.start_task(config, task.id, Actor::Tick).unwrap();
    for trigger in [
        Trigger::SessionStarted(SessionFacts {
            session_matches: true,
            launcher_alive: true,
        }),
        Trigger::ReviewOpened { repo_allowed: true },
    ] {
        store.transition(task.id, &trigger, Actor::Tick).unwrap();
    }
    store.task(task.id).unwrap()
}

fn open_pr(store: &mut Store, task: &Task, number: i64) {
    store
        .record_pr(
            task.id,
            &PrSeen {
                repo: "Ivan091/demo",
                number,
                url: &format!("https://example.test/demo/pull/{number}"),
                state: "open",
            },
            Actor::Tick,
        )
        .unwrap();
}

fn session(store: &mut Store, id: &str, task: &Task) {
    store
        .record_session(
            &NewSession {
                id: id.to_owned(),
                task_id: Some(task.id),
                role: Role::Worker,
                rc_name: id.to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::Hook,
        )
        .unwrap();
}

#[test]
fn asks_wait_with_their_task_and_age() {
    let (mut store, config) = store();
    let task = task(&mut store, &config, "one");
    for (task_id, question) in [(Some(task.id), "Merge it?"), (None, "Pick a plan")] {
        store
            .open_ask(
                &NewAsk {
                    task_id,
                    session_id: None,
                    kind: AskKind::Question,
                    question: question.to_owned(),
                    options: vec!["yes".to_owned()],
                },
                Actor::Hook,
            )
            .unwrap();
    }
    let asks = store.digest_asks().unwrap();
    assert_eq!(asks.len(), 2, "both wait");
    let named: Vec<Option<&str>> = asks.iter().map(|ask| ask.task.as_deref()).collect();
    assert_eq!(named, [Some("me-sim-one"), None], "task names");
    assert!(asks.iter().all(|ask| ask.waiting_s < 60), "just asked");
}

#[test]
fn findings_split_into_standing_ones_and_pending_notes() {
    let (mut store, config) = store();
    let task = task(&mut store, &config, "one");
    store
        .raise_finding(
            "stuck:1",
            FindingId::Stuck,
            Some(task.id),
            &serde_json::json!({}),
            Actor::Tick,
        )
        .unwrap();
    store
        .raise_finding(
            "migration-pending:demo/2026-11-26-x",
            FindingId::MigrationPending,
            None,
            &serde_json::json!({ "note": "demo/2026-11-26-x", "title": "Set a key" }),
            Actor::Tick,
        )
        .unwrap();
    let findings = store.digest_findings().unwrap();
    assert_eq!(findings.len(), 1, "notes are not findings here");
    let finding = findings.first().unwrap();
    assert_eq!(
        (finding.kind.as_str(), finding.task.as_deref()),
        ("stuck", Some("me-sim-one")),
        "kind and task"
    );
    let notes = store.digest_notes().unwrap();
    assert_eq!(notes.len(), 1, "one note");
    let note = notes.first().unwrap();
    assert_eq!(
        (note.address.as_str(), note.title.as_str()),
        ("demo/2026-11-26-x", "Set a key"),
        "address and title"
    );
}

#[test]
fn a_pull_request_waits_once_its_session_ended_and_not_while_it_works() {
    let (mut store, config) = store();
    let waiting = in_review(&mut store, &config, "ended");
    open_pr(&mut store, &waiting, 1);
    open_pr(&mut store, &waiting, 2);
    let working = in_review(&mut store, &config, "working");
    open_pr(&mut store, &working, 3);
    session(&mut store, "s-ended", &waiting);
    session(&mut store, "s-working", &working);
    store.session_ended("s-ended", Some(0)).unwrap();
    let prs = store.digest_prs().unwrap();
    assert_eq!(prs.len(), 1, "only the ended session's task");
    let pr = prs.first().unwrap();
    assert_eq!(pr.task, "me-sim-ended", "the task");
    assert_eq!(pr.urls.len(), 2, "both pull requests");

    store
        .set_session_waiting("s-working", true, Actor::Tick)
        .unwrap();
    assert_eq!(
        store.digest_prs().unwrap().len(),
        2,
        "a session marked waiting on the user counts"
    );
}

#[test]
fn the_daily_push_is_due_from_its_hour_until_one_is_logged_today() {
    let (store, _) = store();
    assert!(store.digest_push_due(0).unwrap(), "hour 0 has passed");
    assert!(
        !store.digest_push_due(24).unwrap(),
        "an hour that never comes"
    );
    store
        .append_event(
            Actor::Tick,
            None,
            None,
            EVENT_DIGEST_PUSHED,
            &serde_json::json!({}),
        )
        .unwrap();
    assert!(!store.digest_push_due(0).unwrap(), "already pushed today");
}
