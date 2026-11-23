//! The agent graph end to end: subagent transcripts read into the `agents` table, and `graph` over a populated store.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork::ops::graph::{GraphQuery, Which, graph};
use headwork::ops::usage::ingest_file;
use headwork_core::config::ParkPolicy;
use headwork_core::graph::EdgeKind;
use headwork_core::{
    Actor, Config, JobState, PermissionMode, ProjectName, Role, Slug, TaskRequest, TaskShape,
};
use headwork_store::{NewJob, NewPr, NewSession, NewTask, PrSeen, Store, TaskId};

fn config() -> Config {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config.toml");
    Config::parse(&std::fs::read_to_string(path).unwrap(), "config.toml").unwrap()
}

fn store_with_task() -> (Store, TaskId) {
    let config = config();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_projects(&config).unwrap();
    let new = NewTask {
        request: TaskRequest {
            project: ProjectName::new("me-sim").unwrap(),
            slug: Slug::new("sand").unwrap(),
            kind: "scene".to_owned(),
            model: None,
            effort: None,
            in_main: false,
            keep: false,
            close: headwork_core::CloseMode::Confirm,
            review: None,
            decision: None,
        },
        brief_path: "briefs/sand.md".to_owned(),
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        shape: TaskShape::Lead,
    };
    let task = store
        .create_task(&config, &new, Actor::Orchestrator)
        .unwrap();
    store
        .record_session(
            &NewSession {
                id: "0a".to_owned(),
                task_id: Some(task.id),
                role: Role::Worker,
                rc_name: "me-sim-sand".to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
    (store, task.id)
}

fn response(id: &str, at: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "uuid": format!("u{id}"),
        "timestamp": at,
        "message": {
            "id": id,
            "model": "claude-haiku-5-5",
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }
    })
    .to_string()
}

#[test]
fn a_subagent_transcript_and_its_meta_become_a_node_in_its_callers_box() {
    let (mut store, task) = store_with_task();
    let dir = tempfile::tempdir().unwrap();
    let subagents = dir.path().join("projects/S--w/0a/subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    let transcript = subagents.join("agent-x1.jsonl");
    std::fs::write(
        &transcript,
        response("m1", "2026-11-22T10:00:00.000Z") + "\n",
    )
    .unwrap();
    std::fs::write(
        subagents.join("agent-x1.meta.json"),
        r#"{"agentType":"Explore","description":"find the hook","model":"haiku","spawnDepth":1}"#,
    )
    .unwrap();
    assert_eq!(
        ingest_file(&mut store, &transcript, false).unwrap(),
        Some((1, 1)),
        "one response read"
    );
    std::fs::write(
        &transcript,
        response("m1", "2026-11-22T10:00:00.000Z")
            + "\n"
            + &response("m2", "2026-11-22T11:00:00.000Z")
            + "\n",
    )
    .unwrap();
    ingest_file(&mut store, &transcript, false).unwrap();

    let graph = graph(&store, &GraphQuery::default()).unwrap();
    let agent = graph.node("agent:x1").unwrap();
    assert_eq!(
        agent.details.get("description").map(String::as_str),
        Some("find the hook"),
        "the label"
    );
    assert_eq!(
        (agent.start.as_deref(), agent.end.as_deref()),
        (
            Some("2026-11-22T10:00:00.000Z"),
            Some("2026-11-22T11:00:00.000Z")
        ),
        "first and last response"
    );
    assert_eq!(
        agent.state().map(headwork_core::graph::Status::as_str),
        Some("seen"),
        "seen now"
    );
    assert!(
        graph
            .edges_of(EdgeKind::Called)
            .any(|edge| edge.source == format!("task:{}", task.0) && edge.target == "agent:x1"),
        "called by its task"
    );
}

#[test]
fn graph_replays_a_past_instant_and_refuses_a_task_not_yet_queued() {
    let (store, task) = store_with_task();
    let now = graph(&store, &GraphQuery::default()).unwrap();
    assert!(now.node(&format!("task:{}", task.0)).is_some(), "drawn now");
    let before = GraphQuery {
        at: Some("2000-01-01".to_owned()),
        ..GraphQuery::default()
    };
    let past = graph(&store, &before).unwrap();
    assert!(
        past.node(&format!("task:{}", task.0)).is_none(),
        "not queued in 2000"
    );
    assert_eq!(
        past.at.as_deref().and_then(|at| at.get(..10)),
        Some("2000-01-01"),
        "the instant"
    );
    let one = GraphQuery {
        which: Which::Task("me-sim-sand".to_owned()),
        ..before
    };
    let error = graph(&store, &one).unwrap_err().to_string();
    assert!(error.contains("was queued by"), "{error}");
    let future = GraphQuery {
        at: Some("2999-01-01".to_owned()),
        ..GraphQuery::default()
    };
    let error = graph(&store, &future).unwrap_err().to_string();
    assert!(error.contains("in the future"), "{error}");
    let all = GraphQuery {
        which: Which::All,
        ..GraphQuery::default()
    };
    assert!(
        graph(&store, &all)
            .unwrap()
            .node(&format!("task:{}", task.0))
            .is_some(),
        "all draws it"
    );
}

/// The store's clock now, then a pause, so the next write lands strictly later.
fn tick(store: &Store) -> String {
    let now = store.instant_ago(0).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    now
}

fn state_at(store: &Store, at: &str, id: &str) -> Option<String> {
    let query = GraphQuery {
        at: Some(at.to_owned()),
        which: Which::All,
    };
    let graph = graph(store, &query).unwrap();
    graph.node(id).map(|node| {
        node.state()
            .map_or_else(String::new, |state| state.as_str().to_owned())
    })
}

#[test]
fn a_replay_shows_each_instant_as_the_operations_left_it() {
    let before = {
        let store = Store::open_in_memory().unwrap();
        tick(&store)
    };
    let (mut store, task) = store_with_task();
    let queued = tick(&store);
    let pr = NewPr {
        repo: "o/r".to_owned(),
        number: 7,
        url: "https://github.com/o/r/pull/7".to_owned(),
    };
    store
        .record_report(task, "0a", "review", "in review", &[], Some(&pr))
        .unwrap();
    let opened = tick(&store);
    let seen = PrSeen {
        repo: "o/r",
        number: 7,
        url: "https://github.com/o/r/pull/7",
        state: "merged",
    };
    store.record_pr(task, &seen, Actor::Tick).unwrap();
    let merged = tick(&store);
    store.session_ended("0a", Some(0)).unwrap();
    tick(&store);

    let task_node = format!("task:{}", task.0);
    let pr_node = graph(
        &store,
        &GraphQuery {
            which: Which::All,
            ..GraphQuery::default()
        },
    )
    .unwrap()
    .nodes
    .iter()
    .find(|node| node.label == "#7")
    .map(|node| node.id.clone())
    .unwrap();
    let table = [
        (&before, task_node.as_str(), None),
        (&queued, task_node.as_str(), Some("queued")),
        (&queued, pr_node.as_str(), None),
        (&opened, pr_node.as_str(), Some("open")),
        (&merged, pr_node.as_str(), Some("merged")),
    ];
    for (at, id, state) in table {
        assert_eq!(state_at(&store, at, id).as_deref(), state, "{id} at {at}");
    }
}

#[test]
fn a_job_parked_and_started_again_is_running_after_its_restart() {
    let (mut store, task) = store_with_task();
    let argv = ["render".to_owned()];
    let job = store
        .register_job(
            &NewJob {
                task_id: Some(task),
                session_id: Some("0a"),
                project: "me-sim",
                label: "render",
                park_policy: ParkPolicy::Rerun,
                argv: &argv,
                cwd: "S:/w",
                pid: Some(10),
                identity: None,
                checkpoint_path: None,
                heavy: false,
                restart: true,
                log_path: "S:/w/render.log",
            },
            Actor::Worker,
        )
        .unwrap();
    let started = tick(&store);
    store
        .set_job_state(job, JobState::Parked, Some("park"), Actor::Tick)
        .unwrap();
    let parked = tick(&store);
    store.set_job_pid(job, 11, None, Actor::Tick).unwrap();
    let restarted = tick(&store);
    store
        .set_job_state(job, JobState::Done, None, Actor::Tick)
        .unwrap();
    let done = tick(&store);
    let id = format!("job:{job}");
    let table = [
        (&started, "running"),
        (&parked, "parked"),
        (&restarted, "running"),
        (&done, "done"),
    ];
    for (at, state) in table {
        assert_eq!(
            state_at(&store, at, &id).as_deref(),
            Some(state),
            "{id} at {at}"
        );
    }
}

#[test]
fn a_meta_file_missing_at_the_first_read_is_read_once_it_is_there() {
    let (mut store, _task) = store_with_task();
    let dir = tempfile::tempdir().unwrap();
    let subagents = dir.path().join("projects/S--w/0a/subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    let transcript = subagents.join("agent-x2.jsonl");
    std::fs::write(
        &transcript,
        response("m1", "2026-11-22T10:00:00.000Z") + "\n",
    )
    .unwrap();
    ingest_file(&mut store, &transcript, false).unwrap();
    let described = |store: &Store| {
        let facts = store.graph_facts().unwrap();
        let agent = facts
            .agents
            .iter()
            .find(|agent| agent.id == "x2")
            .cloned()
            .unwrap();
        (agent.description, agent.finished)
    };
    assert_eq!(described(&store), (None, None), "no meta yet");
    std::fs::write(
        subagents.join("agent-x2.meta.json"),
        r#"{"description":"late","finished":true}"#,
    )
    .unwrap();
    assert_eq!(
        ingest_file(&mut store, &transcript, false).unwrap(),
        None,
        "the transcript did not change"
    );
    assert_eq!(
        described(&store),
        (Some("late".to_owned()), Some(true)),
        "the meta is read anyway"
    );
    std::fs::write(
        subagents.join("agent-x2.meta.json"),
        r#"{"description":"changed","finished":false}"#,
    )
    .unwrap();
    ingest_file(&mut store, &transcript, false).unwrap();
    assert_eq!(
        described(&store),
        (Some("late".to_owned()), Some(true)),
        "a finished agent's meta is not read again"
    );
}
