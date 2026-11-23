//! The agent graph end to end: subagent transcripts read into the `agents` table, and `graph` over a populated store.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork::ops::graph::{GraphQuery, graph, render};
use headwork::ops::usage::ingest_file;
use headwork_core::{
    Actor, Config, PermissionMode, ProjectName, Role, Slug, TaskRequest, TaskShape,
};
use headwork_store::{NewSession, NewTask, Store, TaskId};

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
        agent.parent,
        Some(format!("task:{}", task.0)),
        "in its caller's box"
    );
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
    let text = render(&graph);
    assert!(
        text.contains("  subagent Explore [seen]  agent:x1"),
        "{text}"
    );
    assert!(text.contains("session:0a -called-> agent:x1"), "{text}");
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
        task: Some("me-sim-sand".to_owned()),
        ..before
    };
    let error = graph(&store, &one).unwrap_err().to_string();
    assert!(error.contains("was not queued yet"), "{error}");
    let all = GraphQuery {
        all: true,
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
