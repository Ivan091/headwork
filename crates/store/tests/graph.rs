//! The agent graph's facts: the subagents table, every row and event the graph reads, and the read-only open.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::graph::{EdgeKind, NodeKind, Scope, build};
use headwork_core::lifecycle::SessionFacts;
use headwork_core::usage::{AgentMeta, Tokens, UsageLine};
use headwork_core::{
    Actor, AskKind, Config, PermissionMode, ProjectName, Role, Slug, TaskRequest, TaskShape,
    Trigger,
};
use headwork_store::{
    FileMark, NewAgent, NewAsk, NewPr, NewSession, NewTask, Store, StoreError, Task, TaskId,
    UsageBatch, UsageTag,
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

fn task(store: &mut Store, config: &Config, slug: &str, shape: TaskShape<TaskId>) -> Task {
    let new = NewTask {
        request: TaskRequest {
            project: ProjectName::new("me-sim").unwrap(),
            slug: Slug::new(slug).unwrap(),
            kind: "scene".to_owned(),
            model: None,
            effort: None,
            in_main: false,
            keep: false,
        },
        brief_path: format!("briefs/{slug}.md"),
        depends_on: None,
        permission_mode: PermissionMode::Auto,
        shape,
    };
    store
        .create_task(config, &new, Actor::Orchestrator)
        .unwrap()
}

fn session(store: &mut Store, id: &str, task: Option<TaskId>, role: Role) {
    store
        .record_session(
            &NewSession {
                id: id.to_owned(),
                task_id: task,
                role,
                rc_name: id.to_owned(),
                host: "wmux".to_owned(),
                host_workspace: None,
                host_surface: None,
            },
            Actor::User,
        )
        .unwrap();
}

fn line(id: &str, at: &str, model: &str, input: u64, output: u64) -> UsageLine {
    UsageLine {
        message_id: id.to_owned(),
        at: at.to_owned(),
        model: model.to_owned(),
        effort: Some("medium".to_owned()),
        speed: None,
        service_tier: None,
        cwd: Some("S:/Code/x".to_owned()),
        tokens: Tokens {
            input,
            cache_read: 10,
            output,
            ..Tokens::default()
        },
    }
}

fn record(store: &mut Store, session: &str, agent: Option<&str>, lines: &[UsageLine]) -> usize {
    let tag = store
        .usage_tag(session)
        .unwrap()
        .unwrap_or(UsageTag::UNMANAGED);
    store
        .record_usage(&UsageBatch {
            path: &format!("{session}-{agent:?}.jsonl"),
            session_id: session,
            agent_id: agent,
            agent_type: agent.map(|_| "Explore"),
            tag,
            dir: "S:/Code/x",
            lines,
            mark: FileMark {
                read_to: 100,
                size: 100,
                mtime_ms: 1,
            },
        })
        .unwrap()
}

fn meta(description: &str) -> AgentMeta {
    AgentMeta {
        agent_type: Some("Explore".to_owned()),
        description: Some(description.to_owned()),
        model: Some("haiku".to_owned()),
        spawn_depth: Some(1),
    }
}

#[test]
fn an_agent_keeps_its_earliest_and_latest_response() {
    let (mut store, config) = store();
    let sand = task(&mut store, &config, "sand", TaskShape::Plain);
    session(&mut store, "s1", Some(sand.id), Role::Worker);
    assert!(!store.agent_known("a1").unwrap(), "unknown at first");
    record(
        &mut store,
        "s1",
        Some("a1"),
        &[line(
            "m1",
            "2026-11-22T10:00:00.000Z",
            "claude-haiku-5-5",
            1,
            1,
        )],
    );
    let first = meta("find the hook");
    store
        .record_agent(&NewAgent {
            agent_id: "a1",
            session_id: "s1",
            task: Some(sand.id),
            meta: &first,
        })
        .unwrap();
    record(
        &mut store,
        "s1",
        Some("a1"),
        &[line(
            "m2",
            "2026-11-22T12:00:00.000Z",
            "claude-haiku-5-5",
            1,
            1,
        )],
    );
    let empty = AgentMeta::default();
    store
        .record_agent(&NewAgent {
            agent_id: "a1",
            session_id: "s1",
            task: None,
            meta: &empty,
        })
        .unwrap();
    assert!(store.agent_known("a1").unwrap(), "known now");
    let facts = store.graph_facts().unwrap();
    let agent = &facts.agents[0];
    assert_eq!(
        (
            agent.first_at.as_deref(),
            agent.last_at.as_deref(),
            agent.description.as_deref(),
            agent.task,
        ),
        (
            Some("2026-11-22T10:00:00.000Z"),
            Some("2026-11-22T12:00:00.000Z"),
            Some("find the hook"),
            Some(sand.id.0),
        ),
        "times widen, an empty meta keeps what was known"
    );
}

/// A lead with a part, the orchestrator, a message, a report with a pull request, an ask and a subagent.
fn populate(store: &mut Store, config: &Config) -> (Task, Task) {
    let lead = task(store, config, "big", TaskShape::Lead);
    store.start_task(config, lead.id, Actor::Tick).unwrap();
    store
        .transition(
            lead.id,
            &Trigger::SessionStarted(SessionFacts {
                session_matches: true,
                launcher_alive: true,
            }),
            Actor::Hook,
        )
        .unwrap();
    let part = task(
        store,
        config,
        "big-review",
        TaskShape::Part { parent: lead.id },
    );
    session(store, "orch", None, Role::Orchestrator);
    session(store, "lead", Some(lead.id), Role::Worker);
    session(store, "part", Some(part.id), Role::Worker);
    store
        .append_event(
            Actor::Hook,
            Some(part.id),
            Some("part"),
            "tool-used",
            &serde_json::json!({ "tool": "SendMessage", "to": "lead", "summary": "review ready" }),
        )
        .unwrap();
    store
        .append_event(
            Actor::Hook,
            Some(part.id),
            Some("part"),
            "tool-used",
            &serde_json::json!({ "tool": "SendMessage" }),
        )
        .unwrap();
    let pr = NewPr {
        repo: "o/r".to_owned(),
        number: 41,
        url: "https://github.com/o/r/pull/41".to_owned(),
    };
    store
        .record_report(lead.id, "lead", "review", "in review", &[], Some(&pr))
        .unwrap();
    store
        .open_ask(
            &NewAsk {
                task_id: Some(lead.id),
                session_id: Some("lead".to_owned()),
                kind: AskKind::Question,
                question: "Which scope should the dashboard take?".to_owned(),
                options: vec!["a".to_owned(), "b".to_owned()],
            },
            Actor::Hook,
        )
        .unwrap();
    record(
        store,
        "lead",
        Some("a1"),
        &[line(
            "m1",
            "2026-11-22T10:00:00.000Z",
            "claude-haiku-5-5",
            1,
            1,
        )],
    );
    store
        .record_agent(&NewAgent {
            agent_id: "a1",
            session_id: "lead",
            task: Some(lead.id),
            meta: &meta("look"),
        })
        .unwrap();
    (lead, part)
}

#[test]
fn the_facts_draw_a_lead_its_part_and_their_talk() {
    let (mut store, config) = store();
    let (lead, part) = populate(&mut store, &config);

    let facts = store.graph_facts().unwrap();
    assert_eq!(
        facts.messages.len(),
        1,
        "a SendMessage without `to` draws nothing"
    );
    assert!(!facts.transitions.is_empty(), "transitions read");
    let graph = build(&facts, None, Scope::Live);
    let lead_box = format!("task:{}", lead.id.0);
    let part_box = format!("task:{}", part.id.0);
    let table = [
        ("session:lead", NodeKind::Lead, Some(lead_box.as_str())),
        ("session:part", NodeKind::Part, Some(part_box.as_str())),
        (part_box.as_str(), NodeKind::Task, Some(lead_box.as_str())),
        ("agent:a1", NodeKind::Subagent, Some(lead_box.as_str())),
    ];
    for (id, kind, parent) in table {
        let node = graph.node(id).unwrap();
        assert_eq!((node.kind, node.parent.as_deref()), (kind, parent), "{id}");
    }
    let edges = [
        (EdgeKind::Message, "session:part", "session:lead"),
        (EdgeKind::Report, "session:lead", "orchestrator"),
        (EdgeKind::Called, "session:lead", "agent:a1"),
        (EdgeKind::Spawned, "orchestrator", lead_box.as_str()),
    ];
    for (kind, source, target) in edges {
        assert!(
            graph
                .edges_of(kind)
                .any(|edge| edge.source == source && edge.target == target),
            "{kind:?} {source} -> {target}"
        );
    }
    let ask = graph
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Ask)
        .unwrap();
    assert_eq!(
        ask.label, "Which scope should the dashboard take?",
        "an ask without a header shows its question"
    );
    let pr_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Pr)
        .unwrap();
    assert_eq!(pr_node.label, "#41", "the pull request");
}

#[test]
fn a_read_only_open_reads_and_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    drop(Store::open(&path).unwrap());
    let store = Store::open_read_only(&path).unwrap();
    assert!(store.graph_facts().unwrap().tasks.is_empty(), "reads");
    assert!(
        store
            .append_event(Actor::User, None, None, "probe", &serde_json::json!({}))
            .is_err(),
        "writes are refused"
    );
}

#[test]
fn a_read_only_open_refuses_an_older_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    drop(conn);
    assert!(
        matches!(
            Store::open_read_only(&path),
            Err(StoreError::SchemaBehind { found: 3, .. })
        ),
        "older schema"
    );
}
