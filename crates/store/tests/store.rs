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
    BACKUP_DIR, Backup, EventFilter, FABLE_ALLOW, MIGRATIONS, NewAsk, NewJob, NewRecap, NewSession,
    NewTask, Place, Store, StoreError, Task,
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
                heavy: true,
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
                heavy: true,
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
    shape: headwork_core::TaskShape<headwork_store::TaskId>,
) -> Result<Task, StoreError> {
    let new = NewTask {
        shape,
        ..request(project, slug, "scene")
    };
    store.create_task(config, &new, Actor::Orchestrator)
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
    let (mut store, config) = store();
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
