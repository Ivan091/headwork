//! The store against in-memory and on-disk databases, with the config fixtures.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::config::ParkPolicy;
use headwork_core::lifecycle::{CleanupFacts, MergePolicy, MergeSource, SessionFacts, WorktreeEnd};
use headwork_core::process::ProcessIdentity;
use headwork_core::{
    Actor, AdmissionRefusal, AskKind, AskState, Config, Effort, JobState, MachineMode, Model,
    PermissionMode, PlaceKind, ProjectName, RecapKind, Refusal, Role, SessionState, Slug,
    TaskRequest, TaskState, Trigger,
};
use headwork_store::{
    BACKUP_DIR, Backup, EventFilter, FABLE_ALLOW, Launched, MIGRATIONS, NewAsk, NewJob, NewPr,
    NewRecap, NewSession, NewTask, Place, PrSeen, Store, StoreError, Task, TaskId,
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
            effort: None,
            in_main: false,
            keep: false,
            close: headwork_core::CloseMode::Confirm,
            review: None,
            decision: None,
        },
        brief_path: format!("briefs/{project}-{slug}.md"),
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        shape: headwork_core::TaskShape::Plain,
    }
}

fn create(store: &mut Store, config: &Config, project: &str, slug: &str) -> Task {
    store
        .create_task(
            config,
            &request(project, slug, "scene"),
            Actor::Orchestrator,
        )
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

fn old_store(path: &Path, version: usize) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    for sql in &MIGRATIONS[..version] {
        conn.execute_batch(sql).unwrap();
    }
    conn.pragma_update(None, "user_version", i64::try_from(version).unwrap())
        .unwrap();
    conn.execute_batch(
        "INSERT INTO projects (name, main_path, config_hash) VALUES ('me-sim', 'S:/m', 'h')",
    )
    .unwrap();
}

#[test]
fn opening_an_older_store_copies_it_before_migrating() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let from = MIGRATIONS.len() - 1;
    old_store(&path, from);
    let store = Store::open(&path).unwrap();
    let backup = store.backup().cloned();
    assert!(
        matches!(backup, Some(Backup::Made { .. })),
        "a copy is made: {backup:?}"
    );
    let Some(Backup::Made {
        path: copy,
        from: copied,
        to,
    }) = backup
    else {
        return;
    };
    assert_eq!(
        copied,
        i64::try_from(from).unwrap(),
        "the old version is copied"
    );
    assert_eq!(to, store.schema_version().unwrap(), "the store is migrated");
    assert_eq!(
        copy.parent(),
        Some(dir.path().join(BACKUP_DIR).as_path()),
        "beside the store"
    );
    let conn = rusqlite::Connection::open(&copy).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(
        version, copied,
        "the copy holds the schema before the migration"
    );
    let projects: i64 = conn
        .query_row("SELECT count(*) FROM projects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(projects, 1, "the copy holds the data");
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened.backup(),
        None,
        "a current store is not copied again"
    );
}

#[test]
fn a_new_store_is_not_copied() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(store.backup(), None, "nothing to copy");
    assert!(!dir.path().join(BACKUP_DIR).exists(), "no backup folder");
}

#[test]
fn a_failed_copy_does_not_stop_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    old_store(&path, MIGRATIONS.len() - 1);
    std::fs::write(dir.path().join(BACKUP_DIR), "a file where the folder goes").unwrap();
    let store = Store::open(&path).unwrap();
    assert!(
        matches!(store.backup(), Some(Backup::Failed { .. })),
        "the failure is told: {:?}",
        store.backup()
    );
    assert_eq!(
        store.schema_version().unwrap(),
        i64::try_from(MIGRATIONS.len()).unwrap(),
        "the store is migrated anyway"
    );
}

#[test]
fn migration_9_renames_root_and_child_in_a_store_that_holds_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        for sql in &MIGRATIONS[..8] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 8).unwrap();
        conn.execute_batch(
            "INSERT INTO projects (name, main_path, config_hash) VALUES ('me-sim', 'S:/m', 'h');
             INSERT INTO tasks (id, project, slug, kind, state, model, brief_path, queued_at, updated_at)
                 VALUES (1, 'me-sim', 'sand', 'scene', 'working', 'opus', 'b.md', 't', 't');
             INSERT INTO sessions (id, task_id, role, rc_name, runtime, host, state, started_at, host_pane, dir)
                 VALUES ('r', NULL, 'root', 'root', 'pane', 'wmux', 'live', 't', NULL, 'S:/Code'),
                        ('c', 1, 'child', 'me-sim-sand', 'pane', 'wmux', 'live', 't', 'p1', NULL);
             INSERT INTO asks (task_id, session_id, kind, question, options, state, created_at)
                 VALUES (1, 'c', 'question', 'q', '[]', 'open', 't');
             INSERT INTO screen_samples (session_id, hash, at) VALUES ('c', 'x', 't');
             INSERT INTO parks (id, started_at, deadline, root_session) VALUES (1, 't', 't', 'r');
             INSERT INTO rules (scope, body, version, source) VALUES ('child', 'be kind', 1, 'root');
             INSERT INTO events (at, actor, task_id, session_id, type, data) VALUES
                 ('t', 'root', NULL, 'r', 'park-root', '{\"session\": \"r\"}'),
                 ('t', 'hook', 1, 'c', 'would-deny', '{\"rule\": \"child-edit-outside\"}'),
                 ('t', 'user', NULL, NULL, 'rule-test', '{\"role\": \"root\", \"result\": {\"rule\": \"root-git\"}}'),
                 ('t', 'child', 1, 'c', 'note', 'not json');",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    let latest = i64::try_from(MIGRATIONS.len()).unwrap();
    assert_eq!(store.schema_version().unwrap(), latest, "migrated");
    let orchestrator = store.live_orchestrator_session().unwrap().unwrap();
    assert_eq!(
        orchestrator.rc_name, "root",
        "the old orchestrator keeps its name"
    );
    assert_eq!(
        store.park_orchestrator(1).unwrap().as_deref(),
        Some("r"),
        "parks"
    );
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let strings = |sql: &str| -> Vec<String> {
        let mut statement = conn.prepare(sql).unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        strings(
            "SELECT role || ' ' || coalesce(host_pane, '-') || ' ' || coalesce(dir, '-') FROM sessions ORDER BY id"
        ),
        ["worker p1 -", "orchestrator - S:/Code"],
        "roles renamed, other columns kept"
    );
    assert_eq!(
        strings("SELECT actor || ' ' || type || ' ' || data FROM events ORDER BY id"),
        [
            "orchestrator park-orchestrator {\"session\": \"r\"}",
            "hook would-deny {\"rule\":\"worker-edit-outside\"}",
            "user rule-test {\"role\":\"orchestrator\",\"result\":{\"rule\":\"orchestrator-git\"}}",
            "worker note not json",
        ],
        "events renamed"
    );
    assert_eq!(
        strings("SELECT scope || ' ' || source FROM rules"),
        ["worker orchestrator"],
        "rules renamed"
    );
    assert!(
        strings("SELECT \"table\" FROM pragma_foreign_key_check").is_empty(),
        "asks and screen samples still point at their sessions"
    );
}

#[test]
fn migration_10_ends_the_sessions_and_jobs_of_closed_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        for sql in &MIGRATIONS[..9] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 9).unwrap();
        conn.execute_batch(
            "INSERT INTO projects (name, main_path, config_hash) VALUES ('me-sim', 'S:/m', 'h');
             INSERT INTO tasks (id, project, slug, kind, state, model, brief_path, queued_at, updated_at)
                 VALUES (1, 'me-sim', 'old', 'scene', 'closed', 'opus', 'b.md', 't', 't'),
                        (2, 'me-sim', 'new', 'scene', 'working', 'opus', 'b.md', 't', 't');
             INSERT INTO sessions (id, task_id, role, rc_name, runtime, host, state, started_at)
                 VALUES ('a', 1, 'worker', 'me-sim-old', 'pane', 'wmux', 'live', 't'),
                        ('b', 2, 'worker', 'me-sim-new', 'pane', 'wmux', 'live', 't');
             INSERT INTO jobs (id, task_id, project, label, park_policy, argv, cwd, state, log_path, started_at)
                 VALUES (1, 1, 'me-sim', 'old run', 'rerun', '[]', '.', 'parked', 'l', 't'),
                        (2, 2, 'me-sim', 'new run', 'rerun', '[]', '.', 'running', 'l', 't');",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    let latest = i64::try_from(MIGRATIONS.len()).unwrap();
    assert_eq!(store.schema_version().unwrap(), latest, "migrated");
    let state = |id: &str| store.session(id).unwrap().unwrap().0.state;
    assert_eq!(state("a"), SessionState::Ended, "the closed task's session");
    assert_eq!(state("b"), SessionState::Live, "a working task's session");
    assert_eq!(
        store.job(1).unwrap().unwrap().state,
        JobState::Done,
        "the closed task's job"
    );
    assert_eq!(
        store.job(2).unwrap().unwrap().state,
        JobState::Running,
        "a working task's job"
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
        Actor::Orchestrator,
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
        Actor::Orchestrator,
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
    let refused = store.create_task(&config, &fable, Actor::Orchestrator);
    assert!(
        matches!(
            refused,
            Err(StoreError::NotAdmitted(AdmissionRefusal::ModelNotAllowed(
                Model::Fable
            )))
        ),
        "no Fable at work"
    );

    let unknown = store.create_task(&config, &request("nope", "x", "fix"), Actor::Orchestrator);
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
        store
            .transition(task.id, &trigger, Actor::Orchestrator)
            .unwrap();
    }
    let while_closing = store.create_task(
        &config,
        &request("me-sim", "sand", "scene"),
        Actor::Orchestrator,
    );
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
    let first = store
        .create_task(&config, &training, Actor::Orchestrator)
        .unwrap();
    training.request.slug = Slug::new("train-b").unwrap();
    let second = store
        .create_task(&config, &training, Actor::Orchestrator)
        .unwrap();
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
    let scene = store
        .create_task(&config, &scene, Actor::Orchestrator)
        .unwrap();
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
    let task = store
        .create_task(&config, &fable, Actor::Orchestrator)
        .unwrap();
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
        role: Role::Worker,
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
    assert_eq!(board.live_workers, 1, "one session takes a slot");
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
fn a_closing_task_whose_session_ended_frees_its_slot() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    store.start_task(&config, task.id, Actor::Tick).unwrap();
    let session = NewSession {
        id: "0b6c1d2e-0000-4000-8000-0000000000f1".to_owned(),
        task_id: Some(task.id),
        role: Role::Worker,
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
    store
        .transition(task.id, &Trigger::Drop, Actor::Orchestrator)
        .unwrap();
    store
        .transition(task.id, &Trigger::BeginClosing, Actor::Orchestrator)
        .unwrap();
    assert_eq!(
        store.board(false).unwrap().live_workers,
        1,
        "closing with its session live holds the slot"
    );
    store
        .set_session_state(&session.id, SessionState::Ended, Actor::Hook)
        .unwrap();
    assert_eq!(
        store.board(false).unwrap().live_workers,
        0,
        "closing with its session ended frees the slot"
    );
}

#[test]
fn parking_blocks_new_tasks() {
    let (mut store, config) = store();
    store
        .set_machine_mode(MachineMode::Parking, Actor::Orchestrator)
        .unwrap();
    let refused = store.create_task(
        &config,
        &request("me-sim", "late", "scene"),
        Actor::Orchestrator,
    );
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
        role: Role::Worker,
        rc_name: "me-sim-sand".to_owned(),
        host: "wmux".to_owned(),
        host_workspace: None,
        host_surface: None,
    };
    store.record_session(&session, Actor::User).unwrap();
    store
        .set_session_place(
            id,
            &Place {
                workspace: "ws-1",
                surface: "surf-1",
                pane: Some("pane-1"),
                kind: Some(PlaceKind::Pane),
            },
            Actor::User,
        )
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
    assert_eq!(
        store.live_sessions_with_event("build").unwrap(),
        vec![("me-sim-sand".to_owned(), None)],
        "a live session without a build event"
    );
    for stamp in ["1:1", "2:2"] {
        store
            .append_event(
                Actor::Worker,
                Some(task.id),
                Some(id),
                "build",
                &serde_json::json!({ "stamp": stamp }),
            )
            .unwrap();
    }
    assert_eq!(
        store.live_sessions_with_event("build").unwrap(),
        vec![(
            "me-sim-sand".to_owned(),
            Some(serde_json::json!({ "stamp": "2:2" }))
        )],
        "the newest build event"
    );

    store.session_ended(id, Some(3)).unwrap();
    assert!(
        store.live_sessions_with_event("build").unwrap().is_empty(),
        "ended sessions are not live"
    );
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

#[test]
fn an_ended_session_carries_its_tasks_folders() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    store.start_task(&config, task.id, Actor::Tick).unwrap();
    let id = "0b6c1d2e-0000-4000-8000-000000000003";
    let session = NewSession {
        id: id.to_owned(),
        task_id: Some(task.id),
        role: Role::Worker,
        rc_name: "me-sim-sand".to_owned(),
        host: "wmux".to_owned(),
        host_workspace: None,
        host_surface: None,
    };
    store.record_session(&session, Actor::User).unwrap();
    store
        .record_worktree(task.id, "S:/w/sand", "sand", Actor::User)
        .unwrap();
    store.session_launched(id, 100, 101, "job").unwrap();
    store.session_ended(id, Some(0)).unwrap();
    let ended = store.ended_sessions().unwrap();
    assert_eq!(
        ended
            .iter()
            .map(|s| (s.name.as_str(), s.folders.clone()))
            .collect::<Vec<_>>(),
        [("me-sim-sand", vec!["S:/w/sand".to_owned()])],
        "the task's worktree"
    );
}

#[test]
fn an_orchestrator_session_is_found_by_id_in_any_state_and_a_workers_is_not() {
    let (mut store, _, worker) = hook_world();
    let id = "0b6c1d2e-0000-4000-8000-0000000000bb";
    store
        .record_session(
            &NewSession {
                id: id.to_owned(),
                task_id: None,
                role: Role::Orchestrator,
                rc_name: "orchestrator".to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
    store.session_launched(id, 100, 101, "job").unwrap();
    store.session_ended(id, Some(0)).unwrap();
    assert!(
        store.live_orchestrator_session().unwrap().is_none(),
        "it ended"
    );
    assert_eq!(
        store
            .orchestrator_session(id)
            .unwrap()
            .map(|s| s.id)
            .as_deref(),
        Some(id),
        "found though it ended"
    );
    assert!(
        store.orchestrator_session(&worker).unwrap().is_none(),
        "a worker's session"
    );
    assert!(
        store.orchestrator_session("unknown").unwrap().is_none(),
        "an unknown id"
    );
}

fn hook_world() -> (Store, Task, String) {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    let session = NewSession {
        id: "0b6c1d2e-0000-4000-8000-0000000000aa".to_owned(),
        task_id: Some(task.id),
        role: Role::Worker,
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
    assert_eq!(session.role, Role::Worker, "role");
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
fn background_work_stays_open_until_each_notification_arrives() {
    let (store, task, id) = hook_world();
    let event = |kind: &str, data: serde_json::Value| {
        store
            .append_event(Actor::Hook, Some(task.id), Some(&id), kind, &data)
            .unwrap();
    };
    event("prompt", serde_json::json!({ "notification": false }));
    let started = serde_json::json!({ "tool": "Agent", "background": true });
    event("tool-used", started.clone());
    event("tool-used", started);
    assert!(store.turn_so_far(&id).unwrap().background, "two running");
    event("prompt", serde_json::json!({ "notification": true }));
    assert!(
        store.turn_so_far(&id).unwrap().background,
        "one notification leaves one running"
    );
    event("prompt", serde_json::json!({ "notification": true }));
    assert!(!store.turn_so_far(&id).unwrap().background, "both reported");
    event("prompt", serde_json::json!({ "notification": false }));
    event(
        "tool-used",
        serde_json::json!({ "tool": "Agent", "background": true }),
    );
    event("prompt", serde_json::json!({ "notification": true }));
    event("prompt", serde_json::json!({ "notification": true }));
    assert!(
        !store.turn_so_far(&id).unwrap().background,
        "a notification of an older turn never reopens work"
    );
}

#[test]
fn a_notification_is_no_user_prompt() {
    let (store, task, id) = hook_world();
    store
        .append_event(
            Actor::Hook,
            Some(task.id),
            Some(&id),
            "prompt",
            &serde_json::json!({ "cross_session": false, "notification": true }),
        )
        .unwrap();
    assert!(!store.user_prompted_within(&id, 120).unwrap());
    store
        .append_event(
            Actor::Hook,
            Some(task.id),
            Some(&id),
            "prompt",
            &serde_json::json!({ "cross_session": false, "notification": false }),
        )
        .unwrap();
    assert!(store.user_prompted_within(&id, 120).unwrap());
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

#[test]
fn a_park_collects_the_recap_a_session_writes_during_it() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    let before = NewRecap {
        task_id: Some(task.id),
        session_id: None,
        kind: RecapKind::Session,
        body: "day to day",
    };
    store.add_recap(&before, Actor::Worker).unwrap();
    let park = store.begin_park(300, Actor::User).unwrap();
    assert_eq!(
        store.machine_mode().unwrap(),
        MachineMode::Parking,
        "parking"
    );
    assert!(
        store
            .session_park_recap(park.id, task.id)
            .unwrap()
            .is_none(),
        "none yet"
    );
    assert!(
        !store.park_deadline_passed(park.id).unwrap(),
        "deadline ahead"
    );
    store
        .add_recap(
            &NewRecap {
                body: "at the park",
                ..before
            },
            Actor::Worker,
        )
        .unwrap();
    let found = store.session_park_recap(park.id, task.id).unwrap().unwrap();
    assert_eq!(found.body, "at the park", "attached to the park");
    assert_eq!(
        store.latest_recap(task.id).unwrap().unwrap().id,
        found.id,
        "newest"
    );
    store.finish_park(park.id, Actor::User).unwrap();
    assert_eq!(store.machine_mode().unwrap(), MachineMode::Parked, "parked");
    store
        .finish_resume(Some(park.id), "recaps/park-1.md", Actor::User)
        .unwrap();
    let done = store.park(park.id).unwrap().unwrap();
    assert!(
        done.finished_at.is_some() && done.resumed_at.is_some(),
        "{done:?}"
    );
    assert_eq!(
        store.machine_mode().unwrap(),
        MachineMode::Normal,
        "normal again"
    );
}

#[test]
fn a_park_adopts_only_the_recap_the_orchestrator_wrote_in_its_latest_turn() {
    let (mut store, _) = store();
    let session = "orchestrator-1";
    store
        .record_session(
            &NewSession {
                id: session.to_owned(),
                task_id: None,
                role: Role::Orchestrator,
                rc_name: "orchestrator".to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
    let recap = |body| NewRecap {
        task_id: None,
        session_id: Some(session),
        kind: RecapKind::Session,
        body,
    };
    let prompt = |store: &mut Store| {
        store
            .append_event(
                Actor::Hook,
                None,
                Some(session),
                "prompt",
                &serde_json::json!({}),
            )
            .unwrap();
    };
    store
        .add_recap(&recap("before any prompt"), Actor::Orchestrator)
        .unwrap();
    let first = store.begin_park(300, Actor::User).unwrap();
    assert_eq!(
        store
            .adopt_orchestrator_recap(first.id, session, Actor::User)
            .unwrap(),
        None,
        "no turn of its own is known, so nothing is adopted"
    );
    store.finish_park(first.id, Actor::User).unwrap();
    store
        .finish_resume(Some(first.id), "recaps/park-1.md", Actor::User)
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));
    prompt(&mut store);
    let id = store
        .add_recap(&recap("this turn"), Actor::Orchestrator)
        .unwrap();
    let second = store.begin_park(300, Actor::User).unwrap();
    assert_eq!(
        store
            .adopt_orchestrator_recap(second.id, session, Actor::User)
            .unwrap(),
        Some(id),
        "the recap of the turn that started the park"
    );
    let found = store
        .orchestrator_park_recap(second.id, session)
        .unwrap()
        .unwrap();
    assert_eq!(found.body, "this turn", "attached to the park");
    assert_eq!(
        store
            .adopt_orchestrator_recap(second.id, session, Actor::User)
            .unwrap(),
        None,
        "a recap already in a park is not adopted again"
    );
}

#[test]
fn a_session_records_the_surface_it_runs_in() {
    let (mut store, _) = store();
    store
        .record_session(
            &NewSession {
                id: "orchestrator-1".to_owned(),
                task_id: None,
                role: Role::Orchestrator,
                rc_name: "orchestrator".to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
    store
        .set_session_surface("orchestrator-1", "surf-1", Actor::User)
        .unwrap();
    let (row, _) = store.session("orchestrator-1").unwrap().unwrap();
    assert_eq!(row.host_surface.as_deref(), Some("surf-1"), "recorded");
    assert_eq!(row.host_workspace, None, "the workspace is left alone");
}

#[test]
fn jobs_keep_their_command_line_and_move_through_states() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    let argv = vec![
        "bash".to_owned(),
        "scripts/rl-run.sh".to_owned(),
        "--stage 2".to_owned(),
    ];
    let id = store
        .register_job(
            &NewJob {
                task_id: Some(task.id),
                session_id: None,
                project: "me-sim",
                label: "car-training",
                park_policy: ParkPolicy::Checkpoint,
                argv: &argv,
                cwd: "S:/Code/me-sim",
                pid: Some(4242),
                identity: Some(&ProcessIdentity {
                    image: "bash.exe".to_owned(),
                    started: 1_700_000_000,
                }),
                checkpoint_path: Some("out/checkpoint.json"),
                gate: Some(headwork_core::gate::GateClass::Cpu),
                restart: true,
                log_path: "jobs/car.log",
            },
            Actor::Worker,
        )
        .unwrap();
    let job = store.job(id).unwrap().unwrap();
    assert_eq!(job.argv, argv, "argv round trip");
    assert_eq!(job.park_policy, ParkPolicy::Checkpoint, "policy");
    assert_eq!(
        job.identity
            .as_ref()
            .map(|id| (id.image.as_str(), id.started)),
        Some(("bash.exe", 1_700_000_000)),
        "identity round trip"
    );
    assert_eq!(store.open_jobs(Some(task.id)).unwrap().len(), 1, "open");
    store
        .set_job_state(id, JobState::Parked, Some("checkpointed"), Actor::User)
        .unwrap();
    assert_eq!(
        store.jobs_in(JobState::Parked, None).unwrap().len(),
        1,
        "parked"
    );
    store.set_job_pid(id, 4343, None, Actor::User).unwrap();
    let job = store.job(id).unwrap().unwrap();
    assert_eq!(
        (job.state, job.pid),
        (JobState::Running, Some(4343)),
        "restarted"
    );
    assert_eq!(job.identity, None, "a restart replaces the identity");
    assert_eq!(
        job.note, None,
        "the note of the stop is cleared by the restart"
    );
    store
        .set_job_state(id, JobState::Done, None, Actor::User)
        .unwrap();
    assert!(store.open_jobs(None).unwrap().is_empty(), "over");
}

#[test]
fn forgetting_a_job_ends_it_once_and_logs_why() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    let argv = vec!["bash".to_owned()];
    let id = store
        .register_job(
            &NewJob {
                task_id: Some(task.id),
                session_id: None,
                project: "me-sim",
                label: "sac32",
                park_policy: ParkPolicy::Checkpoint,
                argv: &argv,
                cwd: "S:/Code/me-sim",
                pid: Some(38260),
                identity: None,
                checkpoint_path: Some("out/checkpoint.json"),
                gate: Some(headwork_core::gate::GateClass::Cpu),
                restart: true,
                log_path: "jobs/car.log",
            },
            Actor::Worker,
        )
        .unwrap();
    assert!(
        store
            .forget_job(id, "pid reused", Actor::Orchestrator)
            .unwrap(),
        "forgotten"
    );
    let job = store.job(id).unwrap().unwrap();
    assert_eq!(
        (job.state, job.note.as_deref()),
        (JobState::Done, Some("forgotten: pid reused")),
        "over with the reason"
    );
    assert!(store.open_jobs(None).unwrap().is_empty(), "no longer open");
    assert!(
        !store.forget_job(id, "again", Actor::User).unwrap(),
        "already over"
    );
    let events = store
        .events(EventFilter {
            task: Some(task.id),
            limit: 50,
        })
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "job-forgotten")
        .count();
    assert_eq!(events, 1, "logged once");
}

#[test]
fn a_task_keeps_its_effort_and_none_shows_a_question_mark() {
    let (mut store, config) = store();
    let plain = create(&mut store, &config, "me-sim", "plain");
    assert_eq!(plain.effort, None, "no effort by default");
    assert_eq!(plain.effort_label(), "?", "label");
    let mut new = request("me-sim", "deep", "scene");
    new.request.effort = Some(Effort::High);
    let deep = store
        .create_task(&config, &new, Actor::Orchestrator)
        .unwrap();
    assert_eq!(deep.effort, Some(Effort::High), "stored");
    assert_eq!(deep.effort_label(), "high", "label");
    let loaded = store.find_task("me-sim-deep").unwrap().unwrap();
    assert_eq!(loaded.effort, Some(Effort::High), "reloaded");
    let events = store
        .events(EventFilter {
            task: Some(deep.id),
            limit: 5,
        })
        .unwrap();
    assert_eq!(
        events
            .first()
            .and_then(|e| e.data.get("effort"))
            .and_then(|v| v.as_str()),
        Some("high"),
        "event"
    );
}

#[test]
fn a_placed_session_records_its_pane_and_holds_its_surface_while_live() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    store.start_task(&config, task.id, Actor::Tick).unwrap();
    let id = "0b6c1d2e-0000-4000-8000-000000000003";
    let session = NewSession {
        id: id.to_owned(),
        task_id: Some(task.id),
        role: Role::Worker,
        rc_name: "me-sim-sand".to_owned(),
        host: "wmux".to_owned(),
        host_workspace: None,
        host_surface: None,
    };
    store.record_session(&session, Actor::User).unwrap();
    assert!(
        store.live_session_surfaces().unwrap().is_empty(),
        "not placed yet"
    );
    let place = Place {
        workspace: "ws-1",
        surface: "surf-1",
        pane: Some("pane-1"),
        kind: Some(PlaceKind::Pane),
    };
    store.set_session_place(id, &place, Actor::User).unwrap();
    assert_eq!(
        store.live_session_surfaces().unwrap(),
        ["surf-1"],
        "a starting session holds its surface"
    );
    let row = store.session_of(task.id).unwrap().unwrap();
    assert_eq!(
        (row.host_pane.as_deref(), row.host_place),
        (Some("pane-1"), Some(PlaceKind::Pane)),
        "pane"
    );
    store.session_ended(id, Some(0)).unwrap();
    assert!(
        store.live_session_surfaces().unwrap().is_empty(),
        "an ended session frees its surface"
    );
}

#[test]
fn concurrent_writers_wait_for_each_other_instead_of_failing() {
    const WRITERS: usize = 8;
    const ROUNDS: usize = 40;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let start = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
    let writers: Vec<_> = (0..WRITERS)
        .map(|writer| {
            let path = path.clone();
            let start = std::sync::Arc::clone(&start);
            std::thread::spawn(move || -> Result<(), StoreError> {
                start.wait();
                let mut store = Store::open(&path)?;
                for round in 0..ROUNDS {
                    let mode = if (writer + round) % 2 == 0 {
                        MachineMode::Parking
                    } else {
                        MachineMode::Normal
                    };
                    store.set_machine_mode(mode, Actor::Orchestrator)?;
                }
                Ok(())
            })
        })
        .collect();
    for writer in writers {
        let outcome = writer.join().unwrap();
        assert!(outcome.is_ok(), "a writer failed: {outcome:?}");
    }
    let store = Store::open(&path).unwrap();
    let changes = store
        .events(EventFilter {
            task: None,
            limit: 10_000,
        })
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "machine-mode")
        .count();
    assert_eq!(changes, WRITERS * ROUNDS, "every write landed");
}

fn shaped(
    store: &mut Store,
    config: &Config,
    project: &str,
    slug: &str,
    shape: headwork_core::TaskShape<TaskId>,
) -> Result<Task, StoreError> {
    let new = NewTask {
        shape,
        ..request(project, slug, "scene")
    };
    store.create_task(config, &new, Actor::Orchestrator)
}

fn config_with_leads(leads: &str) -> Config {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    let text = std::fs::read_to_string(path).unwrap();
    Config::parse(
        &format!(
            "{text}
[leads]
enabled = true
{leads}"
        ),
        "config.toml",
    )
    .unwrap()
}

fn lead_in(store: &mut Store, config: &Config, project: &str, slug: &str) -> Task {
    let lead = shaped(store, config, project, slug, headwork_core::TaskShape::Lead).unwrap();
    store.start_task(config, lead.id, Actor::Tick).unwrap();
    store
        .transition(lead.id, &Trigger::SessionStarted(LIVE), Actor::Hook)
        .unwrap();
    lead
}

fn part_in(store: &mut Store, config: &Config, project: &str, lead: &Task, slug: &str) -> Task {
    shaped(
        store,
        config,
        project,
        slug,
        headwork_core::TaskShape::Part { parent: lead.id },
    )
    .unwrap()
}

fn lead_with_parts(store: &mut Store, config: &Config, parts: &[&str]) -> (Task, Vec<Task>) {
    let lead = shaped(
        store,
        config,
        "me-sim",
        "big",
        headwork_core::TaskShape::Lead,
    )
    .unwrap();
    store.start_task(config, lead.id, Actor::Tick).unwrap();
    store
        .transition(lead.id, &Trigger::SessionStarted(LIVE), Actor::Hook)
        .unwrap();
    let parts = parts
        .iter()
        .map(|part| {
            shaped(
                store,
                config,
                "me-sim",
                &format!("big-{part}"),
                headwork_core::TaskShape::Part { parent: lead.id },
            )
            .unwrap()
        })
        .collect();
    (lead, parts)
}

#[test]
fn migration_13_leaves_every_existing_task_plain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        for sql in &MIGRATIONS[..12] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 12).unwrap();
        conn.execute_batch(
            "INSERT INTO projects (name, main_path, config_hash) VALUES ('me-sim', 'S:/m', 'h');
             INSERT INTO tasks (id, project, slug, kind, state, model, brief_path, queued_at, updated_at)
                 VALUES (1, 'me-sim', 'sand', 'scene', 'working', 'opus', 'b.md', 't', 't'),
                        (2, 'me-sim', 'dune', 'scene', 'queued', 'opus', 'b.md', 't', 't');",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.schema_version().unwrap(),
        i64::try_from(MIGRATIONS.len()).unwrap(),
        "migrated"
    );
    for name in ["me-sim-sand", "me-sim-dune"] {
        let task = store.find_task(name).unwrap().unwrap();
        assert_eq!(task.shape, headwork_core::TaskShape::Plain, "{name}");
        assert_eq!(store.live_parts(task.id).unwrap(), 0, "{name} has no parts");
    }
}

fn on_disk(dir: &tempfile::TempDir) -> (Store, Config, rusqlite::Connection) {
    let config = config();
    let path = dir.path().join("state.db");
    let mut store = Store::open(&path).unwrap();
    store.sync_projects(&config).unwrap();
    (store, config, rusqlite::Connection::open(&path).unwrap())
}

#[test]
fn a_part_that_is_also_a_lead_cannot_be_read() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, config, raw) = on_disk(&dir);
    let (_, parts) = lead_with_parts(&mut store, &config, &["review"]);
    raw.execute_batch("UPDATE tasks SET lead = 1 WHERE slug = 'big-review'")
        .unwrap();
    let error = store.task(parts[0].id).unwrap_err();
    assert!(matches!(error, StoreError::BadShape(_)), "{error}");
}

#[test]
fn a_part_goes_only_under_a_live_lead_of_its_own_project() {
    let (mut store, config) = store();
    let plain = create(&mut store, &config, "me-sim", "plain");
    let error = shaped(
        &mut store,
        &config,
        "me-sim",
        "plain-review",
        headwork_core::TaskShape::Part { parent: plain.id },
    )
    .unwrap_err();
    assert!(
        matches!(error, StoreError::NotALead { .. }),
        "a plain task: {error}"
    );
    let (lead, parts) = lead_with_parts(&mut store, &config, &["review"]);
    assert_eq!(
        parts[0].shape,
        headwork_core::TaskShape::Part { parent: lead.id },
        "stored as a part"
    );
    let error = shaped(
        &mut store,
        &config,
        "me-studio",
        "big-render",
        headwork_core::TaskShape::Part { parent: lead.id },
    )
    .unwrap_err();
    assert!(
        matches!(error, StoreError::NotALead { .. }),
        "another project: {error}"
    );
    let error = shaped(
        &mut store,
        &config,
        "me-sim",
        "big-review-nested",
        headwork_core::TaskShape::Part {
            parent: parts[0].id,
        },
    )
    .unwrap_err();
    assert!(
        matches!(error, StoreError::NotALead { .. }),
        "parts start no parts: {error}"
    );
    store.drop_task(lead.id, Actor::Orchestrator).unwrap();
    let error = shaped(
        &mut store,
        &config,
        "me-sim",
        "big-late",
        headwork_core::TaskShape::Part { parent: lead.id },
    )
    .unwrap_err();
    assert!(
        matches!(error, StoreError::NotALead { .. }),
        "a dropped lead: {error}"
    );
}

#[test]
fn a_part_beyond_max_parts_waits_and_starts_when_a_sibling_closes() {
    let (mut store, _) = store();
    let config = config_with_leads(
        "max_parts = 2
",
    );
    store.sync_projects(&config).unwrap();
    let (_, parts) = lead_with_parts(&mut store, &config, &["a", "b", "c"]);
    store.start_task(&config, parts[0].id, Actor::Tick).unwrap();
    store.start_task(&config, parts[1].id, Actor::Tick).unwrap();
    let error = store
        .start_task(&config, parts[2].id, Actor::Tick)
        .unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::Refused {
                refusal: Refusal::PartCapReached { max: 2 },
                ..
            }
        ),
        "{error}"
    );
    let plain = create(&mut store, &config, "me-sim", "other");
    store.start_task(&config, plain.id, Actor::Tick).unwrap();
    store
        .transition(parts[0].id, &Trigger::LaunchFailed, Actor::Tick)
        .unwrap();
    store.start_task(&config, parts[2].id, Actor::Tick).unwrap();
}

#[test]
fn a_waiting_part_queues_right_behind_its_lead() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, config, raw) = on_disk(&dir);
    let early = create(&mut store, &config, "me-sim", "early");
    let (lead, parts) = lead_with_parts(&mut store, &config, &["a"]);
    let later = create(&mut store, &config, "me-sim", "later");
    raw.execute_batch(&format!(
            "UPDATE tasks SET queued_at = CASE id WHEN {} THEN '2026-01-01T00:00:00Z' WHEN {} THEN '2026-01-02T00:00:00Z'
                 WHEN {} THEN '2026-01-04T00:00:00Z' WHEN {} THEN '2026-01-03T00:00:00Z' END",
            early.id, lead.id, parts[0].id, later.id
        ))
        .unwrap();
    assert_eq!(
        store.queued().unwrap(),
        vec![early.id, parts[0].id, later.id],
        "the part queued after `later`, but waits at its lead's place"
    );
}

#[test]
fn a_lead_counts_its_parts_until_they_are_closed() {
    let (mut store, config) = store();
    let (lead, parts) = lead_with_parts(&mut store, &config, &["a", "b"]);
    assert_eq!(store.live_parts(lead.id).unwrap(), 2, "two queued parts");
    assert_eq!(
        store.parts(lead.id).unwrap(),
        parts.iter().map(|part| part.id).collect::<Vec<_>>(),
        "oldest first"
    );
    for trigger in [Trigger::Drop, Trigger::BeginClosing] {
        store
            .transition(parts[0].id, &trigger, Actor::Orchestrator)
            .unwrap();
    }
    assert_eq!(
        store.live_parts(lead.id).unwrap(),
        2,
        "closing still counts"
    );
    store
        .transition(
            parts[0].id,
            &Trigger::Cleaned(CleanupFacts {
                pane_gone: true,
                processes_gone: true,
                worktree: WorktreeEnd::Removed,
            }),
            Actor::Tick,
        )
        .unwrap();
    assert_eq!(
        store.live_parts(lead.id).unwrap(),
        1,
        "a closed part no longer counts"
    );
}

#[test]
fn dropping_a_lead_drops_its_live_parts_and_a_part_drops_alone() {
    let (mut store, config) = store();
    let (lead, parts) = lead_with_parts(&mut store, &config, &["a", "b", "c"]);
    let dropped = store.drop_task(parts[2].id, Actor::Orchestrator).unwrap();
    assert!(dropped.is_empty(), "a part has no parts");
    assert_eq!(
        store.task(lead.id).unwrap().state,
        TaskState::Working,
        "the lead stays"
    );
    store
        .transition(parts[2].id, &Trigger::BeginClosing, Actor::Orchestrator)
        .unwrap();
    let dropped = store.drop_task(lead.id, Actor::Orchestrator).unwrap();
    assert_eq!(
        dropped,
        vec![parts[0].id, parts[1].id],
        "the live parts, not the closing one"
    );
    for id in [lead.id, parts[0].id, parts[1].id] {
        assert_eq!(
            store.task(id).unwrap().state,
            TaskState::Dropped,
            "{id} dropped"
        );
    }
    assert_eq!(
        store.task(parts[2].id).unwrap().state,
        TaskState::Closing,
        "left closing"
    );
    let error = store.drop_task(lead.id, Actor::Orchestrator).unwrap_err();
    assert!(
        matches!(error, StoreError::Refused { .. }),
        "twice: {error}"
    );
}

#[test]
fn a_launched_session_records_both_identities_and_a_resume_clears_them() {
    let (mut store, _, id) = hook_world();
    let launcher = ProcessIdentity {
        image: "headwork.exe".to_owned(),
        started: 1_000,
    };
    let claude = ProcessIdentity {
        image: "claude.exe".to_owned(),
        started: 1_001,
    };
    store
        .session_launched_identified(
            &id,
            Launched {
                pid: 100,
                identity: Some(&launcher),
            },
            Launched {
                pid: 101,
                identity: Some(&claude),
            },
            "job",
        )
        .unwrap();
    let (row, _) = store.session(&id).unwrap().unwrap();
    assert_eq!(
        row.launcher().map(|p| (p.pid, p.identity)),
        Some((100, Some(launcher))),
        "launcher"
    );
    assert_eq!(
        row.claude().map(|p| (p.pid, p.identity)),
        Some((101, Some(claude))),
        "claude"
    );
    assert!(row.pids_at.is_some_and(|at| at > 1_700_000_000), "{row:?}");
    assert_eq!(
        row.processes().iter().map(|p| p.pid).collect::<Vec<_>>(),
        [101, 100],
        "claude first"
    );
    store.session_resumed(&id, Actor::User).unwrap();
    let (row, _) = store.session(&id).unwrap().unwrap();
    assert!(row.processes().is_empty(), "no pids after a resume");
    assert_eq!(
        (row.launcher_identity, row.claude_identity, row.pids_at),
        (None, None, None),
        "nor identities"
    );
    store.session_launched(&id, 200, 201, "job").unwrap();
    let (row, _) = store.session(&id).unwrap().unwrap();
    assert_eq!(
        row.claude()
            .map(|p| (p.pid, p.identity, p.recorded_at.is_some())),
        Some((201, None, true)),
        "a launch without identities still records when"
    );
}

#[test]
fn a_session_found_gone_or_ended_records_its_end_once() {
    for state in [SessionState::Gone, SessionState::Ended] {
        let (mut store, _, id) = hook_world();
        store.session_launched(&id, 100, 101, "job").unwrap();
        let ended_at = |store: &Store| {
            store
                .ended_sessions()
                .unwrap()
                .first()
                .and_then(|s| s.ended)
        };
        store.set_session_state(&id, state, Actor::Tick).unwrap();
        let first = ended_at(&store);
        assert!(first.is_some(), "{state}: an end is recorded");
        let seen = store.ended_sessions().unwrap()[0].last_seen;
        assert!(seen.is_some(), "{state}: the last event is read");
        store
            .set_session_state(&id, SessionState::Gone, Actor::Tick)
            .unwrap();
        assert_eq!(ended_at(&store), first, "{state}: the end is kept");
    }
    let (mut store, _, id) = hook_world();
    store.session_launched(&id, 100, 101, "job").unwrap();
    store
        .set_session_state(&id, SessionState::Live, Actor::Tick)
        .unwrap();
    assert!(store.ended_sessions().unwrap().is_empty(), "not ended");
}

#[test]
fn migration_15_dates_recorded_pids_and_backfills_the_end_of_ended_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    old_store(&path, 14);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "INSERT INTO sessions (id, role, rc_name, runtime, host, launcher_pid, claude_pid, state, started_at, last_event_at)
         VALUES ('launched', 'worker', 'me-sim-a', 'pane', 'wmux', 10, 11, 'live', '2026-11-20T10:00:00.000Z',
                 '2026-11-20T11:00:00.000Z'),
                ('imported', 'worker', 'me-sim-b', 'pane', 'wmux', NULL, 12, 'live', '2026-11-20T09:00:00.000Z', NULL),
                ('gone', 'worker', 'me-sim-c', 'pane', 'wmux', 13, 14, 'gone', '2026-11-20T09:00:00.000Z',
                 '2026-11-20T13:00:00.000Z'),
                ('quiet', 'worker', 'me-sim-d', 'pane', 'wmux', NULL, NULL, 'ended', '2026-11-20T09:00:00.000Z', NULL);
         INSERT INTO events (at, actor, session_id, type, data) VALUES
                ('2026-11-20T10:00:05.000Z', 'worker', 'launched', 'launched', '{}'),
                ('2026-11-20T10:30:00.000Z', 'hook', 'launched', 'tool', '{}'),
                ('2026-11-20T12:00:00.000Z', 'tick', 'gone', 'session-state', '{}');",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path).unwrap();
    let pids_at = |id: &str| store.session(id).unwrap().unwrap().0.pids_at;
    assert_eq!(
        pids_at("launched"),
        Some(1_795_168_805),
        "from the launch event"
    );
    assert_eq!(pids_at("imported"), Some(1_795_165_200), "from the start");
    assert_eq!(pids_at("quiet"), None, "no pids, no date");
    let ended: Vec<(String, Option<u64>)> = store
        .ended_sessions()
        .unwrap()
        .into_iter()
        .map(|s| (s.name, s.ended))
        .collect();
    assert_eq!(
        ended,
        [
            ("me-sim-c".to_owned(), Some(1_795_176_000)),
            ("me-sim-d".to_owned(), Some(1_795_165_200)),
        ],
        "the last event, else the start"
    );
}

const PR_ROWS_BY_TASK: &str = include_str!("../migrations/0017_pr_rows_by_task.sql");

fn new_pr(repo: &str, number: i64) -> NewPr {
    NewPr {
        repo: repo.to_owned(),
        number,
        url: format!("https://github.com/{repo}/pull/{number}"),
    }
}

fn seen<'a>(pr: &'a NewPr, state: &'a str) -> PrSeen<'a> {
    PrSeen {
        repo: &pr.repo,
        number: pr.number,
        url: &pr.url,
        state,
    }
}

fn pr_states(store: &Store, task: &Task) -> Vec<(String, String)> {
    store
        .recorded_prs(task.id)
        .unwrap()
        .into_iter()
        .map(|pr| (pr.url, pr.state))
        .collect()
}

#[test]
fn two_tasks_that_name_one_pull_request_each_keep_a_row_of_their_own() {
    let (mut store, config) = store();
    let old = create(&mut store, &config, "me-sim", "old");
    let new = create(&mut store, &config, "me-sim", "new");
    let pr = new_pr("o/r", 71);
    store
        .record_report(old.id, "s1", "review", "first", &[], Some(&pr))
        .unwrap();
    store
        .record_pr(old.id, &seen(&pr, "merged"), Actor::Tick)
        .unwrap();
    store
        .record_report(new.id, "s2", "review", "reused", &[], Some(&pr))
        .unwrap();
    store
        .record_pr(new.id, &seen(&pr, "closed"), Actor::Tick)
        .unwrap();
    assert_eq!(
        pr_states(&store, &old),
        [(pr.url.clone(), "merged".to_owned())],
        "the first task's row is untouched"
    );
    assert_eq!(
        pr_states(&store, &new),
        [(pr.url.clone(), "closed".to_owned())],
        "the second task has its own row"
    );
}

#[test]
fn reporting_a_merged_pull_request_again_keeps_it_merged() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    let pr = new_pr("o/r", 3);
    store
        .record_report(task.id, "s", "review", "opened", &[], Some(&pr))
        .unwrap();
    assert_eq!(
        pr_states(&store, &task),
        [(pr.url.clone(), "open".to_owned())],
        "a new report records the pull request open"
    );
    store
        .record_pr(task.id, &seen(&pr, "merged"), Actor::Tick)
        .unwrap();
    store
        .record_report(task.id, "s", "review", "again", &[], Some(&pr))
        .unwrap();
    assert_eq!(
        pr_states(&store, &task),
        [(pr.url.clone(), "merged".to_owned())],
        "the report does not take it back to open"
    );
}

type PrRowFields = (String, i64, String, String);

fn pr_rows(store: &Store, task: TaskId) -> Vec<PrRowFields> {
    store
        .recorded_prs(task)
        .unwrap()
        .into_iter()
        .map(|pr| (pr.repo, pr.number, pr.url, pr.state))
        .collect()
}

fn pr_row(repo: &str, number: i64, state: &str) -> PrRowFields {
    (
        repo.to_owned(),
        number,
        format!("https://github.com/{repo}/pull/{number}"),
        state.to_owned(),
    )
}

#[test]
fn the_pr_rows_migration_moves_rows_named_before_the_re_creation_to_the_archive() {
    let index = MIGRATIONS
        .iter()
        .position(|sql| *sql == PR_ROWS_BY_TASK)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        for sql in &MIGRATIONS[..index] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", i64::try_from(index).unwrap())
            .unwrap();
        conn.execute_batch(
            r#"INSERT INTO projects (name, main_path, config_hash) VALUES ('me-sim', 'S:/m', 'h');
             INSERT INTO tasks (id, project, slug, kind, state, model, brief_path, queued_at, updated_at)
                 VALUES (1, 'me-sim', 'old', 'scene', 'closed', 'opus', 'b.md', '2026-11-22T08:11:05.502Z', 't'),
                        (2, 'me-sim', 'new', 'scene', 'working', 'opus', 'b.md', '2026-11-22T09:00:00.000Z', 't'),
                        (3, 'me-sim', 'quiet', 'scene', 'closed', 'opus', 'b.md', '2026-11-20T10:00:00.000Z', 't');
             INSERT INTO prs (id, task_id, repo, number, url, state, checked_at) VALUES
                 (1, 1, 'Ivan091/headwork', 71, 'https://github.com/Ivan091/headwork/pull/71', 'open', NULL),
                 (2, 1, 'Ivan091/headwork', 1, 'https://github.com/Ivan091/headwork/pull/1', 'merged', 't'),
                 (3, 2, 'Ivan091/headwork', 7, 'https://github.com/Ivan091/headwork/pull/7', 'open', NULL),
                 (4, 1, 'o/r', 72, 'https://github.com/o/r/pull/72', 'merged', 't'),
                 (5, 3, 'Ivan091/headwork', 40, 'https://github.com/Ivan091/headwork/pull/40', 'merged', 't');
             INSERT INTO events (at, actor, task_id, session_id, type, data) VALUES
                 ('2026-11-22T08:22:17.160Z', 'worker', 1, 's', 'report', '{"pr":"https://github.com/Ivan091/headwork/pull/71"}'),
                 ('2026-11-22T08:30:00.000Z', 'worker', 1, 's', 'report', '{"pr":"https://github.com/o/r/pull/72"}'),
                 ('2026-11-22T08:52:31.699Z', 'worker', 1, 's', 'report', '{"pr":"https://github.com/Ivan091/headwork/pull/1"}'),
                 ('2026-11-22T09:10:00.000Z', 'worker', 2, 's', 'report', '{"pr":"https://github.com/Ivan091/headwork/pull/7"}');"#,
        )
        .unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    let (old, new, quiet) = (TaskId(1), TaskId(2), TaskId(3));
    assert_eq!(
        pr_rows(&store, old),
        [
            pr_row("Ivan091/headwork-archive", 71, "open"),
            pr_row("Ivan091/headwork", 1, "merged"),
            pr_row("o/r", 72, "merged"),
        ],
        "a row first named before the re-creation moves; one named after and other repositories stay"
    );
    assert_eq!(
        pr_rows(&store, new),
        [pr_row("Ivan091/headwork", 7, "open")],
        "a task of the new repository keeps its row"
    );
    assert_eq!(
        pr_rows(&store, quiet),
        [pr_row("Ivan091/headwork-archive", 40, "merged")],
        "a row no event names moves when its task was queued before the re-creation"
    );
    let moved = store
        .events(EventFilter {
            task: None,
            limit: 100,
        })
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "pr-moved")
        .count();
    assert_eq!(moved, 2, "each move is logged");
    let reused = new_pr("Ivan091/headwork", 71);
    store
        .record_report(new, "s", "review", "reused number", &[], Some(&reused))
        .unwrap();
    assert_eq!(
        pr_rows(&store, new).len(),
        2,
        "a new pull request with an archived number gets a row of its own"
    );
}

#[test]
fn task_flags_default_set_and_log() {
    use headwork_core::review::Level;
    use headwork_core::{CloseMode, DecidedBy};
    let (mut store, config) = store();
    let mut new = request("me-sim", "flags", "scene");
    new.request.review = Some(Level::Standard);
    let task = store.create_task(&config, &new, Actor::User).unwrap();
    assert_eq!(
        (task.close, task.review_plan),
        (CloseMode::Confirm, Some(Level::Standard))
    );
    store
        .set_task_close(task.id, CloseMode::Silent, DecidedBy::User, Actor::User)
        .unwrap();
    store
        .set_review_plan(task.id, None, DecidedBy::User, Actor::User)
        .unwrap();
    let task = store.task(task.id).unwrap();
    assert_eq!((task.close, task.review_plan), (CloseMode::Silent, None));
    let logged = store
        .events(EventFilter {
            task: Some(task.id),
            limit: 20,
        })
        .unwrap();
    assert_eq!(
        logged
            .iter()
            .filter(|event| event.kind == "flags-set")
            .count(),
        2
    );
}

#[test]
fn migration_22_makes_kind_silent_tasks_silent() {
    use headwork_core::CloseMode;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    old_store(&path, 21);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "INSERT INTO tasks (id, project, slug, kind, state, model, brief_path, queued_at, updated_at)
                 VALUES (1, 'me-sim', 'quiet', 'silent', 'working', 'opus', 'b.md', 't', 't'),
                        (2, 'me-sim', 'loud', 'scene', 'working', 'opus', 'b.md', 't', 't');",
        )
        .unwrap();
    let store = Store::open(&path).unwrap();
    let quiet = store.task(TaskId(1)).unwrap();
    let loud = store.task(TaskId(2)).unwrap();
    assert_eq!((quiet.close, quiet.review_plan), (CloseMode::Silent, None));
    assert_eq!((loud.close, loud.review_plan), (CloseMode::Confirm, None));
}

#[test]
fn failed_worktree_removals_are_counted_and_forgotten() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    assert_eq!(
        store.removal_attempts(task.id).unwrap(),
        headwork_store::RemovalAttempts::default(),
        "no worktree, no attempts"
    );
    store
        .record_worktree(task.id, "S:/w/sand", "sand", Actor::User)
        .unwrap();
    assert_eq!(
        store.removal_attempts(task.id).unwrap().failures,
        0,
        "none yet"
    );
    store
        .record_removal_failure(task.id, "locked", Actor::User)
        .unwrap();
    store
        .record_removal_failure(task.id, "Permission denied", Actor::User)
        .unwrap();
    let attempts = store.removal_attempts(task.id).unwrap();
    assert_eq!(attempts.failures, 2, "counted");
    assert_eq!(
        attempts.last_error.as_deref(),
        Some("Permission denied"),
        "last error"
    );
    assert!(
        attempts.since_last_s.is_some_and(|s| s < 5),
        "just now: {attempts:?}"
    );
    store.clear_removal_attempts(task.id).unwrap();
    assert_eq!(
        store.removal_attempts(task.id).unwrap().failures,
        0,
        "cleared"
    );
    store
        .record_removal_failure(task.id, "again", Actor::User)
        .unwrap();
    store
        .record_worktree(task.id, "S:/w/sand", "sand", Actor::User)
        .unwrap();
    assert_eq!(
        store.removal_attempts(task.id).unwrap().failures,
        0,
        "a fresh record starts over"
    );
}

#[test]
fn a_refusal_is_logged_and_counted_on_the_newest_worktree_only() {
    let (mut store, config) = store();
    let task = create(&mut store, &config, "me-sim", "sand");
    store
        .record_worktree(task.id, "S:/w/old", "sand", Actor::User)
        .unwrap();
    store
        .record_worktree(task.id, "S:/w/sand", "sand", Actor::User)
        .unwrap();
    store
        .record_removal_failure(task.id, "locked", Actor::User)
        .unwrap();
    let logged = store
        .events(EventFilter {
            task: Some(task.id),
            limit: 20,
        })
        .unwrap()
        .iter()
        .any(|event| event.kind == "worktree-refused");
    assert!(logged, "the refusal is an event of the same call");
    assert_eq!(
        store.removal_attempts(task.id).unwrap().failures,
        1,
        "counted"
    );
    store
        .set_worktree_state(task.id, "kept", Actor::User)
        .unwrap();
    assert_eq!(
        store.removal_attempts(task.id).unwrap().failures,
        0,
        "a kept worktree forgets its failures"
    );
}

#[test]
fn a_dropped_dependency_is_gone_and_its_refusal_is_logged_once() {
    let (mut store, config) = store();
    let engine = create(&mut store, &config, "me-sim", "engine");
    let mut scene = request("me-sim", "scene", "scene");
    scene.depends_on = Some(engine.id);
    let scene = store
        .create_task(&config, &scene, Actor::Orchestrator)
        .unwrap();
    assert_eq!(
        store.gone_dependency(&scene).unwrap(),
        None,
        "a live dependency is not gone"
    );
    store.drop_task(engine.id, Actor::Orchestrator).unwrap();
    for _ in 0..3 {
        let refused = store
            .start_task(&config, scene.id, Actor::Tick)
            .unwrap_err();
        assert!(
            matches!(
                refused,
                StoreError::Refused {
                    refusal: Refusal::DependencyGone,
                    ..
                }
            ),
            "{refused}"
        );
    }
    let gone = store.gone_dependency(&scene).unwrap().unwrap();
    assert_eq!(gone.id, engine.id, "the gone dependency");
    let refusals = store
        .events(EventFilter {
            task: Some(scene.id),
            limit: 100,
        })
        .unwrap()
        .iter()
        .filter(|event| event.kind == "refused")
        .count();
    assert_eq!(refusals, 1, "logged on change only");
}

fn live_session(store: &mut Store, config: &Config, slug: &str, number: u32) -> (Task, String) {
    let task = create(store, config, "me-sim", slug);
    store.start_task(config, task.id, Actor::Tick).unwrap();
    let session = NewSession {
        id: format!("0b6c1d2e-0000-4000-8000-0000000000{number:02}"),
        task_id: Some(task.id),
        role: Role::Worker,
        rc_name: format!("me-sim-{slug}"),
        host: "wmux".to_owned(),
        host_workspace: Some("ws-1".to_owned()),
        host_surface: Some(format!("surface:{number}")),
    };
    store.record_session(&session, Actor::Tick).unwrap();
    store
        .set_session_state(&session.id, SessionState::Live, Actor::Hook)
        .unwrap();
    store
        .transition(task.id, &Trigger::SessionStarted(LIVE), Actor::Hook)
        .unwrap();
    (task, session.id)
}

fn workers(store: &Store) -> (u32, u32) {
    let board = store.board(false).unwrap();
    (board.live_workers, board.waiting_workers)
}

#[test]
fn an_idle_session_in_review_or_merged_frees_its_slot() {
    let (mut store, config) = store();
    let (review, review_session) = live_session(&mut store, &config, "sand", 11);
    let (merged, merged_session) = live_session(&mut store, &config, "snow", 12);
    for task in [&review, &merged] {
        store
            .transition(
                task.id,
                &Trigger::ReviewOpened { repo_allowed: true },
                Actor::Hook,
            )
            .unwrap();
    }
    store
        .transition(
            merged.id,
            &Trigger::Merged {
                source: MergeSource::GitHost,
                policy: MergePolicy::Deliver,
            },
            Actor::Tick,
        )
        .unwrap();
    assert_eq!(
        store.task(review.id).unwrap().state,
        TaskState::InReview,
        "review"
    );
    assert_eq!(
        store.task(merged.id).unwrap().state,
        TaskState::Merged,
        "merged"
    );
    assert_eq!(workers(&store), (2, 0), "both work as far as the slots go");
    assert!(
        store
            .set_session_idle(&review_session, true, Actor::Tick)
            .unwrap(),
        "set"
    );
    assert_eq!(workers(&store), (1, 1), "the review session waits");
    assert!(
        store
            .set_session_idle(&merged_session, true, Actor::Tick)
            .unwrap(),
        "set"
    );
    assert_eq!(workers(&store), (0, 2), "both wait");
    assert!(
        !store
            .set_session_idle(&merged_session, true, Actor::Tick)
            .unwrap(),
        "idempotent"
    );
    assert!(
        store
            .set_session_idle(&merged_session, false, Actor::Hook)
            .unwrap(),
        "cleared"
    );
    assert!(
        !store
            .set_session_idle(&merged_session, false, Actor::Hook)
            .unwrap(),
        "idempotent"
    );
    assert_eq!(workers(&store), (1, 1), "the merged session works again");
    let logged = store
        .events(EventFilter {
            task: None,
            limit: 1000,
        })
        .unwrap()
        .iter()
        .filter(|event| event.kind == headwork_store::EVENT_SESSION_IDLE)
        .count();
    assert_eq!(logged, 3, "two sets and one clear");
}

#[test]
fn an_idle_mark_on_a_working_task_frees_nothing() {
    let (mut store, config) = store();
    let (task, session) = live_session(&mut store, &config, "sand", 13);
    assert_eq!(
        store.task(task.id).unwrap().state,
        TaskState::Working,
        "working"
    );
    store.set_session_idle(&session, true, Actor::Tick).unwrap();
    assert_eq!(workers(&store), (1, 0), "still holds its slot");
}

#[test]
fn without_max_parts_or_with_zero_a_lead_runs_any_number_of_parts() {
    for leads in [
        "",
        "max_parts = 0
",
    ] {
        let mut store = Store::open_in_memory().unwrap();
        let config = config_with_leads(leads);
        store.sync_projects(&config).unwrap();
        let (_, parts) = lead_with_parts(&mut store, &config, &["a", "b", "c", "d"]);
        for part in &parts {
            store.start_task(&config, part.id, Actor::Tick).unwrap();
        }
    }
}

#[test]
fn two_leads_share_the_project_slots_without_a_part_limit() {
    let mut store = Store::open_in_memory().unwrap();
    let config = config_with_leads("");
    store.sync_projects(&config).unwrap();
    let first = lead_in(&mut store, &config, "work-service", "one");
    let second = lead_in(&mut store, &config, "work-service", "two");
    let a = part_in(&mut store, &config, "work-service", &first, "one-a");
    let b = part_in(&mut store, &config, "work-service", &second, "two-a");
    store.start_task(&config, a.id, Actor::Tick).unwrap();
    let error = store.start_task(&config, b.id, Actor::Tick).unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::Refused {
                refusal: Refusal::ProjectCapReached { cap: 3 },
                ..
            }
        ),
        "the other lead's part waits for the project's slots: {error}"
    );
}

#[test]
fn the_backlog_migration_renames_the_tables_and_the_event_types_and_keeps_the_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    old_store(&path, 26);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO todos (id, project, text, state, owner, task_id, position, created_at, updated_at)
             VALUES (1, 'demo', 'Keep me', 'open', 'user', 7, 1, 't', 't');
         INSERT INTO todo_files (path, imported_at, export_hash, exported_at)
             VALUES ('S:/x/todo.md', 't', 'abc', '2026-11-26T10:00:00.000Z');
         INSERT INTO todo_files (path, imported_at) VALUES ('S:/x/never.md', 't');
         INSERT INTO events (at, actor, type, data) VALUES ('t', 'user', 'todo-added', '{}');
         INSERT INTO events (at, actor, type, data) VALUES ('t', 'user', 'todo-export-overwrote', '{}');
         INSERT INTO events (at, actor, type, data) VALUES ('t', 'user', 'todo-export-failed', '{}');
         INSERT INTO events (at, actor, type, data) VALUES ('t', 'user', 'tick-error', '{}')",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.schema_version().unwrap(),
        i64::try_from(MIGRATIONS.len()).unwrap()
    );
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let (text, task, status): (String, Option<i64>, String) = conn
        .query_row(
            "SELECT text, task_id, status FROM backlog_items WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (text.as_str(), task, status.as_str()),
        ("Keep me", None, "backlog"),
        "the item stays; 0031 drops a link to a task the store never had"
    );
    let mut files = conn
        .prepare("SELECT path, export_hash, configured_at FROM backlog_files ORDER BY path")
        .unwrap();
    let files: Vec<(String, Option<String>, Option<String>)> = files
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        files,
        [
            ("S:/x/never.md".to_owned(), None, None),
            (
                "S:/x/todo.md".to_owned(),
                Some("abc".to_owned()),
                Some("2026-11-26T10:00:00.000Z".to_owned())
            ),
        ],
        "an exported file counts as the configured one it was"
    );
    let mut statement = conn.prepare("SELECT type FROM events ORDER BY id").unwrap();
    let kinds: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        kinds,
        [
            "backlog-added",
            "backlog-export-overwrote",
            "backlog-export-failed",
            "tick-error"
        ]
    );
    let old_tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('todos', 'todo_files', 'todos_by_task')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_tables, 0);
    let index: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'backlog_items_by_task'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index, 1);
}

#[test]
fn the_status_migration_maps_each_old_state_and_keeps_ids_and_links() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    old_store(&path, 30);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "INSERT INTO tasks (id, project, slug, kind, state, model, brief_path, queued_at, updated_at)
             VALUES (1, 'me-sim', 'live', 'scene', 'working', 'opus', 'b.md', 't', 't'),
                    (2, 'me-sim', 'over', 'scene', 'closed', 'opus', 'b.md', 't', 't'),
                    (3, 'me-sim', 'gone', 'scene', 'dropped', 'opus', 'b.md', 't', 't'),
                    (4, 'me-sim', 'broke', 'scene', 'failed', 'opus', 'b.md', 't', 't'),
                    (5, 'me-sim', 'wait', 'scene', 'queued', 'opus', 'b.md', 't', 't');
         INSERT INTO backlog_items (id, project, text, state, owner, task_id, position, done_on, done_note, created_at, updated_at)
             VALUES (10, 'me-sim', 'open alone', 'open', 'agent', NULL, 1, NULL, NULL, 't', 't'),
                    (11, 'me-sim', 'open working', 'open', 'agent', 1, 2, NULL, NULL, 't', 't'),
                    (12, 'me-sim', 'open closed', 'open', 'agent', 2, 3, NULL, NULL, 't', 't'),
                    (13, 'me-sim', 'open dropped', 'open', 'agent', 3, 4, NULL, NULL, 't', 't'),
                    (14, 'me-sim', 'open failed', 'open', 'agent', 4, 5, NULL, NULL, 't', 't'),
                    (15, 'me-sim', 'open queued', 'open', 'user', 5, 6, NULL, NULL, 't', 't'),
                    (16, 'me-sim', 'was done', 'done', 'agent', NULL, 7, '2026-11-20', 'PR #1', 't', 't'),
                    (17, 'me-sim', 'was dropped', 'dropped', 'agent', NULL, 8, '2026-11-21', 'no', 't', 't');
         INSERT INTO events (at, actor, type, data) VALUES ('t', 'user', 'backlog-dropped', '{\"id\": 17}');",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path).unwrap();
    let items = store
        .backlog_items(&headwork_store::BacklogFilter::default())
        .unwrap();
    let found: Vec<(i64, &str, Option<i64>)> = items
        .iter()
        .map(|item| {
            (
                item.id,
                item.draft.status.kind().as_str(),
                item.task.as_ref().map(|task| task.id),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (10, "backlog", None),
            (11, "in_progress", Some(1)),
            (12, "backlog", Some(2)),
            (13, "backlog", Some(3)),
            (14, "backlog", Some(4)),
            (15, "in_progress", Some(5)),
            (16, "done", None),
            (17, "cancelled", None),
        ]
    );
    let done = items.iter().find(|item| item.id == 16).unwrap();
    assert_eq!(
        (
            done.draft
                .done_on
                .as_ref()
                .map(headwork_core::backlog::BacklogDate::as_str),
            done.draft.done_note.as_deref()
        ),
        (Some("2026-11-20"), Some("PR #1")),
        "the other columns come along"
    );
    assert_eq!(
        items[5].draft.owner,
        headwork_core::backlog::BacklogOwner::User
    );
    let kinds: Vec<String> = store
        .events(EventFilter {
            task: None,
            limit: 50,
        })
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert!(
        kinds.contains(&"backlog-dropped".to_owned()),
        "old events stay as written: {kinds:?}"
    );
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let index: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'backlog_items_by_task'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index, 1, "the index is made again");
    let refused = conn.execute("UPDATE backlog_items SET status = 'open' WHERE id = 10", []);
    assert!(refused.is_err(), "the old word is refused by the CHECK");
}
