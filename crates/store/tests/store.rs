//! The store against in-memory and on-disk databases, with the config fixtures.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::lifecycle::{CleanupFacts, MergePolicy, MergeSource, SessionFacts, WorktreeEnd};
use headwork_core::{
    Actor, AdmissionRefusal, AskKind, AskState, Config, MachineMode, Model, PermissionMode,
    ProjectName, Refusal, Role, SessionState, Slug, TaskRequest, TaskState, Trigger,
};
use headwork_store::{
    EventFilter, FABLE_ALLOW, MIGRATIONS, NewAsk, NewSession, NewTask, Store, StoreError, Task,
};

fn config() -> Config {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    Config::parse(&std::fs::read_to_string(path).unwrap(), "config.toml").unwrap()
}

fn store() -> (Store, Config) {
    let config = config();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_projects(&config).unwrap();
    (store, config)
}

fn request(project: &str, slug: &str, kind: &str) -> NewTask {
    NewTask {
        request: TaskRequest {
            project: ProjectName::new(project).unwrap(),
            slug: Slug::new(slug).unwrap(),
            kind: kind.to_owned(),
            model: None,
            in_main: false,
            keep: false,
        },
        brief_path: format!("briefs/{project}-{slug}.md"),
        depends_on: None,
        permission_mode: PermissionMode::Auto,
    }
}

fn create(store: &mut Store, config: &Config, project: &str, slug: &str) -> Task {
    store
        .create_task(config, &request(project, slug, "scene"), Actor::Root)
        .unwrap()
}

const LIVE: SessionFacts = SessionFacts {
    session_matches: true,
    launcher_alive: true,
};

#[test]
fn migrations_bring_a_new_database_to_the_latest_version() {
    let store = Store::open_in_memory().unwrap();
    let latest = i64::try_from(MIGRATIONS.len()).unwrap();
    assert_eq!(store.schema_version().unwrap(), latest, "fully migrated");
    assert_eq!(
        store.machine_mode().unwrap(),
        MachineMode::Normal,
        "machine starts normal"
    );
}

#[test]
fn reopening_keeps_data_and_newer_schemas_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let config = config();
    {
        let mut store = Store::open(&path).unwrap();
        store.sync_projects(&config).unwrap();
        create(&mut store, &config, "me-sim", "sand-cascade");
    }
    let store = Store::open(&path).unwrap();
    assert!(
        store.find_task("me-sim-sand-cascade").unwrap().is_some(),
        "task survives a reopen"
    );
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    let error = Store::open(&path).unwrap_err();
    assert!(
        matches!(error, StoreError::SchemaTooNew { found: 99, .. }),
        "{error}"
    );
}

#[test]
fn projects_sync_and_disable_when_removed() {
    let (mut store, mut config) = store();
    let names: Vec<_> = store
        .projects()
        .unwrap()
        .into_iter()
        .map(|p| p.name.to_string())
        .collect();
    assert_eq!(names, ["me-sim", "me-studio", "work-service"], "all synced");
    config
        .projects
        .remove(&ProjectName::new("me-studio").unwrap());
    store.sync_projects(&config).unwrap();
    let studio = store
        .projects()
        .unwrap()
        .into_iter()
        .find(|p| p.name.as_str() == "me-studio")
        .unwrap();
    assert!(!studio.enabled, "removed project is disabled");
}

#[test]
fn creating_tasks_checks_admission() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand-cascade");
    assert_eq!(task.state, TaskState::Queued, "new tasks queue");
    assert_eq!(task.model, Model::Sonnet, "default model");
    assert_eq!(task.name.to_string(), "me-sim-sand-cascade", "name");

    let again = store.create_task(
        &config,
        &request("me-sim", "sand-cascade", "scene"),
        Actor::Root,
    );
    assert!(
        matches!(
            again,
            Err(StoreError::NotAdmitted(AdmissionRefusal::NameTaken { .. }))
        ),
        "name taken"
    );

    let training = store.create_task(
        &config,
        &request("me-sim", "train", "car-training"),
        Actor::Root,
    );
    assert!(
        matches!(
            training,
            Err(StoreError::NotAdmitted(AdmissionRefusal::MainOnlyKind(_)))
        ),
        "main-only"
    );

    let mut fable = request("work-service", "x", "fix");
    fable.request.model = Some(Model::Fable);
    let refused = store.create_task(&config, &fable, Actor::Root);
    assert!(
        matches!(
            refused,
            Err(StoreError::NotAdmitted(AdmissionRefusal::ModelNotAllowed(
                Model::Fable
            )))
        ),
        "no Fable at work"
    );

    let unknown = store.create_task(&config, &request("nope", "x", "fix"), Actor::Root);
    assert!(
        matches!(
            unknown,
            Err(StoreError::NotAdmitted(AdmissionRefusal::UnknownProject(_)))
        ),
        "unknown"
    );
}

#[test]
fn a_closed_task_frees_its_name() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    let clean = CleanupFacts {
        pane_gone: true,
        processes_gone: true,
        worktree: WorktreeEnd::Removed,
    };
    for trigger in [Trigger::Drop, Trigger::BeginClosing] {
        store.transition(task.id, &trigger, Actor::Root).unwrap();
    }
    let while_closing =
        store.create_task(&config, &request("me-sim", "sand", "scene"), Actor::Root);
    assert!(
        while_closing.is_err(),
        "a closing task still holds its name"
    );
    store
        .transition(task.id, &Trigger::Cleaned(clean), Actor::Tick)
        .unwrap();
    let again = create(&mut store, &config, "me-sim", "sand");
    assert_ne!(again.id, task.id, "a new task with the same name");
    assert_eq!(
        store.find_task("me-sim-sand").unwrap().unwrap().id,
        again.id,
        "the live one is found first"
    );
}

#[test]
fn start_reads_slots_from_the_store() {
    let (mut store, config) = store();
    let first = create(&mut store, &config, "work-service", "a");
    for slug in ["b", "c"] {
        let task = create(&mut store, &config, "work-service", slug);
        store.start_task(&config, task.id, Actor::Tick).unwrap();
    }
    store.start_task(&config, first.id, Actor::Tick).unwrap();
    let fourth = create(&mut store, &config, "work-service", "d");
    let refused = store
        .start_task(&config, fourth.id, Actor::Tick)
        .unwrap_err();
    assert!(
        matches!(
            refused,
            StoreError::Refused {
                refusal: Refusal::ProjectCapReached { cap: 3 },
                ..
            }
        ),
        "work-service caps at 3: {refused}"
    );
    let other = create(&mut store, &config, "me-sim", "e");
    assert!(
        store.start_task(&config, other.id, Actor::Tick).is_ok(),
        "other projects still start"
    );
}

#[test]
fn main_only_kinds_run_one_at_a_time() {
    let (mut store, config) = store();
    let mut training = request("me-sim", "train-a", "car-training");
    training.request.in_main = true;
    let first = store.create_task(&config, &training, Actor::Root).unwrap();
    training.request.slug = Slug::new("train-b").unwrap();
    let second = store.create_task(&config, &training, Actor::Root).unwrap();
    store.start_task(&config, first.id, Actor::Tick).unwrap();
    let refused = store
        .start_task(&config, second.id, Actor::Tick)
        .unwrap_err();
    assert!(
        matches!(
            refused,
            StoreError::Refused {
                refusal: Refusal::MainOnlyKindBusy,
                ..
            }
        ),
        "{refused}"
    );
}

#[test]
fn dependencies_wait_for_the_merge() {
    let (mut store, config) = store();
    let engine = create(&mut store, &config, "me-sim", "engine");
    let mut scene = request("me-sim", "scene", "scene");
    scene.depends_on = Some(engine.id);
    let scene = store.create_task(&config, &scene, Actor::Root).unwrap();
    let refused = store
        .start_task(&config, scene.id, Actor::Tick)
        .unwrap_err();
    assert!(
        matches!(
            refused,
            StoreError::Refused {
                refusal: Refusal::DependencyNotMerged,
                ..
            }
        ),
        "{refused}"
    );

    store.start_task(&config, engine.id, Actor::Tick).unwrap();
    let merged = Trigger::Merged {
        source: MergeSource::GitHost,
        policy: MergePolicy::Deliver,
    };
    for trigger in [
        Trigger::SessionStarted(LIVE),
        Trigger::ReviewOpened { repo_allowed: true },
        merged,
    ] {
        store.transition(engine.id, &trigger, Actor::Tick).unwrap();
    }
    assert!(
        store.start_task(&config, scene.id, Actor::Tick).is_ok(),
        "starts after the merge"
    );
}

#[test]
fn fable_waits_for_the_users_approval() {
    let (mut store, config) = store();
    let mut fable = request("me-sim", "deep", "scene");
    fable.request.model = Some(Model::Fable);
    let task = store.create_task(&config, &fable, Actor::Root).unwrap();
    let ask = task.fable_ask.unwrap();
    let board = store.board(false).unwrap();
    let pending = &board.tasks.first().unwrap().asks;
    assert_eq!(pending.len(), 1, "the approval is on the board");
    assert_eq!(
        pending.first().unwrap().state,
        AskState::Pending,
        "pending until put to the user"
    );

    let refused = store.start_task(&config, task.id, Actor::Tick).unwrap_err();
    assert!(
        matches!(
            refused,
            StoreError::Refused {
                refusal: Refusal::FableNotAllowed,
                ..
            }
        ),
        "{refused}"
    );
    store.answer_ask(ask, FABLE_ALLOW, Actor::Hook).unwrap();
    assert!(
        store.start_task(&config, task.id, Actor::Tick).is_ok(),
        "starts once allowed"
    );
}

#[test]
fn refusals_and_transitions_are_logged() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    assert!(
        store
            .transition(task.id, &Trigger::AskOpened, Actor::Hook)
            .is_err(),
        "queued cannot ask"
    );
    store.start_task(&config, task.id, Actor::Tick).unwrap();
    let events = store
        .events(EventFilter {
            task: Some(task.id),
            limit: 50,
        })
        .unwrap();
    let kinds: Vec<_> = events.iter().map(|event| event.kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["task-created", "refused", "transition"],
        "the task's history in order"
    );
    let refused = events.get(1).unwrap();
    assert_eq!(refused.actor, Actor::Hook, "actor recorded");
    assert_eq!(refused.data["trigger"], "ask-opened", "trigger recorded");
    assert_eq!(
        refused.task.as_deref(),
        Some("me-sim-sand"),
        "task name joined"
    );
    let last = store
        .events(EventFilter {
            task: None,
            limit: 1,
        })
        .unwrap();
    assert_eq!(last.len(), 1, "limit keeps the newest");
    assert_eq!(
        last.first().unwrap().data["to"],
        "spawning",
        "newest is the start"
    );
}

#[test]
fn board_shows_sessions_asks_and_states() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    create(&mut store, &config, "me-studio", "charts");
    store.start_task(&config, task.id, Actor::Tick).unwrap();
    let session = NewSession {
        id: "0b6c1d2e-0000-4000-8000-000000000001".to_owned(),
        task_id: Some(task.id),
        role: Role::Child,
        rc_name: "me-sim-sand".to_owned(),
        host: "wmux".to_owned(),
        host_workspace: Some("ws-1".to_owned()),
        host_surface: Some("surface:7".to_owned()),
    };
    store.record_session(&session, Actor::Tick).unwrap();
    store
        .set_session_state(&session.id, SessionState::Live, Actor::Hook)
        .unwrap();
    store
        .transition(task.id, &Trigger::SessionStarted(LIVE), Actor::Hook)
        .unwrap();
    let ask = NewAsk {
        task_id: Some(task.id),
        session_id: Some(session.id.clone()),
        kind: AskKind::Review,
        question: "Ship the sand render?".to_owned(),
        options: vec!["Good, ship it".to_owned(), "Drop it".to_owned()],
    };
    store.open_ask(&ask, Actor::Hook).unwrap();
    store
        .transition(task.id, &Trigger::AskOpened, Actor::Hook)
        .unwrap();

    let board = store.board(false).unwrap();
    assert_eq!(board.machine, MachineMode::Normal, "mode");
    assert_eq!(board.live_children, 1, "one session takes a slot");
    assert_eq!(board.tasks.len(), 2, "both tasks");
    let sand = board
        .tasks
        .iter()
        .find(|t| t.task.name.to_string() == "me-sim-sand")
        .unwrap();
    assert_eq!(sand.task.state, TaskState::NeedsUser, "waiting on the user");
    assert_eq!(
        sand.session.as_ref().unwrap().state,
        SessionState::Live,
        "session live"
    );
    assert_eq!(sand.asks.first().unwrap().options.len(), 2, "ask options");
    let json = serde_json::to_value(&board).unwrap();
    assert_eq!(
        json["tasks"][0]["name"], "me-sim-sand",
        "JSON names tasks by their full name"
    );
}

#[test]
fn parking_blocks_new_tasks() {
    let (mut store, config) = store();
    store
        .set_machine_mode(MachineMode::Parking, Actor::Root)
        .unwrap();
    let refused = store.create_task(&config, &request("me-sim", "late", "scene"), Actor::Root);
    assert!(
        matches!(
            refused,
            Err(StoreError::NotAdmitted(AdmissionRefusal::MachineMode(
                MachineMode::Parking
            )))
        ),
        "parking refuses spawns"
    );
}

#[test]
fn launcher_records_place_pids_exit_and_worktree() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    store.start_task(&config, task.id, Actor::Tick).unwrap();
    let id = "0b6c1d2e-0000-4000-8000-000000000002";
    let session = NewSession {
        id: id.to_owned(),
        task_id: Some(task.id),
        role: Role::Child,
        rc_name: "me-sim-sand".to_owned(),
        host: "wmux".to_owned(),
        host_workspace: None,
        host_surface: None,
    };
    store.record_session(&session, Actor::User).unwrap();
    store
        .set_session_place(id, "ws-1", "surf-1", Actor::User)
        .unwrap();
    store.session_launched(id, 100, 101, "job").unwrap();
    let (row, owner) = store.session(id).unwrap().unwrap();
    assert_eq!(owner, Some(task.id), "the session's task");
    assert_eq!(row.state, SessionState::Live, "live once launched");
    assert_eq!(
        (row.launcher_pid, row.claude_pid),
        (Some(100), Some(101)),
        "pids"
    );
    assert_eq!(row.host_surface.as_deref(), Some("surf-1"), "surface");

    store.session_ended(id, Some(3)).unwrap();
    let row = store.session_of(task.id).unwrap().unwrap();
    assert_eq!(row.state, SessionState::Ended, "ended");
    assert_eq!(row.exit_code, Some(3), "exit code");

    store
        .record_worktree(task.id, "S:/w/sand", "sand", Actor::User)
        .unwrap();
    store
        .set_worktree_state(task.id, "removed", Actor::User)
        .unwrap();
    let worktree = store.worktree_of(task.id).unwrap().unwrap();
    assert_eq!(
        (worktree.path.as_str(), worktree.state.as_str()),
        ("S:/w/sand", "removed"),
        "worktree"
    );
    let kinds: Vec<String> = store
        .events(EventFilter {
            task: Some(task.id),
            limit: 50,
        })
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    for kind in [
        "session-placed",
        "launched",
        "exited",
        "worktree-recorded",
        "worktree-state",
    ] {
        assert!(kinds.iter().any(|k| k == kind), "{kind} logged");
    }
}

fn hook_world() -> (Store, Task, String) {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    let session = NewSession {
        id: "0b6c1d2e-0000-4000-8000-0000000000aa".to_owned(),
        task_id: Some(task.id),
        role: Role::Child,
        rc_name: "me-sim-sand".to_owned(),
        host: "wmux".to_owned(),
        host_workspace: None,
        host_surface: Some("surf-1".to_owned()),
    };
    store.record_session(&session, Actor::User).unwrap();
    (store, task, session.id)
}

#[test]
fn hooks_see_their_session() {
    let (store, task, id) = hook_world();
    let session = store.hook_session(&id).unwrap().unwrap();
    assert_eq!(session.task_id, Some(task.id), "task");
    assert_eq!(session.role, Role::Child, "role");
    assert_eq!(session.rules_seen, 0, "no rules yet");
    assert_eq!(session.host_surface.as_deref(), Some("surf-1"), "surface");
    assert!(
        store.hook_session("unknown").unwrap().is_none(),
        "unmanaged"
    );
}

#[test]
fn a_changed_rules_hash_bumps_the_version_once() {
    let (mut store, task, id) = hook_world();
    assert_eq!(
        store.observe_rules("aaa", Actor::Hook).unwrap(),
        1,
        "first hash"
    );
    assert_eq!(
        store.observe_rules("aaa", Actor::Hook).unwrap(),
        1,
        "same hash"
    );
    assert_eq!(
        store.observe_rules("bbb", Actor::Hook).unwrap(),
        2,
        "changed"
    );
    let version = store.rules_version().unwrap();
    assert_eq!(version.hash.as_deref(), Some("bbb"), "{version:?}");
    store.set_rules_seen(&id, Some(task.id), 2, 1234).unwrap();
    assert_eq!(
        store.hook_session(&id).unwrap().unwrap().rules_seen,
        2,
        "seen"
    );
    let kinds: Vec<String> = store
        .events(EventFilter {
            task: None,
            limit: 100,
        })
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "rules-changed").count(),
        2,
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "rules-injected"),
        "{kinds:?}"
    );
}

#[test]
fn the_turn_counts_from_the_last_prompt() {
    let (store, task, id) = hook_world();
    let event = |kind: &str, data: serde_json::Value| {
        store
            .append_event(Actor::Hook, Some(task.id), Some(&id), kind, &data)
            .unwrap();
    };
    event(
        "tool-used",
        serde_json::json!({ "tool": "AskUserQuestion" }),
    );
    event(
        "would-deny",
        serde_json::json!({ "hook": "Stop", "rule": "stop-unfinished" }),
    );
    let before = store.turn_so_far(&id).unwrap();
    assert!(before.asked, "asked before any prompt");
    assert_eq!(before.stops_denied, 1, "one stop");
    event("prompt", serde_json::json!({ "cross_session": false }));
    assert_eq!(
        store.turn_so_far(&id).unwrap(),
        headwork_store::TurnSoFar::default(),
        "a new turn"
    );
    event("tool-used", serde_json::json!({ "tool": "SendMessage" }));
    event(
        "deny",
        serde_json::json!({ "hook": "Stop", "rule": "stop-unfinished" }),
    );
    event(
        "deny",
        serde_json::json!({ "hook": "PreToolUse", "rule": "root-git" }),
    );
    let turn = store.turn_so_far(&id).unwrap();
    assert!(turn.messaged && !turn.asked, "{turn:?}");
    assert_eq!(turn.stops_denied, 1, "only Stop denies count");
}

#[test]
fn recent_tool_calls_and_open_asks() {
    let (mut store, task, id) = hook_world();
    assert!(
        !store
            .used_tool_within(&id, "PushNotification", 120)
            .unwrap(),
        "none yet"
    );
    store
        .append_event(
            Actor::Hook,
            Some(task.id),
            Some(&id),
            "tool-used",
            &serde_json::json!({ "tool": "PushNotification" }),
        )
        .unwrap();
    assert!(
        store
            .used_tool_within(&id, "PushNotification", 120)
            .unwrap(),
        "recent push"
    );
    assert!(
        !store.used_tool_within(&id, "SendUserFile", 120).unwrap(),
        "other tool"
    );
    assert!(store.open_ask_of(&id).unwrap().is_none(), "no ask");
    let ask = store
        .open_ask(
            &NewAsk {
                task_id: Some(task.id),
                session_id: Some(id.clone()),
                kind: AskKind::Question,
                question: "Ship?".to_owned(),
                options: vec!["Ship it".to_owned(), "Drop it".to_owned()],
            },
            Actor::Hook,
        )
        .unwrap();
    assert_eq!(store.open_ask_of(&id).unwrap(), Some(ask), "open ask");
    store.answer_ask(ask, "Ship it", Actor::Hook).unwrap();
    assert!(store.open_ask_of(&id).unwrap().is_none(), "answered");
    assert!(!store.fable_allowed(task.id).unwrap(), "no fable ask");
}
