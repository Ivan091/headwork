//! The agent graph's facts: the subagents table and its seeding migration, and every row and event the graph reads.

#![expect(
    clippy::unwrap_used,
    reason = "tests fail loudly on an unexpected error"
)]

use std::path::Path;

use headwork_core::graph::{EdgeKind, NodeKind, Scope, build};
use headwork_core::lifecycle::SessionFacts;
use headwork_core::usage::{AgentMeta, Tokens, UsageLine};
use headwork_core::{
    Actor, AskKind, Config, JobState, PermissionMode, ProjectName, PullState, ReviewState, Role,
    Slug, TaskRequest, TaskShape, Trigger,
};
use headwork_store::{
    FileMark, MIGRATIONS, NewAsk, NewPr, NewSession, NewTask, Store, StoreError, Task, TaskId,
    UsageBatch, UsageTag, migrate,
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

fn record(
    store: &mut Store,
    session: &str,
    agent: Option<&str>,
    meta: Option<&AgentMeta>,
    lines: &[UsageLine],
) -> usize {
    let tag = store
        .usage_tag(session)
        .unwrap()
        .unwrap_or(UsageTag::UNMANAGED);
    store
        .record_usage(&UsageBatch {
            path: &format!("{session}-{agent:?}.jsonl"),
            session_id: session,
            agent_id: agent,
            agent_meta: meta,
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
        parent_agent_id: None,
    }
}

const HAIKU: &str = "claude-haiku-5-5";

#[test]
fn an_agent_keeps_its_earliest_and_latest_response_and_what_its_meta_said() {
    let (mut store, config) = store();
    let sand = task(&mut store, &config, "sand", TaskShape::Plain);
    session(&mut store, "s1", Some(sand.id), Role::Worker);
    let first = meta("find the hook");
    record(
        &mut store,
        "s1",
        Some("a1"),
        Some(&first),
        &[line("m1", "2026-11-22T10:00:00.000Z", HAIKU, 1, 1)],
    );
    record(
        &mut store,
        "s1",
        Some("a1"),
        None,
        &[line("m2", "2026-11-22T12:00:00.000Z", HAIKU, 1, 1)],
    );
    record(
        &mut store,
        "s1",
        Some("a1"),
        None,
        &[line("m0", "2026-11-22T09:00:00.000Z", HAIKU, 1, 1)],
    );
    record(&mut store, "s1", Some("a1"), None, &[]);
    let facts = store.graph_facts().unwrap();
    assert_eq!(facts.agents.len(), 1, "one agent");
    let agent = &facts.agents[0];
    assert_eq!(
        (
            agent.first_at.as_deref(),
            agent.last_at.as_deref(),
            agent.description.as_deref(),
            agent.session.as_str()
        ),
        (
            Some("2026-11-22T09:00:00.000Z"),
            Some("2026-11-22T12:00:00.000Z"),
            Some("find the hook"),
            "s1"
        ),
        "times widen both ways, a batch without meta keeps what was known"
    );
}

#[test]
fn an_agent_with_no_responses_is_recorded_without_times() {
    let (mut store, _config) = store();
    record(&mut store, "s9", Some("a9"), Some(&meta("quiet")), &[]);
    let facts = store.graph_facts().unwrap();
    assert_eq!(
        (
            facts.agents[0].first_at.as_deref(),
            facts.agents[0].last_at.as_deref()
        ),
        (None, None),
        "no times"
    );
}

#[test]
fn an_agent_id_that_is_not_a_plain_word_is_not_recorded() {
    let (mut store, _config) = store();
    record(
        &mut store,
        "s1",
        Some("a/../b"),
        Some(&meta("odd")),
        &[line("m1", "2026-11-22T10:00:00.000Z", HAIKU, 1, 1)],
    );
    assert!(
        store.graph_facts().unwrap().agents.is_empty(),
        "left out of the graph"
    );
}

#[test]
fn migration_0020_seeds_agents_from_the_usage_rows_already_read() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let before = MIGRATIONS.len() - 1;
    for (number, sql) in MIGRATIONS.iter().take(before).enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.pragma_update(None, "user_version", i64::try_from(number + 1).unwrap())
            .unwrap();
    }
    for (id, agent, at) in [
        ("m1", Some("a1"), "2026-11-22T10:00:00.000Z"),
        ("m2", Some("a1"), "2026-11-22T11:00:00.000Z"),
        ("m3", None, "2026-11-22T12:00:00.000Z"),
        ("m4", Some("x y"), "2026-11-22T12:00:00.000Z"),
    ] {
        conn.execute(
            "INSERT INTO usage (message_id, session_id, agent_id, agent_type, role, dir, at, model, input, cache_write_5m,
                 cache_write_1h, cache_read, output, thinking, web_searches, web_fetches)
             VALUES (?1, 's1', ?2, 'Explore', 'unmanaged', 'd', ?3, 'm', 0, 0, 0, 0, 0, 0, 0, 0)",
            rusqlite::params![id, agent, at],
        )
        .unwrap();
    }
    migrate(&mut conn).unwrap();
    let rows: Vec<(String, String, String, String)> = conn
        .prepare("SELECT agent_id, agent_type, first_at, last_at FROM agents")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [(
            "a1".to_owned(),
            "Explore".to_owned(),
            "2026-11-22T10:00:00.000Z".to_owned(),
            "2026-11-22T11:00:00.000Z".to_owned()
        )],
        "one agent with its times; plain sessions and odd ids left out"
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
    for data in [
        serde_json::json!({ "tool": "SendMessage" }),
        serde_json::json!({ "tool": "SendMessage", "to": "" }),
    ] {
        store
            .append_event(Actor::Hook, Some(part.id), Some("part"), "tool-used", &data)
            .unwrap();
    }
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
        Some(&meta("look")),
        &[line("m1", "2026-11-22T10:00:00.000Z", HAIKU, 1, 1)],
    );
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
        "a SendMessage without a `to` draws nothing"
    );
    assert!(!facts.transitions.is_empty(), "transitions read");
    assert_eq!(
        facts.tasks[0].created_by,
        Actor::Orchestrator,
        "who created it"
    );
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
fn a_superseded_ask_closes_when_it_was_superseded() {
    let (mut store, config) = store();
    let (lead, _) = populate(&mut store, &config);
    store.supersede_session_asks("lead", Actor::Hook).unwrap();
    let facts = store.graph_facts().unwrap();
    let ask = facts
        .asks
        .iter()
        .find(|ask| ask.task == Some(lead.id.0))
        .unwrap();
    assert!(!ask.state.is_waiting(), "superseded");
    let closed = ask.closed_at.as_deref().unwrap();
    assert!(
        closed >= ask.created_at.as_str(),
        "closed at the supersede event, not before it was made"
    );
    assert_eq!(ask.header, None, "no header");
}

#[test]
fn a_pull_request_starts_at_the_first_event_naming_its_whole_url() {
    let (mut store, config) = store();
    let (lead, _) = populate(&mut store, &config);
    let other = NewPr {
        repo: "o/r".to_owned(),
        number: 4,
        url: "https://github.com/o/r/pull/4".to_owned(),
    };
    std::thread::sleep(std::time::Duration::from_millis(20));
    store
        .record_report(lead.id, "lead", "review", "second", &[], Some(&other))
        .unwrap();
    let facts = store.graph_facts().unwrap();
    let first = |number: i64| {
        facts
            .prs
            .iter()
            .find(|pr| pr.number == number)
            .unwrap()
            .first_at
            .clone()
    };
    assert!(
        first(4) > first(41),
        "#4 starts at its own event, not at the earlier one naming #41"
    );
}

#[test]
fn an_unknown_stored_word_names_its_row() {
    let (mut store, config) = store();
    let (lead, _) = populate(&mut store, &config);
    store
        .append_event(
            Actor::Hook,
            Some(lead.id),
            None,
            "transition",
            &serde_json::json!({ "from": "working", "to": "sleeping" }),
        )
        .unwrap();
    let error = store.graph_facts().unwrap_err();
    assert!(
        matches!(&error, StoreError::BadRow { row, .. } if row.starts_with("event ")),
        "names the event: {error}"
    );
}

#[test]
fn a_nested_agent_keeps_its_parent() {
    let (mut store, _config) = store();
    let mut nested = meta("inner");
    nested.spawn_depth = Some(2);
    nested.parent_agent_id = Some("a1".to_owned());
    record(
        &mut store,
        "s1",
        Some("a2"),
        Some(&nested),
        &[line("m1", "2026-11-22T10:00:00.000Z", HAIKU, 1, 1)],
    );
    record(&mut store, "s1", Some("a2"), Some(&meta("again")), &[]);
    let facts = store.graph_facts().unwrap();
    assert_eq!(
        facts.agents[0].parent.as_deref(),
        Some("a1"),
        "kept when a later meta names none"
    );
}

#[test]
fn logged_state_changes_are_read_per_row() {
    let (mut store, config) = store();
    let (lead, _) = populate(&mut store, &config);
    let url = "https://github.com/o/r/pull/41";
    let events = [
        (
            "pr-state",
            serde_json::json!({ "url": url, "from": "open", "to": "merged" }),
        ),
        (
            "pr-state",
            serde_json::json!({ "url": "https://github.com/o/r/pull/999", "from": "open", "to": "merged" }),
        ),
        (
            "job-state",
            serde_json::json!({ "job": 7, "state": "done", "note": null }),
        ),
        (
            "review-started",
            serde_json::json!({ "review": 3, "from": "requested", "to": "running", "round": 1 }),
        ),
        ("review-requested", serde_json::json!({ "review": 3 })),
    ];
    for (kind, data) in events {
        store
            .append_event(Actor::Tick, Some(lead.id), None, kind, &data)
            .unwrap();
    }
    let facts = store.graph_facts().unwrap();
    let pr = facts.prs.iter().find(|pr| pr.url == url).unwrap();
    assert_eq!(
        facts.pr_changes.len(),
        1,
        "only the task's own pull request"
    );
    assert_eq!(
        (facts.pr_changes[0].key, facts.pr_changes[0].to),
        (pr.id, PullState::Merged),
        "keyed by its row"
    );
    let job = &facts.job_changes[0];
    assert_eq!(
        (job.key, job.from, job.to),
        (7, None, JobState::Done),
        "job"
    );
    assert_eq!(
        facts.review_changes.len(),
        1,
        "a review event without states is no change"
    );
    assert_eq!(facts.review_changes[0].to, ReviewState::Running, "review");
}
